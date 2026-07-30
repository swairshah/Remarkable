#pragma once
/*
 * qtfb.h — the AppLoad/qtfb wire protocol, in plain C.
 *
 * Derived from rm-appload (https://github.com/asivery/rm-appload),
 * backends/qtfb-clients/cpp/common.h, GPL-3.0. Struct layouts must match
 * that file exactly — the server (appload.so, running inside xochitl) reads
 * these bytes straight off the socket.
 *
 * How the whole thing works, in one paragraph:
 * AppLoad runs a unix SEQPACKET socket server at /tmp/qtfb.sock. When it
 * launches your app (manifest has "qtfb": true), it picks a framebuffer key
 * and passes it to you in the QTFB_KEY env var. You connect to the socket,
 * send MESSAGE_INITIALIZE with that key and a pixel format, and the server
 * replies with the name of a POSIX shared-memory object (/qtfb_<id>). You
 * shm_open + mmap it: that memory IS your screen. Draw into it, then send
 * MESSAGE_UPDATE to tell the server which region to blit into the AppLoad
 * window. Input (touch/pen/keyboard) arrives back over the same socket as
 * MESSAGE_USERINPUT packets. That's the entire API.
 */

#include <stdint.h>
#include <stddef.h>

#define QTFB_SOCKET_PATH "/tmp/qtfb.sock"

/* shm object name is "/qtfb_<key>" where <key> comes in the init response */
#define QTFB_SHM_NAME_FMT "/qtfb_%d"

/* Screen sizes per device family. The rM2 (and rM1) are 1404x1872. */
#define RM2_WIDTH 1404
#define RM2_HEIGHT 1872
#define RMPP_WIDTH 1620
#define RMPP_HEIGHT 2160

/* message.type values — client -> server */
#define MESSAGE_INITIALIZE 0        /* claim a framebuffer, pick a format   */
#define MESSAGE_UPDATE 1            /* "please blit this region"            */
#define MESSAGE_CUSTOM_INITIALIZE 2 /* like INITIALIZE but custom w/h       */
#define MESSAGE_TERMINATE 3         /* clean goodbye (send before exit)     */
#define MESSAGE_SET_REFRESH_MODE 5  /* e-ink waveform hint, REFRESH_MODE_*  */
#define MESSAGE_REQUEST_FULL_REFRESH 6 /* full deghosting flash             */
/* message.type values — server -> client */
#define MESSAGE_USERINPUT 4         /* touch / pen / button / keyboard      */

/* framebufferType values (pixel format + implied resolution).
 * FBFMT_RM2FB = 1404x1872, 16bpp RGB565 — what you want on a reMarkable 2. */
#define FBFMT_RM2FB 0
#define FBFMT_RMPP_RGB888 1
#define FBFMT_RMPP_RGBA8888 2
#define FBFMT_RMPP_RGB565 3

/* MESSAGE_UPDATE subtypes */
#define UPDATE_ALL 0
#define UPDATE_PARTIAL 1

/* e-ink refresh (waveform) modes for MESSAGE_SET_REFRESH_MODE.
 * UFAST/FAST: low latency, more ghosting — good while inking.
 * UI (default): balanced. CONTENT: high quality, slow. */
#define REFRESH_MODE_UFAST 0
#define REFRESH_MODE_FAST 1
#define REFRESH_MODE_ANIMATE 2
#define REFRESH_MODE_CONTENT 3
#define REFRESH_MODE_UI 4

/* UserInput.inputType values */
#define INPUT_TOUCH_PRESS 0x10
#define INPUT_TOUCH_RELEASE 0x11
#define INPUT_TOUCH_UPDATE 0x12
#define INPUT_PEN_PRESS 0x20
#define INPUT_PEN_RELEASE 0x21
#define INPUT_PEN_UPDATE 0x22
#define INPUT_BTN_PRESS 0x30   /* rM1 hardware buttons; never fires on rM2 */
#define INPUT_BTN_RELEASE 0x31
#define INPUT_VKB_PRESS 0x40   /* AppLoad's on-screen keyboard, d = keycode */
#define INPUT_VKB_RELEASE 0x41

/* --- wire structs ------------------------------------------------------- */

typedef struct {
    int framebufferKey;      /* from the QTFB_KEY env var */
    uint8_t framebufferType; /* FBFMT_* */
} qtfb_init;

typedef struct {
    int framebufferKey;
    uint8_t framebufferType;
    uint16_t width, height;  /* only for MESSAGE_CUSTOM_INITIALIZE */
} qtfb_custom_init;

typedef struct {
    int shmKeyDefined;       /* plug into QTFB_SHM_NAME_FMT */
    size_t shmSize;          /* bytes to mmap (w * h * bytes-per-pixel) */
} qtfb_init_response;

typedef struct {
    int type;                /* UPDATE_ALL or UPDATE_PARTIAL */
    int x, y, w, h;          /* region, ignored for UPDATE_ALL */
} qtfb_update;

typedef struct {
    int inputType;           /* INPUT_* */
    int devId;               /* finger slot for multitouch, 0 for pen */
    int x, y;                /* framebuffer coordinates */
    int d;                   /* pen: pressure, keyboard: keycode */
} qtfb_userinput;

typedef struct {
    uint8_t type;            /* MESSAGE_* */
    union {
        qtfb_init init;
        qtfb_custom_init customInit;
        qtfb_update update;
        int refreshMode;
    };
} qtfb_client_message;

typedef struct {
    uint8_t type;            /* MESSAGE_* */
    union {
        qtfb_init_response init;
        qtfb_userinput userInput;
    };
} qtfb_server_message;

/* The server is a 32-bit arm process; these sizes are what it expects on the
 * wire. size_t is 8 bytes on 64-bit targets, which shifts the layout — so
 * this only builds correctly for 32-bit, and we only assert there. */
#if UINTPTR_MAX == 0xFFFFFFFF
_Static_assert(sizeof(qtfb_client_message) == 24, "client message ABI drift");
_Static_assert(sizeof(qtfb_server_message) == 24, "server message ABI drift");
#endif
