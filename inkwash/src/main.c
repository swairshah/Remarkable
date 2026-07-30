/*
 * inkwash — pen-and-ink with living water, on a reMarkable 2.
 *
 * A native AppLoad port of https://github.com/johnowhitaker/inkwash
 * (the single-file WebGL2 fluid-painting app, see ../../inkwash in this
 * account): the PEN lays down crisp dark ink; WATER makes the paper wet,
 * and wherever the paper is wet the ink lifts, flows, bleeds and blends —
 * then dries back into the page. On the tablet the mapping is:
 *
 *     pen tip                  ink (real pressure shapes the line)
 *     Marker tail (eraser end) water brush
 *     finger                   water brush too (when the pen is away)
 *     FIX button               flash-dry: settle every wash into the paper
 *
 * The original runs a GPU fluid sim per frame. An i.MX7 has no GPU worth
 * the name, so this port re-thinks the pipeline for a CPU + e-ink:
 *
 *   - the fluid state (wetness, velocity, mobile pigment, fixed pigment)
 *     lives on a quarter-resolution grid (351x468) — bleeding is a soft,
 *     low-frequency phenomenon, it doesn't need 1404x1872
 *   - pen linework stays at FULL resolution in its own layer, so lines are
 *     razor sharp; water gradually transfers line pixels into the sim's
 *     mobile pigment, which is exactly how a real ink line re-wets
 *   - the pressure-projection step of the original is dropped; brush-driven
 *     velocity + wet-confined diffusion carries the look
 *   - rendering composites lines + pigment + wet sheen through an
 *     exp() absorption LUT with paper grain, then dithers: to pure B/W
 *     (stipple, like a halftone) while the fast e-ink waveform is active,
 *     and to true 16-level gray for the slow "the wash dries" refresh
 *     that fires when all the water has evaporated
 *
 * All the AppLoad/qtfb plumbing (shared framebuffer, input, batching,
 * direct Wacom digitizer reads, palm rejection) is inherited from
 * ../sample-app — read that first if this is your first AppLoad app.
 */

#include <errno.h>
#include <fcntl.h>
#include <math.h>
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

#define WHITE 0xFFFF
#define BLACK 0x0000
#define GRAY 0x8410

/* ---- UI layout (framebuffer pixels) ------------------------------------ */

#define HEADER_H 140
#define FOOTER_H 80
#define BTN_H 88
#define BTN_Y 26

#define BTN_EXIT_W 150
#define BTN_CLEAR_W 180
#define BTN_FIX_W 150
#define BTN_BLEED_W 210
#define BTN_GAP 20
#define BTN_EXIT_X (FB_W - 24 - BTN_EXIT_W)
#define BTN_CLEAR_X (BTN_EXIT_X - BTN_GAP - BTN_CLEAR_W)
#define BTN_FIX_X (BTN_CLEAR_X - BTN_GAP - BTN_FIX_W)
#define BTN_BLEED_X (BTN_FIX_X - BTN_GAP - BTN_BLEED_W)

#define CANVAS_Y0 (HEADER_H + 4)
#define CANVAS_Y1 (FB_H - FOOTER_H)

/* ---- simulation geometry ------------------------------------------------ */

#define DOWN 4                /* sim cell = DOWN x DOWN screen pixels */
#define SW (FB_W / DOWN)      /* 351 */
#define SH (FB_H / DOWN)      /* 468 */
#define CELL_Y0 (CANVAS_Y0 / DOWN)
#define CELL_Y1 (CANVAS_Y1 / DOWN)

#define SIM_MS 40             /* one fluid step every 40ms (25 Hz) */
#define SIM_DT 0.040f

/* ---- globals ------------------------------------------------------------ */

static int sock_fd = -1;
static uint16_t *fb;
static volatile sig_atomic_t running = 1;

static void on_signal(int sig) {
    (void)sig;
    running = 0;
}

/* ---- protocol helpers (same as sample-app) ------------------------------ */

static void qtfb_send(const qtfb_client_message *m) {
    if (send(sock_fd, m, sizeof *m, 0) < 0)
        perror("qtfb send");
}

static void update_all(void) {
    qtfb_client_message m = {.type = MESSAGE_UPDATE, .update = {.type = UPDATE_ALL}};
    qtfb_send(&m);
}

static void update_region(int x, int y, int w, int h) {
    qtfb_client_message m = {
        .type = MESSAGE_UPDATE,
        .update = {.type = UPDATE_PARTIAL, .x = x, .y = y, .w = w, .h = h},
    };
    qtfb_send(&m);
}

static void full_refresh(void) {
    qtfb_client_message m = {.type = MESSAGE_REQUEST_FULL_REFRESH};
    qtfb_send(&m);
}

static void set_refresh_mode(int mode) {
    qtfb_client_message m = {.type = MESSAGE_SET_REFRESH_MODE, .refreshMode = mode};
    qtfb_send(&m);
}

/* ---- update batching (same scheme as sample-app) ------------------------ */

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

/* ---- fluid state ----------------------------------------------------------
 * Double-buffered where a pass reads neighbors while writing. All static:
 * ~6MB of float grids + a 2.6MB full-res line layer; the rM2 has 1GB. */

static float buf_wet[2][SW * SH], buf_ink[2][SW * SH];
static float buf_vx[2][SW * SH], buf_vy[2][SW * SH];
static float fixd[SW * SH];              /* pigment settled into the paper */
static float *wet = buf_wet[0], *wet2 = buf_wet[1];
static float *ink = buf_ink[0], *ink2 = buf_ink[1];
static float *vx = buf_vx[0], *vx2 = buf_vx[1];
static float *vy = buf_vy[0], *vy2 = buf_vy[1];

static uint8_t lines[FB_W * FB_H];       /* crisp pen ink, full resolution */

/* per-cell composite density, 0..4095 == absorbance 0..4.0, kept in sync by
 * the sim tick; the renderer bilinearly upsamples this and adds `lines` */
static uint16_t cdens[SW * SH];

/* sim scheduling + active region (cell coords, inclusive) */
static int sim_active = 0;
static long long next_sim = 0;
static int ax0, ay0, ax1, ay1;           /* cells the sim currently touches */
static int sx0, sy0, sx1, sy1;           /* session bbox for the dry refresh */

/* water-brush footprint of the current tick, for scrub/bleed boost */
static float brush_cx, brush_cy, brush_r = 0;

/* BLEED button cycles how eagerly pigment spreads in water */
static const float BLEED_LEVELS[] = {0.25f, 0.55f, 0.90f};
static const char *BLEED_NAMES[] = {"LO", "MED", "HI"};
static int bleed_ix = 1;

#define DRY_TAU 10.0f        /* seconds for the water to evaporate */
#define WET_SPREAD 0.12f     /* how much water creeps to neighbors per step */
#define REWET_RATE 1.1f      /* how fast water dissolves crisp linework */
#define INK_STRENGTH 2458.0f /* pigment 1.0 -> absorbance idx (2.4 * 1024) */
#define LINE_IDX 10          /* lines 255 -> absorbance idx 2550 (~2.5) */
#define WET_SHEEN 360        /* absorbance idx of fully wet blank paper */

/* ---- lookup tables ------------------------------------------------------- */

static uint8_t lut[4096];     /* absorbance idx -> gray (255 * exp(-idx/1024)) */
static int8_t grain[128 * 128]; /* paper texture, +-24 */
static const uint8_t bayer[4][4] = {
    {0, 8, 2, 10}, {12, 4, 14, 6}, {3, 11, 1, 9}, {15, 7, 13, 5}};

static uint32_t hash32(uint32_t x) {
    x ^= x >> 16; x *= 0x7feb352d;
    x ^= x >> 15; x *= 0x846ca68b;
    x ^= x >> 16;
    return x;
}

static void init_tables(void) {
    for (int i = 0; i < 4096; i++) {
        float v = 255.0f * expf(-(float)i / 1024.0f);
        lut[i] = (uint8_t)(v + 0.5f);
    }
    /* two octaves of hash noise: fine tooth + coarse fiber */
    for (int y = 0; y < 128; y++)
        for (int x = 0; x < 128; x++) {
            int fine = (int)(hash32(y * 131 + x) & 31);
            int coarse = (int)(hash32(((y >> 2) * 37 + (x >> 2)) * 2654435761u) & 31);
            grain[y * 128 + x] = (int8_t)(((fine + coarse) * 24) / 31 - 24);
        }
}

/* ---- rendering -------------------------------------------------------------
 * Composite one framebuffer region from the layers. `gray16` picks the
 * dither: 0 = pure black/white (for the fast near-binary waveform: washes
 * appear as halftone stipple, which reads as ink on e-ink), 1 = ordered
 * dither to the panel's 16 gray levels (for quality refreshes). */

static int live_mode = REFRESH_MODE_UFAST; /* current live waveform */

static void render_region(int x0, int y0, int x1, int y1, int gray16) {
    if (x0 < 0) x0 = 0;
    if (y0 < CANVAS_Y0) y0 = CANVAS_Y0;
    if (x1 > FB_W) x1 = FB_W;
    if (y1 > CANVAS_Y1) y1 = CANVAS_Y1;
    if (x0 >= x1 || y0 >= y1)
        return;

    for (int y = y0; y < y1; y++) {
        /* bilinear setup: sample the cell grid at pixel centers, 8.8 fixed */
        int py = y * (256 / DOWN) + (256 / DOWN) / 2 - 128;
        if (py < 0) py = 0;
        int cy = py >> 8, wy = py & 255;
        if (cy >= SH - 1) { cy = SH - 2; wy = 255; }
        const uint16_t *row0 = cdens + cy * SW, *row1 = row0 + SW;
        const uint8_t *lrow = lines + y * FB_W;
        uint16_t *frow = fb + y * FB_W;
        const int8_t *grow = grain + (y & 127) * 128;
        const uint8_t *brow = bayer[y & 3];

        int px = x0 * (256 / DOWN) + (256 / DOWN) / 2 - 128;
        for (int x = x0; x < x1; x++, px += 256 / DOWN) {
            int p = px < 0 ? 0 : px;
            int cx = p >> 8, wx = p & 255;
            if (cx >= SW - 1) { cx = SW - 2; wx = 255; }
            int d00 = row0[cx], d10 = row0[cx + 1];
            int d01 = row1[cx], d11 = row1[cx + 1];
            int top = d00 + (((d10 - d00) * wx) >> 8);
            int bot = d01 + (((d11 - d01) * wx) >> 8);
            int idx = top + (((bot - top) * wy) >> 8) + lrow[x] * LINE_IDX;
            idx += (idx * grow[x & 127]) >> 9; /* +-5% paper grain */
            if (idx > 4095) idx = 4095;
            int v = lut[idx];
            int t = brow[x & 3];
            if (gray16) {
                int q = v * 15;
                int lvl = q >> 8;
                if ((q & 255) > t * 16 + 8) lvl++;
                if (lvl > 15) lvl = 15;
                int g = lvl * 17;
                frow[x] = (uint16_t)(((g >> 3) << 11) | ((g >> 2) << 5) | (g >> 3));
            } else {
                frow[x] = v > t * 16 + 8 ? WHITE : BLACK;
            }
        }
    }
}

/* ---- fluid step -------------------------------------------------------------
 * The original's shader passes, collapsed to two CPU sweeps over the active
 * bounding box. Velocity has no pressure projection: it is injected by the
 * brush, confined to wet paper, and decays — enough for directional bleed. */

static inline float clampf(float v, float lo, float hi) {
    return v < lo ? lo : v > hi ? hi : v;
}

static inline float bsample(const float *f, float x, float y) {
    x = clampf(x, 0, SW - 1.001f);
    y = clampf(y, 0, SH - 1.001f);
    int ix = (int)x, iy = (int)y;
    float fx = x - ix, fy = y - iy;
    const float *r0 = f + iy * SW + ix, *r1 = r0 + SW;
    return (r0[0] * (1 - fx) + r0[1] * fx) * (1 - fy) +
           (r1[0] * (1 - fx) + r1[1] * fx) * fy;
}

static void activate_sim(void) {
    if (!sim_active) {
        sim_active = 1;
        next_sim = now_ms() + SIM_MS;
        sx0 = ax0; sy0 = ay0; sx1 = ax1; sy1 = ay1;
    }
}

static void grow_active(int x0, int y0, int x1, int y1) {
    if (x0 < 0) x0 = 0;
    if (y0 < CELL_Y0) y0 = CELL_Y0;
    if (x1 > SW - 1) x1 = SW - 1;
    if (y1 > CELL_Y1 - 1) y1 = CELL_Y1 - 1;
    if (x0 > x1 || y0 > y1)
        return;
    if (!sim_active && ax0 > ax1) { /* empty box */
        ax0 = x0; ay0 = y0; ax1 = x1; ay1 = y1;
    } else {
        if (x0 < ax0) ax0 = x0;
        if (y0 < ay0) ay0 = y0;
        if (x1 > ax1) ax1 = x1;
        if (y1 > ay1) ay1 = y1;
    }
    activate_sim();
    if (ax0 < sx0) sx0 = ax0;
    if (ay0 < sy0) sy0 = ay0;
    if (ax1 > sx1) sx1 = ax1;
    if (ay1 > sy1) sy1 = ay1;
}

/* settle every remaining wash into the paper and repaint it with the slow,
 * quality waveform: the visible "drying into the page" moment */
static void settle_and_refresh(void) {
    for (int y = sy0; y <= sy1; y++)
        for (int x = sx0; x <= sx1; x++) {
            int i = y * SW + x;
            fixd[i] += ink[i];
            ink[i] = 0; wet[i] = 0; vx[i] = 0; vy[i] = 0;
            int d = (int)(fixd[i] * INK_STRENGTH);
            cdens[i] = (uint16_t)(d > 4095 ? 4095 : d);
        }
    flush_dirty(); /* push pending live updates before switching waveform */
    int px0 = sx0 * DOWN - DOWN, py0 = sy0 * DOWN - DOWN;
    int px1 = (sx1 + 2) * DOWN, py1 = (sy1 + 2) * DOWN;
    render_region(px0, py0, px1, py1, 1);
    set_refresh_mode(REFRESH_MODE_UI);
    if (px0 < 0) px0 = 0;
    if (py0 < CANVAS_Y0) py0 = CANVAS_Y0;
    if (px1 > FB_W) px1 = FB_W;
    if (py1 > CANVAS_Y1) py1 = CANVAS_Y1;
    update_region(px0, py0, px1 - px0, py1 - py0);
    set_refresh_mode(live_mode);
    sim_active = 0;
    ax0 = ay0 = SW + SH; ax1 = ay1 = -1; /* empty */
}

static void sim_step(void) {
    const float dt = SIM_DT;
    const float vdecay = expf(-dt * 2.5f);
    const float wdecay = expf(-dt / DRY_TAU);
    const float bleed = BLEED_LEVELS[bleed_ix];
    float maxwet = 0;

    /* the wet front creeps ~1 cell per step; let the domain follow it */
    grow_active(ax0 - 1, ay0 - 1, ax1 + 1, ay1 + 1);

    /* pass 1: water advects, creeps, evaporates; velocity decays and is
     * confined to wet paper */
    for (int y = ay0; y <= ay1; y++) {
        int ym = y > 0 ? y - 1 : 0, yp = y < SH - 1 ? y + 1 : SH - 1;
        for (int x = ax0; x <= ax1; x++) {
            int i = y * SW + x;
            int xm = x > 0 ? x - 1 : 0, xp = x < SW - 1 ? x + 1 : SW - 1;
            float w = wet[i];
            float m = clampf((w - 0.005f) / 0.195f, 0, 1);
            float cvx = clampf(vx[i], -150, 150) * vdecay * m;
            float cvy = clampf(vy[i], -150, 150) * vdecay * m;
            vx2[i] = cvx;
            vy2[i] = cvy;
            float wadv = (cvx != 0 || cvy != 0)
                             ? bsample(wet, x - dt * cvx * 0.6f, y - dt * cvy * 0.6f)
                             : w;
            float navg = (wet[y * SW + xm] + wet[y * SW + xp] +
                          wet[ym * SW + x] + wet[yp * SW + x]) * 0.25f;
            float nw = (wadv + (navg - wadv) * WET_SPREAD) * wdecay;
            if (nw < 1e-4f) nw = 0;
            wet2[i] = nw;
            if (nw > maxwet) maxwet = nw;
        }
    }
    { float *t;
      t = wet; wet = wet2; wet2 = t;
      t = vx; vx = vx2; vx2 = t;
      t = vy; vy = vy2; vy2 = t; }

    /* pass 2: pigment moves and bleeds only where wet; water lifts settled
     * pigment back up (scrubbing with the brush lifts much more); wet cells
     * dissolve the crisp linework beneath them into mobile pigment */
    float br2 = brush_r * brush_r;
    for (int y = ay0; y <= ay1; y++) {
        int ym = y > 0 ? y - 1 : 0, yp = y < SH - 1 ? y + 1 : SH - 1;
        for (int x = ax0; x <= ax1; x++) {
            int i = y * SW + x;
            float w = wet[i];
            float m = clampf((w - 0.02f) / 0.43f, 0, 1);
            if (m < 0.002f) {
                ink2[i] = ink[i];
                continue;
            }
            int xm = x > 0 ? x - 1 : 0, xp = x < SW - 1 ? x + 1 : SW - 1;
            float b = 0;
            if (br2 > 0) {
                float ddx = x - brush_cx, ddy = y - brush_cy;
                float d2 = ddx * ddx + ddy * ddy;
                if (d2 < br2 * 6.0f)
                    b = expf(-d2 / br2);
            }
            float cur = ink[i];
            float adv = bsample(ink, x - dt * vx[i] * m, y - dt * vy[i] * m);
            float navg = (ink[y * SW + xm] + ink[y * SW + xp] +
                          ink[ym * SW + x] + ink[yp * SW + x]) * 0.25f;
            float g = 1.0f + grain[(y & 127) * 128 + (x & 127)] / 96.0f;
            float amt = clampf(bleed * (0.25f + 1.3f * b) * m * g, 0, 0.92f);
            float mixed = adv + (navg - adv) * amt;
            float ni = cur + (mixed - cur) * m;
            float lift = clampf(w * (0.04f + 0.35f * b) * dt, 0, 0.25f);
            ni += fixd[i] * lift;
            fixd[i] *= 1.0f - lift;
            /* re-wet the full-res line layer under this cell */
            if (w > 0.10f) {
                float frac = dt * REWET_RATE * m;
                if (frac > 0.35f) frac = 0.35f;
                int taken = 0;
                for (int j = 0; j < DOWN; j++) {
                    uint8_t *lp = lines + (y * DOWN + j) * FB_W + x * DOWN;
                    for (int k = 0; k < DOWN; k++) {
                        int t = (int)(lp[k] * frac);
                        lp[k] -= (uint8_t)t;
                        taken += t;
                    }
                }
                if (taken)
                    ni += taken * (1.0f / (255.0f * DOWN * DOWN));
            }
            ink2[i] = ni;
        }
    }
    { float *t = ink; ink = ink2; ink2 = t; }
    brush_r = 0; /* footprint consumed; the next stroke event re-arms it */

    /* refresh the composite density grid; remember which cells actually
     * changed so we only repaint (and e-ink refresh) that region */
    int rx0 = SW, ry0 = SH, rx1 = -1, ry1 = -1;
    for (int y = ay0; y <= ay1; y++)
        for (int x = ax0; x <= ax1; x++) {
            int i = y * SW + x;
            float ws = clampf((wet[i] - 0.02f) / 0.58f, 0, 1);
            int d = (int)((ink[i] + fixd[i]) * INK_STRENGTH + ws * WET_SHEEN);
            if (d > 4095) d = 4095;
            if (abs(d - cdens[i]) >= 4) {
                cdens[i] = (uint16_t)d;
                if (x < rx0) rx0 = x;
                if (y < ry0) ry0 = y;
                if (x > rx1) rx1 = x;
                if (y > ry1) ry1 = y;
            }
        }
    if (rx1 >= 0) {
        int px0 = rx0 * DOWN - DOWN, py0 = ry0 * DOWN - DOWN;
        int px1 = (rx1 + 2) * DOWN, py1 = (ry1 + 2) * DOWN;
        render_region(px0, py0, px1, py1, 0);
        mark_dirty(px0 < 0 ? 0 : px0, py0 < CANVAS_Y0 ? CANVAS_Y0 : py0,
                   px1 > FB_W - 1 ? FB_W - 1 : px1,
                   py1 > CANVAS_Y1 - 1 ? CANVAS_Y1 - 1 : py1);
    }

    if (maxwet < 0.004f) {
        settle_and_refresh(); /* everything has evaporated: dry the page */
        printf("inkwash: washes dried\n");
    }
}

/* ---- splats ------------------------------------------------------------- */

static void splat_wet(float px_x, float px_y, float r_px, float amp) {
    float cx = (px_x + 0.5f) / DOWN - 0.5f, cy = (px_y + 0.5f) / DOWN - 0.5f;
    float r = r_px / DOWN;
    if (r < 1.0f) r = 1.0f;
    int x0 = (int)(cx - r * 2), x1 = (int)(cx + r * 2) + 1;
    int y0 = (int)(cy - r * 2), y1 = (int)(cy + r * 2) + 1;
    grow_active(x0, y0, x1, y1);
    if (x0 < 0) x0 = 0;
    if (y0 < CELL_Y0) y0 = CELL_Y0;
    if (x1 > SW - 1) x1 = SW - 1;
    if (y1 > CELL_Y1 - 1) y1 = CELL_Y1 - 1;
    float inv = 1.0f / (r * r);
    for (int y = y0; y <= y1; y++)
        for (int x = x0; x <= x1; x++) {
            float dx = x - cx, dy = y - cy;
            float f = amp * expf(-(dx * dx + dy * dy) * inv);
            int i = y * SW + x;
            if (f > wet[i])
                wet[i] = f; /* MAX blend, like the original's water splat */
        }
}

static void splat_vel(float px_x, float px_y, float r_px, float ax, float ay) {
    float cx = (px_x + 0.5f) / DOWN - 0.5f, cy = (px_y + 0.5f) / DOWN - 0.5f;
    float r = r_px / DOWN * 1.15f;
    if (r < 1.0f) r = 1.0f;
    int x0 = (int)(cx - r * 2), x1 = (int)(cx + r * 2) + 1;
    int y0 = (int)(cy - r * 2), y1 = (int)(cy + r * 2) + 1;
    if (x0 < 0) x0 = 0;
    if (y0 < CELL_Y0) y0 = CELL_Y0;
    if (x1 > SW - 1) x1 = SW - 1;
    if (y1 > CELL_Y1 - 1) y1 = CELL_Y1 - 1;
    float inv = 1.0f / (r * r);
    for (int y = y0; y <= y1; y++)
        for (int x = x0; x <= x1; x++) {
            float dx = x - cx, dy = y - cy;
            float f = expf(-(dx * dx + dy * dy) * inv);
            int i = y * SW + x;
            vx[i] += ax * f;
            vy[i] += ay * f;
        }
}

/* ---- input: strokes ---------------------------------------------------------
 * Same slot machinery as sample-app: fingers get slots 0..14, the pen slot
 * 15. Each slot remembers whether it is inking or watering, its last point
 * and its last event time (for the water's flow velocity). */

#define SLOTS 16
#define PEN_SLOT (SLOTS - 1)
static int slot_active[SLOTS];
static int slot_water[SLOTS];
static int slot_x[SLOTS], slot_y[SLOTS];
static long long slot_ms[SLOTS];

static int pen_fd = -1;
static int direct_pen = 0;
static int pen_wx, pen_wy;
static int pen_pressure;
static int pen_is_rubber;
static int pen_touching, pen_was_touching;
static int pen_sx, pen_sy;

static int slot_for(const qtfb_userinput *in) {
    if (in->inputType >= INPUT_PEN_PRESS && in->inputType <= INPUT_PEN_UPDATE)
        return PEN_SLOT;
    return in->devId % (SLOTS - 1);
}

static int in_rect(int x, int y, int rx, int ry, int rw, int rh) {
    return x >= rx && x < rx + rw && y >= ry && y < ry + rh;
}

/* crisp ink: stamp a pressure-sized disc into the line layer + framebuffer */
static void ink_disc(int cx, int cy, int r) {
    for (int j = -r; j <= r; j++) {
        int y = cy + j;
        if (y < CANVAS_Y0 || y >= CANVAS_Y1)
            continue;
        for (int i = -r; i <= r; i++) {
            int x = cx + i;
            if (x < 0 || x >= FB_W)
                continue;
            if (i * i + j * j <= r * r) {
                lines[y * FB_W + x] = 255;
                fb[y * FB_W + x] = BLACK;
            }
        }
    }
}

static void stroke_to(int slot, int x, int y) {
    int x0 = slot_x[slot], y0 = slot_y[slot];
    int dx = x - x0, dy = y - y0;
    long long t = now_ms();
    long long el = t - slot_ms[slot];
    if (el < 1) el = 1;
    if (el > 100) el = 100;
    slot_ms[slot] = t;
    float dist = sqrtf((float)(dx * dx + dy * dy));

    if (slot_water[slot]) {
        /* water: pressure sets the brush width; motion drives the flow */
        float pr = (slot == PEN_SLOT && direct_pen && pen_pressure > 0)
                       ? pen_pressure / 4096.0f
                       : 0.45f;
        float r = 30.0f + 100.0f * pr;
        float amp = 0.5f + 0.5f * pr;
        float axv = 0, ayv = 0;
        if (dist > 0.5f) {
            float v = dist / el * 1000.0f / DOWN * 1.2f; /* cells per second */
            if (v > 150.0f) v = 150.0f;
            axv = dx / dist * v * 0.5f;
            ayv = dy / dist * v * 0.5f;
        }
        int steps = (int)(dist / (r * 0.7f)) + 1;
        if (steps > 12) steps = 12;
        for (int i = 1; i <= steps; i++) {
            float px = x0 + dx * (float)i / steps;
            float py = y0 + dy * (float)i / steps;
            splat_wet(px, py, r, amp);
            if (axv != 0 || ayv != 0)
                splat_vel(px, py, r, axv, ayv);
        }
        brush_cx = (x + 0.5f) / DOWN - 0.5f;
        brush_cy = (y + 0.5f) / DOWN - 0.5f;
        brush_r = r / DOWN;
    } else {
        /* ink: dark full-res discs; real pressure only via the digitizer */
        int r = (slot == PEN_SLOT && direct_pen && pen_pressure > 0)
                    ? 3 + pen_pressure * 8 / 4096
                    : 5;
        int steps = (abs(dx) > abs(dy) ? abs(dx) : abs(dy)) + 1;
        for (int i = 0; i <= steps; i++) {
            int sx = x0 + dx * i / steps;
            int sy = y0 + dy * i / steps;
            ink_disc(sx, sy, r);
            /* fresh ink leaves the paper faintly damp, so overlapping
             * strokes blend a little — same trick as the original */
            if ((i & 7) == 0)
                splat_wet(sx, sy, r * 2.8f, 0.13f);
        }
        mark_dirty((x0 < x ? x0 : x) - r, (y0 < y ? y0 : y) - r,
                   (x0 > x ? x0 : x) + r, (y0 > y ? y0 : y) + r);
    }
    slot_x[slot] = x;
    slot_y[slot] = y;
}

/* ---- UI ------------------------------------------------------------------ */

static int draw_text(int x, int y, const char *s, int scale, uint16_t c);
static int text_width(const char *s, int scale);
static void fill_rect(int x, int y, int w, int h, uint16_t c);
static void rect_outline(int x, int y, int w, int h, int t, uint16_t c);

static void draw_button(int x, int y, int w, const char *label, int scale) {
    fill_rect(x, y, w, BTN_H, WHITE);
    rect_outline(x, y, w, BTN_H, 4, BLACK);
    draw_text(x + (w - text_width(label, scale)) / 2,
              y + (BTN_H - 7 * scale) / 2, label, scale, BLACK);
}

static void draw_bleed_button(void) {
    char label[16];
    snprintf(label, sizeof label, "BLEED:%s", BLEED_NAMES[bleed_ix]);
    draw_button(BTN_BLEED_X, BTN_Y, BTN_BLEED_W, label, 3);
    update_region(BTN_BLEED_X, BTN_Y, BTN_BLEED_W, BTN_H);
}

static void clear_canvas(void) {
    memset(lines, 0, sizeof lines);
    memset(buf_wet, 0, sizeof buf_wet);
    memset(buf_ink, 0, sizeof buf_ink);
    memset(buf_vx, 0, sizeof buf_vx);
    memset(buf_vy, 0, sizeof buf_vy);
    memset(fixd, 0, sizeof fixd);
    memset(cdens, 0, sizeof cdens);
    sim_active = 0;
    ax0 = ay0 = SW + SH; ax1 = ay1 = -1;
    brush_r = 0;
    fill_rect(0, CANVAS_Y0, FB_W, CANVAS_Y1 - CANVAS_Y0, WHITE);
    dirty = 0;
    update_all();
    full_refresh();
}

static void fix_drawing(void) {
    if (sim_active) {
        settle_and_refresh();
    } else {
        /* nothing wet: repaint the whole canvas in quality gray anyway —
         * doubles as a deghosting pass for the wash areas */
        sx0 = 0; sy0 = CELL_Y0; sx1 = SW - 1; sy1 = CELL_Y1 - 1;
        settle_and_refresh();
    }
    printf("inkwash: fixed\n");
}

static void draw_scene(void) {
    fill_rect(0, 0, FB_W, FB_H, WHITE);
    draw_text(32, (HEADER_H - 7 * 6) / 2, "INKWASH", 6, BLACK);
    draw_bleed_button();
    draw_button(BTN_FIX_X, BTN_Y, BTN_FIX_W, "FIX", 4);
    draw_button(BTN_CLEAR_X, BTN_Y, BTN_CLEAR_W, "CLEAR", 4);
    draw_button(BTN_EXIT_X, BTN_Y, BTN_EXIT_W, "EXIT", 4);
    fill_rect(0, HEADER_H, FB_W, 4, BLACK);
    fill_rect(0, FB_H - FOOTER_H - 4, FB_W, 2, GRAY);
    draw_text(32, FB_H - FOOTER_H + 20,
              "PEN INKS - FLIP THE MARKER OR USE A FINGER FOR WATER", 3, GRAY);
    update_all();
}

static void cycle_refresh_mode(void) {
    static const char *names[] = {"UFAST", "FAST", "ANIM", "CONTENT", "UI"};
    live_mode = (live_mode + 1) % 5;
    set_refresh_mode(live_mode);
    char label[24];
    snprintf(label, sizeof label, "MODE:%s", names[live_mode]);
    int x = FB_W - 32 - text_width("MODE:CONTENT", 3), y = FB_H - FOOTER_H + 20;
    fill_rect(x, y, FB_W - x, 7 * 3, WHITE);
    draw_text(x, y, label, 3, GRAY);
    update_region(x, y, FB_W - x, 7 * 3);
    printf("inkwash: refresh mode -> %s\n", names[live_mode]);
}

static void pointer_press(int slot, int x, int y, int water) {
    if (in_rect(x, y, BTN_EXIT_X, BTN_Y, BTN_EXIT_W, BTN_H)) {
        running = 0;
    } else if (in_rect(x, y, BTN_CLEAR_X, BTN_Y, BTN_CLEAR_W, BTN_H)) {
        clear_canvas();
    } else if (in_rect(x, y, BTN_FIX_X, BTN_Y, BTN_FIX_W, BTN_H)) {
        fix_drawing();
    } else if (in_rect(x, y, BTN_BLEED_X, BTN_Y, BTN_BLEED_W, BTN_H)) {
        bleed_ix = (bleed_ix + 1) % 3;
        draw_bleed_button();
    } else if (in_rect(x, y, 0, 0, 500, HEADER_H)) {
        cycle_refresh_mode();
    } else {
        slot_active[slot] = 1;
        slot_water[slot] = water;
        slot_x[slot] = x;
        slot_y[slot] = y;
        slot_ms[slot] = now_ms();
        stroke_to(slot, x, y);
    }
}

/* ---- palm rejection (same as sample-app) --------------------------------- */

#define PEN_TIMEOUT_MS 1500
static long long last_pen_ms = 0;

static int pen_recent(void) {
    return last_pen_ms != 0 && now_ms() - last_pen_ms < PEN_TIMEOUT_MS;
}

/* ---- direct pen input (see sample-app for the full story) ----------------- */

struct raw_input_event {
    uint32_t sec, usec;
    uint16_t type, code;
    int32_t value;
};
#define EV_SYN 0
#define EV_KEY 1
#define EV_ABS 3
#define BTN_TOOL_PEN 0x140
#define BTN_TOOL_RUBBER 0x141
#define BTN_TOUCH 0x14a
#define ABS_X 0
#define ABS_Y 1
#define ABS_PRESSURE 24
#define EVIOCGNAME(len) (2u << 30 | (uint32_t)(len) << 16 | 'E' << 8 | 0x06)

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
            printf("inkwash: direct pen input from %s (%s)\n", path, name);
            pen_fd = fd;
            direct_pen = 1;
            return;
        }
        close(fd);
    }
    printf("inkwash: no Wacom device, inking via AppLoad events\n");
}

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
                    last_pen_ms = now_ms();
                else if (ev[i].code == BTN_TOOL_RUBBER) {
                    /* the Marker flipped: its tail is the water brush */
                    pen_is_rubber = ev[i].value;
                    last_pen_ms = now_ms();
                } else if (ev[i].code == BTN_TOUCH)
                    pen_touching = ev[i].value;
                break;
            case EV_SYN:
                last_pen_ms = now_ms();
                if (!direct_pen)
                    break;
                pen_sx = pen_wy * FB_W / WACOM_Y_MAX;
                pen_sy = FB_H - pen_wx * FB_H / WACOM_X_MAX;
                if (pen_touching && !pen_was_touching) {
                    for (int s = 0; s < SLOTS - 1; s++)
                        slot_active[s] = 0; /* freeze palm strokes */
                    pointer_press(PEN_SLOT, pen_sx, pen_sy, pen_is_rubber);
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

#ifdef DEBUG_INPUT
    printf("in: type=0x%02x dev=%d x=%4d y=%4d d=%d\n", t, in->devId, in->x,
           in->y, in->d);
#endif

    if (t >= INPUT_TOUCH_PRESS && t <= INPUT_TOUCH_UPDATE && pen_recent())
        return; /* that "touch" is a palm */
    if (t >= INPUT_PEN_PRESS && t <= INPUT_PEN_UPDATE) {
        last_pen_ms = now_ms();
        if (t == INPUT_PEN_PRESS)
            for (int i = 0; i < SLOTS - 1; i++)
                slot_active[i] = 0;
        if (direct_pen) {
            if (t == INPUT_PEN_PRESS && (pen_sx || pen_sy) &&
                abs(in->x - pen_sx) + abs(in->y - pen_sy) > 150) {
                printf("inkwash: windowed? falling back to AppLoad pen\n");
                direct_pen = 0;
            } else {
                return;
            }
        }
    }

    switch (t) {
    case INPUT_TOUCH_PRESS:
        /* a finger is the water brush, exactly like the iPad version */
        pointer_press(slot, in->x, in->y, 1);
        break;
    case INPUT_PEN_PRESS:
        pointer_press(slot, in->x, in->y, 0);
        break;

    case INPUT_TOUCH_UPDATE:
    case INPUT_PEN_UPDATE:
        if (slot_active[slot])
            stroke_to(slot, in->x, in->y);
        break;

    case INPUT_TOUCH_RELEASE:
    case INPUT_PEN_RELEASE:
        slot_active[slot] = 0;
        break;
    }
}

/* ---- drawing primitives (UI chrome only) ---------------------------------- */

static void px_set(int x, int y, uint16_t c) {
    if (x >= 0 && x < FB_W && y >= 0 && y < FB_H)
        fb[y * FB_W + x] = c;
}

static void fill_rect(int x, int y, int w, int h, uint16_t c) {
    for (int j = y; j < y + h; j++)
        for (int i = x; i < x + w; i++)
            px_set(i, j, c);
}

static void rect_outline(int x, int y, int w, int h, int t, uint16_t c) {
    fill_rect(x, y, w, t, c);
    fill_rect(x, y + h - t, w, t, c);
    fill_rect(x, y, t, h, c);
    fill_rect(x + w - t, y, t, h, c);
}

static int draw_text(int x, int y, const char *s, int scale, uint16_t c) {
    int x0 = x;
    for (; *s; s++) {
        const uint8_t *g = font_lookup(*s);
        for (int col = 0; col < 5; col++)
            for (int row = 0; row < 7; row++)
                if ((g[col] >> row) & 1)
                    fill_rect(x + col * scale, y + row * scale, scale, scale, c);
        x += 6 * scale;
    }
    return x - x0;
}

static int text_width(const char *s, int scale) {
    return (int)strlen(s) * 6 * scale;
}

/* ---- setup + main loop ----------------------------------------------------- */

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    init_tables();
    ax0 = ay0 = SW + SH; ax1 = ay1 = -1;

    const char *key_env = getenv("QTFB_KEY");
    if (!key_env) {
        fprintf(stderr, "inkwash: QTFB_KEY not set. "
                        "This program must be launched by AppLoad.\n");
        return 1;
    }

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
        fprintf(stderr, "inkwash: shm smaller than expected (%zu)\n",
                resp.init.shmSize);
        return 1;
    }
    printf("inkwash: up, fb=%dx%d shm=%s (%zu bytes) sim=%dx%d\n", FB_W, FB_H,
           shm_name, resp.init.shmSize, SW, SH);

    struct sigaction sa = {0};
    sa.sa_handler = on_signal;
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT, &sa, NULL);
    signal(SIGPIPE, SIG_IGN);

    draw_scene();
    set_refresh_mode(live_mode);
    open_pen_device();

    /* Event loop: qtfb socket + pen hardware, with poll()'s timeout doing
     * double duty as the e-ink flush deadline AND the fluid-sim metronome.
     * The sim only ticks while some paper is wet; a dry page costs nothing. */
    while (running) {
        long long t = now_ms();
        int timeout = -1;
        if (dirty) {
            long long w = FLUSH_MS - (t - last_flush);
            timeout = w < 0 ? 0 : (int)w;
        }
        if (sim_active) {
            long long w = next_sim - t;
            int st = w < 0 ? 0 : (int)w;
            if (timeout < 0 || st < timeout)
                timeout = st;
        }
        struct pollfd pfds[2] = {
            {.fd = sock_fd, .events = POLLIN},
            {.fd = pen_fd, .events = POLLIN},
        };
        if (poll(pfds, 2, timeout) < 0)
            continue;

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
                break;
        }
        t = now_ms();
        if (sim_active && t >= next_sim) {
            sim_step();
            /* fixed cadence, but never schedule into the past if a step
             * (or a huge wash) overran the budget */
            next_sim += SIM_MS;
            if (next_sim < t)
                next_sim = t + SIM_MS;
        }
        if (dirty && now_ms() - last_flush >= FLUSH_MS) {
            flush_dirty();
            last_flush = now_ms();
        }
    }

    printf("inkwash: exiting\n");
    qtfb_client_message bye = {.type = MESSAGE_TERMINATE};
    qtfb_send(&bye);
    munmap(fb, resp.init.shmSize);
    close(sock_fd);
    return 0;
}
