# sample-app-rust — the Rust twin of ../sample-app

The same minimal AppLoad doodle pad for the reMarkable 2 as
[`../sample-app/`](../sample-app/), written in Rust. Same UI, same features
(pressure-sensitive pen ink read directly from the Wacom digitizer, eraser
end, palm rejection, live refresh-mode cycling via the title), same
protocol, same Makefile workflow — use whichever language you'd rather grow
an app in. If you haven't read the C version's README, read it first:
everything about AppLoad, the qtfb protocol, the latency story, deployment,
and troubleshooting applies here unchanged.

**Prerequisite:** the xovi + AppLoad stack from `../pi/pi-appload/install.sh`
must already be on the tablet.

## Quick start

```sh
rustup target add armv7-unknown-linux-musleabihf   # once
make            # cargo build --release (cross-compiles, ~seconds)
make deploy     # push to the tablet (default HOST=root@10.11.99.1)
```

Then on the tablet: AppLoad menu → **sample-rs**.

No docker needed for building: the target is armv7 + musl (fully static,
zero device dependencies) and rust-lld from rustup does the linking —
configured in `.cargo/config.toml`. The binary comes out ~300 KB.

## Files

```
src/main.rs                 the app: scene, strokes, brushes, event loop
src/qtfb.rs                 the AppLoad/qtfb protocol (socket + shm + events)
src/pen.rs                  direct Wacom digitizer input (low-latency ink,
                            real pressure, eraser detection)
src/draw.rs                 pixel/rect/disc/text primitives on the framebuffer
src/font.rs                 tiny 5x7 bitmap font (same table as the C sample)
app/external.manifest.json  what AppLoad launches
app/icon.png                launcher icon
test/                       fake qtfb server for `make preview` (shared design
                            with ../sample-app — it is language-agnostic)
```

## Why not the official Rust client crate?

The rm-appload repo ships a `qtfb-client` crate
(`backends/qtfb-clients/rust`), but as of now it only covers pixel output —
no input events, no refresh modes, no terminate message. A doodle pad needs
input, so `src/qtfb.rs` implements the full protocol directly (modeled on
upstream's C++ `common.h`, credited in the header). It's ~300 lines and
yours to extend; if upstream's crate grows input support later, swapping it
in is straightforward.

Rust-specific notes, if you're porting your own ideas onto this:

- The wire structs are `#[repr(C)]` with unions, matching the 32-bit server
  ABI; compile-time size asserts guard against drift (they only run for
  32-bit targets — `usize` differs on your dev machine).
- `qtfb::connect()` returns a `(Framebuffer, Socket)` pair, so mutating
  pixels and sending messages don't fight over one borrow.
- Input arrives as a decoded `Event` enum (`Touch`/`Pen`/`Key`), nicer to
  match on than raw packet constants.
- Rust ignores SIGPIPE by default, so a `send()` after the window closes is
  an `Err`, not a crash. SIGTERM/SIGINT are installed *without* SA_RESTART
  so they actually interrupt the blocking `recv()`.

## Testing without the tablet

```sh
make preview    # qemu + fake server -> build/preview.png
open build/preview.png
```

Exercises the real arm binary end-to-end: handshake, scene, scripted finger
and pen strokes, EXIT tap, clean shutdown.

## Everything else

`make log`, `make kill`, `make uninstall`, `make restart-ui`, deployment
details, troubleshooting, hacking ideas: see
[`../sample-app/README.md`](../sample-app/README.md) — identical here, with
`sample-app-rs` as the binary and `sample-rs` as the launcher name.
