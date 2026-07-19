# Tiny image for `make preview`: runs the arm binary under qemu against a
# fake qtfb server (test/fake-qtfb.py) and screenshots the framebuffer.
#
# arm64 on purpose (not amd64): on an Apple-silicon docker host the harness
# python then runs NATIVELY and only the armv7 app binary is emulated —
# one qemu layer instead of two. Nested emulation (amd64 container on an
# aarch64 VM) breaks MAP_SHARED coherency between the app and the harness:
# the app's framebuffer writes never reach the screenshot reader.
FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends python3 qemu-user-static && \
    rm -rf /var/lib/apt/lists/*
