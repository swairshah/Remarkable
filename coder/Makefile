# coder — build & deploy for the reMarkable 2 (takeover app, Rust)
#
# Usual loop:   edit src/*.rs  ->  make deploy  ->  tap the icon on the tablet
#
#   make              cross-compile (cargo + rust-lld; no docker needed)
#   make preview      run under qemu with a FAKE pi -> build/preview.png
#   make fetch-server download rm2fb_server (timower/rM2-stuff) into vendor/
#   make deploy       build + push binary/scripts/manifest/server/extension
#   make log          tail the app log on the device (/tmp/coder.log)
#   make log-server   tail the rm2fb server log (/tmp/rm2fb.log)
#   make kill         stop a running takeover session (restores xochitl)
#   make restore-ui   emergency: restart xochitl by hand
#   make uninstall    remove the app from the device
#   make clean        remove build output
#
# One-time setup: rustup target add armv7-unknown-linux-musleabihf
# Requires pi already on the tablet (../pi/pi-harness/install.sh) — coder
# runs `/home/root/bin/pi --mode rpc` in the background with the
# coder-canvas.ts extension (its page-drawing tools).

HOST ?= remarkable
BIN := coder
TARGET := armv7-unknown-linux-musleabihf
BUILT := target/$(TARGET)/release/$(BIN)
DEVICE_DIR := /home/root/xovi/exthome/appload/coder
SSH := ssh -o ConnectTimeout=8 $(HOST)
RM2FB_IPK := https://github.com/timower/rM2-stuff/releases/latest/download/rm2display.ipk

# The device has pgrep but NO pkill. The [n] bracket stops pgrep -f from
# matching its own ssh command line. Stopping the transient unit first lets
# takeover.sh's trap restore xochitl cleanly.
KILL_APP := systemctl stop coder-takeover 2>/dev/null; \
            q=$$(pgrep -f 'appload/coder/[c]oder'); \
            if [ -n "$$q" ]; then kill $$q; sleep 1; kill -9 $$q 2>/dev/null; fi

.PHONY: all build preview fetch-server deploy trace log log-server kill restore-ui uninstall clean

all: build

build:
	cargo build --release
	file $(BUILT)

# No tablet needed: qemu runs the real arm binary, a fake pi (test/fake-pi.py)
# plays the RPC other end AND exercises the tool socket, and
# test/fake-qtfb.py scripts input + screenshots. This exercises the WINDOWED
# (qtfb) backend; takeover needs the device.
# Native-arch container (no --platform pin): on Apple Silicon the amd64
# image runs qemu-arm-static under Rosetta, whose address-space layout
# makes 32-bit guest mapping flaky ("Unable to find a guest_base").
preview: build
	mkdir -p build
	chmod +x test/fake-pi.py
	docker build -t coder-preview -f test/preview.Dockerfile test
	docker run --rm -v "$(CURDIR):/src" \
		-v "$(abspath $(CURDIR)/../../libreink):/libreink:ro" -e LIBREINK_PREVIEW=/libreink/tools/preview -w /src \
		coder-preview python3 test/fake-qtfb.py $(BUILT) build/preview.png

# The takeover display host: timower/rM2-stuff's rm2fb server. Prefer the
# copy the collab app already fetched; else download the .ipk.
fetch-server:
	mkdir -p vendor build
	@if [ -f ../sketchbook/vendor/rm2fb_server ]; then \
		cp ../sketchbook/vendor/rm2fb_server ../sketchbook/vendor/librm2fb_server.so vendor/; \
		echo 'copied from ../sketchbook/vendor'; \
	else \
		curl -sL -o build/rm2display.ipk $(RM2FB_IPK) && \
		cd build && tar xf rm2display.ipk data.tar.gz && \
		tar xzf data.tar.gz ./opt/bin/rm2fb_server ./opt/lib/librm2fb_server.so && \
		cp opt/bin/rm2fb_server opt/lib/librm2fb_server.so ../vendor/; \
	fi
	file vendor/rm2fb_server

# Streamed over plain ssh (no scp: the device sshd has no sftp subsystem).
# Kill first: overwriting a running binary fails with "text file busy".
deploy: build vendor/rm2fb_server
	node --experimental-strip-types --check ext/coder-canvas.ts   # a broken extension kills pi silently on-device
	node --experimental-strip-types --check ext/coder-context.ts
	$(SSH) '$(KILL_APP); mkdir -p $(DEVICE_DIR)'
	cat $(BUILT) | $(SSH) 'cat > $(DEVICE_DIR)/$(BIN) && chmod +x $(DEVICE_DIR)/$(BIN)'
	cat scripts/appload-launch.sh | $(SSH) 'cat > $(DEVICE_DIR)/appload-launch.sh && chmod +x $(DEVICE_DIR)/appload-launch.sh'
	cat scripts/takeover.sh | $(SSH) 'cat > $(DEVICE_DIR)/takeover.sh && chmod +x $(DEVICE_DIR)/takeover.sh'
	cat ext/coder-canvas.ts | $(SSH) 'cat > $(DEVICE_DIR)/coder-canvas.ts'
	cat ext/coder-context.ts | $(SSH) 'cat > $(DEVICE_DIR)/coder-context.ts'
	cat vendor/rm2fb_server | $(SSH) 'cat > $(DEVICE_DIR)/rm2fb_server && chmod +x $(DEVICE_DIR)/rm2fb_server'
	cat vendor/librm2fb_server.so | $(SSH) 'cat > $(DEVICE_DIR)/librm2fb_server.so'
	cat app/external.manifest.json | $(SSH) 'cat > $(DEVICE_DIR)/external.manifest.json'
	cat app/icon.png | $(SSH) 'cat > $(DEVICE_DIR)/icon.png'
	@echo
	@echo 'Deployed. On the tablet: open the AppLoad menu -> tap "coder".'
	@echo 'It stops xochitl and takes the panel over; top-edge swipe -> CLOSE.'
	@echo 'If the icon is missing (first install), run: make restart-ui'
	@echo 'The VM side: ssh exedev@remarkable.exe.xyz "mkdir -p ~/coder" (once).'

vendor/rm2fb_server:
	@echo 'vendor/rm2fb_server missing — run: make fetch-server' && exit 1

# Pull pi's session traces + the app log off the device and render them
# into one HTML (tools/trace-viewer.py): the device log up top, then every
# pause — the page image pi saw, its thinking, per-turn cost — and each
# coder_draw overlaid in red on that page image.
trace:
	mkdir -p build/sessions
	$(SSH) 'tar -C /home/root/.local/share/coder/sessions -cf - .' | tar -C build/sessions -xf -
	$(SSH) 'cat /tmp/coder.log 2>/dev/null' > build/coder.log || true
	python3 ../notebook/tools/trace-viewer.py build/sessions --log build/coder.log -o build/trace.html
	open build/trace.html 2>/dev/null || true

log:
	$(SSH) 'touch /tmp/coder.log; tail -f /tmp/coder.log'

log-server:
	$(SSH) 'touch /tmp/rm2fb.log; tail -f /tmp/rm2fb.log'

kill:
	$(SSH) '$(KILL_APP); true'

restore-ui:
	$(SSH) 'systemctl start xochitl'

restart-ui:
	$(SSH) '/home/root/xovi/start'

uninstall:
	$(SSH) '$(KILL_APP); rm -rf $(DEVICE_DIR)'
	@echo 'Removed $(DEVICE_DIR).'

clean:
	cargo clean
	rm -rf build
