/*
 * sample-app — a minimal AppLoad app for the reMarkable 2.
 *
 * What it does: a doodle pad. Draw on the canvas with pen or finger,
 * CLEAR wipes it, EXIT quits. That's deliberately boring — the point is
 * that this file demonstrates every mechanism an AppLoad app needs:
 *
 *   1. connect to AppLoad's qtfb server and map the shared framebuffer
 *   2. put pixels into that framebuffer (RGB565)
 *   3. tell the server to refresh all of / part of the e-ink screen
 *   4. receive and dispatch touch + pen input
 *   5. exit cleanly
 *
 * See src/qtfb.h for the protocol itself. There is no event library, no
 * toolkit, no magic: one socket, one mmap, one recv() loop.
 *
 * The app is launched BY AppLoad (tap the icon in the xochitl sidebar
 * menu). Launching it from a shell won't work: the QTFB_KEY env var and
 * the /tmp/qtfb.sock server only exist courtesy of AppLoad inside xochitl.
 * stdout/stderr end up in xochitl's journal: `make log` tails it.
 */

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#include "font5x7.h"
#include "qtfb.h"

/* ---- screen ------------------------------------------------------------ */

#define FB_W RM2_WIDTH  /* 1404 */
#define FB_H RM2_HEIGHT /* 1872 */

/* RGB565. On the monochrome e-ink panel only luminance matters, so shades
 * of gray are all you'll ever see — but the buffer is real color. */
#define WHITE 0xFFFF
#define BLACK 0x0000
#define GRAY 0x8410

/* ---- UI layout (all in framebuffer pixels) ------------------------------ */

#define HEADER_H 140       /* title bar height */
#define FOOTER_H 80        /* hint line at the bottom */
#define BTN_W 260
#define BTN_H 88
#define BTN_Y 26
#define BTN_EXIT_X (FB_W - 24 - BTN_W)
#define BTN_CLEAR_X (BTN_EXIT_X - 24 - BTN_W)

/* the drawable canvas is everything between header and footer */
#define CANVAS_Y0 (HEADER_H + 4)
#define CANVAS_Y1 (FB_H - FOOTER_H)

#define BRUSH_R 4 /* stroke half-width in pixels */

/* ---- globals ------------------------------------------------------------ */

static int sock_fd = -1;
static uint16_t *fb; /* the mmap'ed shared framebuffer: fb[y * FB_W + x] */
static volatile sig_atomic_t running = 1;

static void on_signal(int sig) {
    (void)sig; /* AppLoad may SIGTERM us when the window closes */
    running = 0;
}

/* ---- protocol helpers ---------------------------------------------------- */

static void qtfb_send(const qtfb_client_message *m) {
    if (send(sock_fd, m, sizeof *m, 0) < 0)
        perror("qtfb send");
}

/* Blit the whole framebuffer to the screen. Use sparingly: full-screen
 * e-ink updates are slow and flashy. */
static void update_all(void) {
    qtfb_client_message m = {.type = MESSAGE_UPDATE, .update = {.type = UPDATE_ALL}};
    qtfb_send(&m);
}

/* Blit just a region. This is the workhorse: small regions refresh fast. */
static void update_region(int x, int y, int w, int h) {
    qtfb_client_message m = {
        .type = MESSAGE_UPDATE,
        .update = {.type = UPDATE_PARTIAL, .x = x, .y = y, .w = w, .h = h},
    };
    qtfb_send(&m);
}

/* Ask for a full deghosting flash (the black/white blink e-readers do). */
static void full_refresh(void) {
    qtfb_client_message m = {.type = MESSAGE_REQUEST_FULL_REFRESH};
    qtfb_send(&m);
}

/* Pick the e-ink waveform for subsequent updates. This is THE latency
 * lever: the default (REFRESH_MODE_UI) is a quality waveform that takes
 * hundreds of ms per refresh; UFAST is the fast near-binary waveform the
 * stock notebook uses while inking — much lower latency, more ghosting.
 * The CLEAR button's full_refresh() wipes the accumulated ghosting. */
static void set_refresh_mode(int mode) {
    qtfb_client_message m = {.type = MESSAGE_SET_REFRESH_MODE, .refreshMode = mode};
    qtfb_send(&m);
}

/* ---- update batching ------------------------------------------------------
 * The pen streams move events far faster than e-ink can refresh, and a
 * region that is mid-refresh can't start a new one — sending one update
 * per event just piles up a queue and the ink lags further and further
 * behind the pen. So strokes only *mark* what they touched, and the main
 * loop flushes ONE update covering all of it once the input queue is dry. */

/* At most one e-ink update per FLUSH_MS while inking: refreshes have a
 * fixed per-update cost through AppLoad's Qt pipeline, so 200 tiny ones a
 * second are slower than ~80 slightly larger ones. Tune 10-30 to taste. */
#define FLUSH_MS 12

static long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000LL + ts.tv_nsec / 1000000;
}

static int dirty_x0, dirty_y0, dirty_x1, dirty_y1;
static int dirty = 0;
static long long last_flush = 0;

static void mark_dirty(int x0, int y0, int x1, int y1) {
    if (!dirty) {
        dirty_x0 = x0; dirty_y0 = y0; dirty_x1 = x1; dirty_y1 = y1;
        dirty = 1;
        return;
    }
    if (x0 < dirty_x0) dirty_x0 = x0;
    if (y0 < dirty_y0) dirty_y0 = y0;
    if (x1 > dirty_x1) dirty_x1 = x1;
    if (y1 > dirty_y1) dirty_y1 = y1;
}

static void flush_dirty(void) {
    if (!dirty)
        return;
    update_region(dirty_x0 < 0 ? 0 : dirty_x0, dirty_y0 < 0 ? 0 : dirty_y0,
                  dirty_x1 - dirty_x0 + 1, dirty_y1 - dirty_y0 + 1);
    dirty = 0;
}

/* ---- drawing primitives --------------------------------------------------
 * All clip against the framebuffer, so callers don't have to be careful. */

static void px(int x, int y, uint16_t c) {
    if (x >= 0 && x < FB_W && y >= 0 && y < FB_H)
        fb[y * FB_W + x] = c;
}

static void fill_rect(int x, int y, int w, int h, uint16_t c) {
    for (int j = y; j < y + h; j++)
        for (int i = x; i < x + w; i++)
            px(i, j, c);
}

static void rect_outline(int x, int y, int w, int h, int t, uint16_t c) {
    fill_rect(x, y, w, t, c);             /* top */
    fill_rect(x, y + h - t, w, t, c);     /* bottom */
    fill_rect(x, y, t, h, c);             /* left */
    fill_rect(x + w - t, y, t, h, c);     /* right */
}

static void disc(int cx, int cy, int r, uint16_t c) {
    for (int j = -r; j <= r; j++)
        for (int i = -r; i <= r; i++)
            if (i * i + j * j <= r * r)
                px(cx + i, cy + j, c);
}

/* Draw text with the 5x7 font scaled up `scale` times. Returns the width
 * in pixels; use text_width() to measure without drawing. */
static int draw_text(int x, int y, const char *s, int scale, uint16_t c) {
    int x0 = x;
    for (; *s; s++) {
        const uint8_t *g = font_lookup(*s);
        for (int col = 0; col < 5; col++)
            for (int row = 0; row < 7; row++)
                if ((g[col] >> row) & 1)
                    fill_rect(x + col * scale, y + row * scale, scale, scale, c);
        x += 6 * scale; /* 5 columns + 1 column of spacing */
    }
    return x - x0;
}

static int text_width(const char *s, int scale) {
    return (int)strlen(s) * 6 * scale;
}

static void draw_button(int x, int y, const char *label, int scale) {
    fill_rect(x, y, BTN_W, BTN_H, WHITE);
    rect_outline(x, y, BTN_W, BTN_H, 4, BLACK);
    draw_text(x + (BTN_W - text_width(label, scale)) / 2,
              y + (BTN_H - 7 * scale) / 2, label, scale, BLACK);
}

/* ---- the scene ------------------------------------------------------------ */

static void clear_canvas(void) {
    fill_rect(0, CANVAS_Y0, FB_W, CANVAS_Y1 - CANVAS_Y0, WHITE);
    dirty = 0; /* whatever strokes were pending just got wiped anyway */
    update_all();
    full_refresh(); /* deghost — leftover strokes would shadow otherwise */
}

static void draw_scene(void) {
    fill_rect(0, 0, FB_W, FB_H, WHITE);

    /* header: title + buttons + separator line */
    draw_text(32, (HEADER_H - 7 * 6) / 2, "SAMPLE APP", 6, BLACK);
    draw_button(BTN_CLEAR_X, BTN_Y, "CLEAR", 4);
    draw_button(BTN_EXIT_X, BTN_Y, "EXIT", 4);
    fill_rect(0, HEADER_H, FB_W, 4, BLACK);

    /* footer hint */
    fill_rect(0, FB_H - FOOTER_H - 4, FB_W, 2, GRAY);
    draw_text(32, FB_H - FOOTER_H + 20, "DRAW WITH PEN OR FINGER", 3, GRAY);

    update_all();
}

/* ---- input handling -------------------------------------------------------
 * Strokes: we remember the previous point per input source and stamp a line
 * of brush discs between it and the new point, then refresh just the dirty
 * bounding box of that segment. Touch can have several fingers down at once
 * (devId = finger slot), the pen is its own source — give each a slot. */

#define SLOTS 16
static int slot_active[SLOTS];
static int slot_x[SLOTS], slot_y[SLOTS];

static int slot_for(const qtfb_userinput *in) {
    if (in->inputType >= INPUT_PEN_PRESS && in->inputType <= INPUT_PEN_UPDATE)
        return SLOTS - 1; /* the pen gets its own slot... */
    return in->devId % (SLOTS - 1); /* ...fingers share the rest */
}

static int in_rect(int x, int y, int rx, int ry, int rw, int rh) {
    return x >= rx && x < rx + rw && y >= ry && y < ry + rh;
}

/* ---- direct pen input (state; reader functions further down) ---------------
 * AppLoad forwards input through xochitl's Qt loop, which stalls while the
 * e-ink refreshes — measured on-device, pen positions arrive from it in
 * bursts up to ~50ms apart. Since we run as root on the tablet we can read
 * the Wacom digitizer straight from /dev/input instead: ~1ms latency at
 * hardware rate, plus REAL pressure (AppLoad only ever forwards 0 or 100).
 * The digitizer maps to the whole SCREEN, so this is only correct when the
 * app runs fullscreen (the default); windowed mode is detected via
 * AppLoad's own pen events and inking falls back to them (see handle_input). */

#define PEN_SLOT (SLOTS - 1)
#define ERASER_R 20 /* the Marker's tail erases with a wide brush */

static int pen_fd = -1;      /* /dev/input/eventN of the digitizer, or -1 */
static int direct_pen = 0;   /* 1 while evdev inking is active and trusted */
static int pen_wx, pen_wy;   /* latest raw digitizer coordinates */
static int pen_pressure;     /* raw, 0..4095 */
static int pen_is_rubber;    /* 1 while the pen's ERASER end faces the glass */
static int pen_touching, pen_was_touching;
static int pen_sx, pen_sy;   /* latest mapped screen coordinates */

static void stroke_to(int slot, int x, int y) {
    /* the eraser paints white and wide; the pen tip inks black, with the
     * real pressure (direct input only) modulating the brush width */
    int rubber = slot == PEN_SLOT && direct_pen && pen_is_rubber;
    uint16_t color = rubber ? WHITE : BLACK;
    int r = rubber ? ERASER_R
            : (slot == PEN_SLOT && direct_pen && pen_pressure > 0)
                ? 2 + pen_pressure * 5 / 4096
                : BRUSH_R;
    int x0 = slot_x[slot], y0 = slot_y[slot];
    int dx = x - x0, dy = y - y0;
    int steps = (abs(dx) > abs(dy) ? abs(dx) : abs(dy)) + 1;

    for (int i = 0; i <= steps; i++) {
        int sx = x0 + dx * i / steps;
        int sy = y0 + dy * i / steps;
        /* clamp the stamp into the canvas so strokes can't paint the UI */
        if (sy - r >= CANVAS_Y0 && sy + r < CANVAS_Y1)
            disc(sx, sy, r, color);
    }
    slot_x[slot] = x;
    slot_y[slot] = y;

    /* remember the segment's bounding box (padded by the brush radius);
     * the main loop sends one refresh for everything once input is drained */
    mark_dirty((x0 < x ? x0 : x) - r, (y0 < y ? y0 : y) - r,
               (x0 > x ? x0 : x) + r, (y0 > y ? y0 : y) + r);
}

/* Tap the "SAMPLE APP" title to cycle the e-ink waveform live and feel the
 * latency/quality trade-off yourself; which mode is fastest varies between
 * OS versions. The current mode is shown at the bottom-right. */
static void cycle_refresh_mode(void) {
    static const char *names[] = {"UFAST", "FAST", "ANIM", "CONTENT", "UI"};
    static int mode = REFRESH_MODE_UFAST;
    mode = (mode + 1) % 5;
    set_refresh_mode(mode);
    char label[24];
    snprintf(label, sizeof label, "MODE:%s", names[mode]);
    int x = FB_W - 32 - text_width("MODE:CONTENT", 3), y = FB_H - FOOTER_H + 20;
    fill_rect(x, y, FB_W - x, 7 * 3, WHITE);
    draw_text(x, y, label, 3, GRAY);
    update_region(x, y, FB_W - x, 7 * 3);
    printf("sample-app: refresh mode -> %s\n", names[mode]);
}

/* a press from EITHER source (AppLoad message or the digitizer itself):
 * buttons first, otherwise begin a stroke with a dot */
static void pointer_press(int slot, int x, int y) {
    if (in_rect(x, y, BTN_EXIT_X, BTN_Y, BTN_W, BTN_H)) {
        running = 0;
    } else if (in_rect(x, y, BTN_CLEAR_X, BTN_Y, BTN_W, BTN_H)) {
        clear_canvas();
    } else if (in_rect(x, y, 0, 0, 560, HEADER_H)) {
        cycle_refresh_mode();
    } else {
        slot_active[slot] = 1;
        slot_x[slot] = x;
        slot_y[slot] = y;
        stroke_to(slot, x, y);
    }
}

/* ---- palm rejection --------------------------------------------------------
 * While the pen is on (or hovering near) the screen, the touchscreen mostly
 * reports the writing hand. So: any pen event stamps a timestamp, and touch
 * input is ignored wholesale — buttons included, a resting palm must not
 * press CLEAR — until the pen has been away for PEN_TIMEOUT_MS. Marks the
 * palm made *before* the pen arrived can't be unpainted; pen-down at least
 * stops the palm strokes from growing. The pen can tap buttons itself. */
#define PEN_TIMEOUT_MS 1500

static long long last_pen_ms = 0;

static int pen_recent(void) {
    return last_pen_ms != 0 && now_ms() - last_pen_ms < PEN_TIMEOUT_MS;
}

/* ---- direct pen input: reading the digitizer -------------------------------
 * Minimal evdev definitions, instead of <linux/input.h>: the kernel's
 * 32-bit input_event record is fixed at 16 bytes, but the header's struct
 * grows to 24 under musl's 64-bit time_t — defining it ourselves keeps
 * both the glibc (docker) and musl (zig) builds correct. */
struct raw_input_event {
    uint32_t sec, usec;
    uint16_t type, code;
    int32_t value;
};
#define EV_SYN 0
#define EV_KEY 1
#define EV_ABS 3
#define BTN_TOOL_PEN 0x140    /* pen tip enters/leaves hover range */
#define BTN_TOOL_RUBBER 0x141 /* eraser end (Marker tail) does */
#define BTN_TOUCH 0x14a       /* either end touches/leaves the glass */
#define ABS_X 0
#define ABS_Y 1
#define ABS_PRESSURE 24
/* _IOC(_IOC_READ, 'E', 0x06, len) spelled out, to not depend on kernel
 * headers: direction<<30 | size<<16 | type<<8 | nr (generic/arm layout) */
#define EVIOCGNAME(len) (2u << 30 | (uint32_t)(len) << 16 | 'E' << 8 | 0x06)

/* rM2 Wacom geometry (from rM2-stuff's rMlib): ABS_X runs 0..20967 along
 * the LONG edge, ABS_Y 0..15725 along the short edge, rotated vs the
 * screen:  screen_x = wy * W / 15725   screen_y = H - wx * H / 20967 */
#define WACOM_X_MAX 20967
#define WACOM_Y_MAX 15725

static void open_pen_device(void) {
    char path[32], name[64];
    for (int i = 0; i < 8; i++) {
        snprintf(path, sizeof path, "/dev/input/event%d", i);
        int fd = open(path, O_RDONLY | O_NONBLOCK);
        if (fd < 0)
            continue;
        if (ioctl(fd, EVIOCGNAME(sizeof name), name) > 0 && strstr(name, "Wacom")) {
            printf("sample-app: direct pen input from %s (%s)\n", path, name);
            pen_fd = fd;
            direct_pen = 1;
            return;
        }
        close(fd);
    }
    printf("sample-app: no Wacom device, inking via AppLoad events\n");
}

/* The digitizer streams ABS_X/ABS_Y/ABS_PRESSURE followed by an EV_SYN
 * "frame complete" marker; we act once per frame. Any pen sighting also
 * feeds the palm-rejection clock — including hover, which the hardware
 * reports from ~1cm above the glass, usually before the palm lands. */
static void drain_pen(void) {
    struct raw_input_event ev[64];
    ssize_t n;
    while ((n = read(pen_fd, ev, sizeof ev)) > 0) {
        for (int i = 0; i < (int)(n / (ssize_t)sizeof ev[0]); i++) {
            switch (ev[i].type) {
            case EV_ABS:
                if (ev[i].code == ABS_X) pen_wx = ev[i].value;
                else if (ev[i].code == ABS_Y) pen_wy = ev[i].value;
                else if (ev[i].code == ABS_PRESSURE) pen_pressure = ev[i].value;
                break;
            case EV_KEY:
                if (ev[i].code == BTN_TOOL_PEN)
                    last_pen_ms = now_ms(); /* hover begins/ends */
                else if (ev[i].code == BTN_TOOL_RUBBER) {
                    pen_is_rubber = ev[i].value; /* Marker flipped over */
                    last_pen_ms = now_ms();
                } else if (ev[i].code == BTN_TOUCH)
                    pen_touching = ev[i].value;
                break;
            case EV_SYN:
                last_pen_ms = now_ms();
                if (!direct_pen)
                    break; /* still useful above for palm rejection */
                pen_sx = pen_wy * FB_W / WACOM_Y_MAX;
                pen_sy = FB_H - pen_wx * FB_H / WACOM_X_MAX;
                if (pen_touching && !pen_was_touching) {
                    for (int s = 0; s < SLOTS - 1; s++)
                        slot_active[s] = 0; /* freeze palm strokes */
                    pointer_press(PEN_SLOT, pen_sx, pen_sy);
                } else if (pen_touching && slot_active[PEN_SLOT]) {
                    stroke_to(PEN_SLOT, pen_sx, pen_sy);
                } else if (!pen_touching) {
                    slot_active[PEN_SLOT] = 0;
                }
                pen_was_touching = pen_touching;
                break;
            }
        }
    }
}

static void handle_input(const qtfb_userinput *in) {
    int slot = slot_for(in);
    int t = in->inputType;

#ifdef DEBUG_INPUT /* build with make CFLAGS_EXTRA=-DDEBUG_INPUT, watch make log */
    printf("in: type=0x%02x dev=%d x=%4d y=%4d d=%d\n", t, in->devId, in->x,
           in->y, in->d);
#endif

    if (t >= INPUT_TOUCH_PRESS && t <= INPUT_TOUCH_UPDATE && pen_recent())
        return; /* that "touch" is a palm */
    if (t >= INPUT_PEN_PRESS && t <= INPUT_PEN_UPDATE) {
        last_pen_ms = now_ms();
        if (t == INPUT_PEN_PRESS) /* freeze whatever the palm was drawing */
            for (int i = 0; i < SLOTS - 1; i++)
                slot_active[i] = 0;
        if (direct_pen) {
            /* The digitizer inks the pen; AppLoad's delayed copies of the
             * same strokes only serve as a sanity check. Its coords are
             * WINDOW-relative — if they disagree with our screen mapping,
             * we're running windowed (long-press launch) and the mapping
             * is wrong: hand inking back to AppLoad's events. */
            if (t == INPUT_PEN_PRESS && (pen_sx || pen_sy) &&
                abs(in->x - pen_sx) + abs(in->y - pen_sy) > 150) {
                printf("sample-app: windowed? falling back to AppLoad pen\n");
                direct_pen = 0;
            } else {
                return;
            }
        }
    }

    switch (t) {
    case INPUT_TOUCH_PRESS:
    case INPUT_PEN_PRESS:
        pointer_press(slot, in->x, in->y);
        break;

    case INPUT_TOUCH_UPDATE:
    case INPUT_PEN_UPDATE:
        /* in->d carries pen pressure here — but only ever 0 or 100;
         * the direct digitizer path gets the real 0..4095 range */
        if (slot_active[slot])
            stroke_to(slot, in->x, in->y);
        break;

    case INPUT_TOUCH_RELEASE:
    case INPUT_PEN_RELEASE:
        slot_active[slot] = 0;
        break;

    case INPUT_VKB_PRESS:
        /* AppLoad's on-screen keyboard (if you enable it): d = keycode */
        printf("sample-app: key 0x%x\n", in->d);
        break;
    }
}

/* ---- setup + main loop ---------------------------------------------------- */

int main(void) {
    /* stdout goes to xochitl's journal through a pipe, which libc would
     * fully buffer — line-buffer it so printf shows up as it happens */
    setvbuf(stdout, NULL, _IOLBF, 0);

    /* 1. AppLoad tells us which framebuffer is ours via QTFB_KEY. */
    const char *key_env = getenv("QTFB_KEY");
    if (!key_env) {
        fprintf(stderr, "sample-app: QTFB_KEY not set. "
                        "This program must be launched by AppLoad.\n");
        return 1;
    }

    /* 2. Connect to the qtfb server living inside xochitl. */
    sock_fd = socket(AF_UNIX, SOCK_SEQPACKET, 0);
    if (sock_fd < 0) {
        perror("socket");
        return 1;
    }
    struct sockaddr_un addr = {.sun_family = AF_UNIX};
    strncpy(addr.sun_path, QTFB_SOCKET_PATH, sizeof(addr.sun_path) - 1);
    if (connect(sock_fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        perror("connect " QTFB_SOCKET_PATH);
        return 1;
    }

    /* 3. Handshake: claim our key, ask for the rM2 format (RGB565). */
    qtfb_client_message init = {
        .type = MESSAGE_INITIALIZE,
        .init = {.framebufferKey = atoi(key_env), .framebufferType = FBFMT_RM2FB},
    };
    qtfb_send(&init);

    qtfb_server_message resp;
    if (recv(sock_fd, &resp, sizeof resp, 0) < 1) {
        perror("recv init response");
        return 1;
    }

    /* 4. Map the shared memory the server just described. Writing to this
     *    buffer + sending MESSAGE_UPDATE is all that "drawing" means. */
    char shm_name[32];
    snprintf(shm_name, sizeof shm_name, QTFB_SHM_NAME_FMT, resp.init.shmKeyDefined);
    int shm_fd = shm_open(shm_name, O_RDWR, 0);
    if (shm_fd < 0) {
        perror("shm_open");
        return 1;
    }
    fb = mmap(NULL, resp.init.shmSize, PROT_READ | PROT_WRITE, MAP_SHARED, shm_fd, 0);
    if (fb == MAP_FAILED) {
        perror("mmap");
        return 1;
    }
    if (resp.init.shmSize < (size_t)FB_W * FB_H * 2) {
        fprintf(stderr, "sample-app: shm smaller than expected (%zu)\n",
                resp.init.shmSize);
        return 1;
    }
    printf("sample-app: up, fb=%dx%d shm=%s (%zu bytes)\n", FB_W, FB_H,
           shm_name, resp.init.shmSize);

    /* sigaction with sa_flags = 0 (no SA_RESTART), so a signal interrupts
     * the blocking recv() below with EINTR instead of silently restarting
     * it — otherwise SIGTERM wouldn't break the loop until the next event */
    struct sigaction sa = {0};
    sa.sa_handler = on_signal;
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT, &sa, NULL);
    signal(SIGPIPE, SIG_IGN); /* send() to a closed window must not kill us */

    draw_scene();
    /* ink with the low-latency waveform from here on (see set_refresh_mode) */
    set_refresh_mode(REFRESH_MODE_UFAST);
    open_pen_device();

    /* 5. Event loop, two sources: the qtfb socket (touch, and pen when the
     *    digitizer isn't available) and the pen hardware itself. poll()
     *    wakes us for either; while strokes are pending its timeout is the
     *    flush deadline, so ink hits the screen at most FLUSH_MS late but
     *    never more often than the throttle allows. recv() returning 0
     *    means AppLoad closed our window. */
    while (running) {
        int timeout = -1; /* idle: sleep until input arrives */
        if (dirty) {
            long long w = FLUSH_MS - (now_ms() - last_flush);
            timeout = w < 0 ? 0 : (int)w;
        }
        struct pollfd pfds[2] = {
            {.fd = sock_fd, .events = POLLIN},
            {.fd = pen_fd, .events = POLLIN}, /* -1 = absent; poll skips it */
        };
        if (poll(pfds, 2, timeout) < 0)
            continue; /* EINTR: a signal fired; `running` decides */

        if (pfds[1].revents & POLLIN)
            drain_pen();
        if (pfds[0].revents & POLLIN) {
            qtfb_server_message msg;
            ssize_t n;
            while ((n = recv(sock_fd, &msg, sizeof msg, MSG_DONTWAIT)) > 0)
                if (msg.type == MESSAGE_USERINPUT)
                    handle_input(&msg.userInput);
            if (n == 0 || (n < 0 && errno != EAGAIN && errno != EWOULDBLOCK &&
                           errno != EINTR))
                break; /* AppLoad closed our window */
        }
        if (dirty && now_ms() - last_flush >= FLUSH_MS) {
            flush_dirty();
            last_flush = now_ms();
        }
    }

    /* 6. Clean exit: say goodbye so the server tears the window down. */
    printf("sample-app: exiting\n");
    qtfb_client_message bye = {.type = MESSAGE_TERMINATE};
    qtfb_send(&bye);
    munmap(fb, resp.init.shmSize);
    close(sock_fd);
    return 0;
}
