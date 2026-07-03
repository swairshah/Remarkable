# pi-collab — build & deploy for the reMarkable 2 (AppLoad app, Rust)
#
# Usual loop:   edit src/*.rs  ->  make deploy  ->  tap the icon on the tablet
#
#   make            cross-compile (cargo + rust-lld; no docker needed)
#   make preview    run under qemu with a FAKE pi -> build/preview.png
#   make deploy     build + push binary/manifest/icon to the device
#   make log        tail xochitl's journal (app + pi output land there)
#   make restart-ui restart xochitl+xovi (only if the icon won't appear)
#   make kill       kill a running instance on the device
#   make uninstall  remove the app from the device
#   make clean      remove build output
#
# One-time setup: rustup target add armv7-unknown-linux-musleabihf
# Requires pi already on the tablet (../pi/pi-harness/install.sh) — pi-collab
# runs `/home/root/bin/pi --mode rpc` in the background.

HOST ?= root@192.168.1.7
BIN := pi-collab
TARGET := armv7-unknown-linux-musleabihf
BUILT := target/$(TARGET)/release/$(BIN)
DEVICE_DIR := /home/root/xovi/exthome/appload/pi-collab
SSH := ssh -o ConnectTimeout=8 $(HOST)

# The device has pgrep but NO pkill. The [p] bracket stops pgrep -f from
# matching its own ssh command line.
PGREP := pgrep -f 'appload/pi-collab/[p]i-collab'
KILL_APP := q=$$($(PGREP)); if [ -n "$$q" ]; then kill $$q; sleep 1; kill -9 $$q 2>/dev/null; fi

.PHONY: all build preview deploy log restart-ui kill uninstall clean

all: build

build:
	cargo build --release
	file $(BUILT)

# No tablet needed: qemu runs the real arm binary, a fake pi (test/fake-pi.py)
# plays the RPC other end, and test/fake-qtfb.py scripts input + screenshots.
preview: build
	mkdir -p build
	chmod +x test/fake-pi.py
	docker build --platform linux/amd64 -t pi-collab-preview -f test/preview.Dockerfile test
	docker run --rm --platform linux/amd64 -v "$(CURDIR):/src" -w /src \
		pi-collab-preview python3 test/fake-qtfb.py $(BUILT) build/preview.png

# Streamed over plain ssh (no scp: the device sshd has no sftp subsystem).
# Kill first: overwriting a running binary fails with "text file busy".
deploy: build
	$(SSH) '$(KILL_APP); mkdir -p $(DEVICE_DIR)'
	cat $(BUILT) | $(SSH) 'cat > $(DEVICE_DIR)/$(BIN) && chmod +x $(DEVICE_DIR)/$(BIN)'
	cat app/external.manifest.json | $(SSH) 'cat > $(DEVICE_DIR)/external.manifest.json'
	cat app/icon.png | $(SSH) 'cat > $(DEVICE_DIR)/icon.png'
	@echo
	@echo 'Deployed. On the tablet: open the AppLoad menu -> tap "pi-collab".'
	@echo 'If the icon is missing (first install), run: make restart-ui'

log:
	$(SSH) 'journalctl -u xochitl -f -n 50 | grep --line-buffered -iE "pi-collab|pi:"'

restart-ui:
	$(SSH) '/home/root/xovi/start'

kill:
	$(SSH) '$(KILL_APP); true'

uninstall:
	$(SSH) '$(KILL_APP); rm -rf $(DEVICE_DIR)'
	@echo 'Removed $(DEVICE_DIR).'

clean:
	cargo clean
	rm -rf build
