# sample-app-rust — build & deploy for the reMarkable 2 (AppLoad app, Rust)
#
# Usual loop:   edit src/main.rs  ->  make deploy  ->  tap the icon on the tablet
#
#   make            cross-compile (cargo + rust-lld; no docker needed)
#   make preview    run under qemu against a fake server -> build/preview.png
#   make deploy     build + push binary/manifest/icon to the device
#   make log        tail xochitl's journal (println! output lands there)
#   make restart-ui restart xochitl+xovi (only needed if the icon won't appear)
#   make kill       kill a running instance on the device
#   make uninstall  remove the app from the device
#   make clean      remove build output
#
# One-time setup: rustup target add armv7-unknown-linux-musleabihf
# (musl => fully static binary, no device library dependencies)

#HOST ?= root@10.11.99.1
HOST ?= root@192.168.1.7
BIN := sample-app-rs
TARGET := armv7-unknown-linux-musleabihf
BUILT := target/$(TARGET)/release/$(BIN)
DEVICE_DIR := /home/root/xovi/exthome/appload/sample-app-rust
SSH := ssh -o ConnectTimeout=8 $(HOST)

.PHONY: all build preview deploy log restart-ui kill uninstall clean

all: build

build:
	cargo build --release
	file $(BUILT)

# No tablet needed: run the app under qemu against a fake qtfb server with
# scripted touch input, and screenshot the framebuffer to build/preview.png.
# (test/fake-qtfb.py is shared with ../sample-app — it's language-agnostic.)
preview: build
	mkdir -p build
	docker build --platform linux/amd64 -t sample-app-preview -f test/preview.Dockerfile test
	docker run --rm --platform linux/amd64 -v "$(CURDIR):/src" -w /src \
		sample-app-preview python3 test/fake-qtfb.py $(BUILT) build/preview.png

# The device has pgrep but NO pkill. The [s] bracket keeps pgrep -f from
# matching this very ssh command line (which contains the pattern too).
PGREP := pgrep -f 'appload/sample-app-rust/[s]ample-app-rs'
KILL_APP := p=$$($(PGREP)); if [ -n "$$p" ]; then kill $$p; sleep 1; kill -9 $$p 2>/dev/null; fi

# Streamed over plain ssh (no scp: the device sshd has no sftp subsystem).
# Kill first: overwriting a running binary fails with "text file busy".
# TERM, a grace period, then KILL — a doodle pad has no state to lose.
deploy: build
	$(SSH) '$(KILL_APP); mkdir -p $(DEVICE_DIR)'
	cat $(BUILT) | $(SSH) 'cat > $(DEVICE_DIR)/$(BIN) && chmod +x $(DEVICE_DIR)/$(BIN)'
	cat app/external.manifest.json | $(SSH) 'cat > $(DEVICE_DIR)/external.manifest.json'
	cat app/icon.png | $(SSH) 'cat > $(DEVICE_DIR)/icon.png'
	@echo
	@echo 'Deployed. On the tablet: open the AppLoad menu -> tap "sample-rs".'
	@echo 'If the icon is missing (first install), run: make restart-ui'

log:
	$(SSH) 'journalctl -u xochitl -f -n 50 | grep --line-buffered -iE "sample|appload|qtfb"'

restart-ui:
	$(SSH) '/home/root/xovi/start'

kill:
	$(SSH) '$(KILL_APP); true'

uninstall:
	$(SSH) '$(KILL_APP); rm -rf $(DEVICE_DIR)'
	@echo 'Removed $(DEVICE_DIR). The icon disappears next time the menu reloads.'

clean:
	cargo clean
	rm -rf build
