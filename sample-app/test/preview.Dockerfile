# Tiny image for `make preview`: runs the arm binary under qemu against a
# fake qtfb server (test/fake-qtfb.py) and screenshots the framebuffer.
FROM debian:stable-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends python3-minimal qemu-user-static && \
    rm -rf /var/lib/apt/lists/*
