# notebook — build & deploy for the reMarkable 2 (takeover app, Rust)
#
# Usual loop:   edit src/*.rs  ->  make deploy  ->  tap the icon on the tablet
#
#   make              cross-compile (cargo + rust-lld; no docker needed)
#   make preview      run under qemu with a FAKE pi -> build/preview.png
#   make fetch-server download rm2fb_server (timower/rM2-stuff) into vendor/
#   make deploy       build + push binary/scripts/manifest/server/extension
#   make log          tail the app log on the device (/tmp/notebook.log)
#   make log-server   tail the rm2fb server log (/tmp/rm2fb.log)
#   make kill         stop a running takeover session (restores xochitl)
#   make restore-ui   emergency: restart xochitl by hand
#   make uninstall    remove the app from the device
#   make clean        remove build output
#
# One-time setup: rustup target add armv7-unknown-linux-musleabihf
# Requires pi already on the tablet (../pi/pi-harness/install.sh) — notebook
# runs `/home/root/bin/pi --mode rpc` in the background with the
# notebook-canvas.ts extension (its page-drawing tools).

HOST ?= root@192.168.1.7
BIN := notebook
TARGET := armv7-unknown-linux-musleabihf
BUILT := target/$(TARGET)/release/$(BIN)
DEVICE_DIR := /home/root/xovi/exthome/appload/notebook
SSH := ssh -o ConnectTimeout=8 $(HOST)
RM2FB_IPK := https://github.com/timower/rM2-stuff/releases/latest/download/rm2display.ipk

# The device has pgrep but NO pkill. The [n] bracket stops pgrep -f from
# matching its own ssh command line. Stopping the transient unit first lets
# takeover.sh's trap restore xochitl cleanly.
KILL_APP := systemctl stop notebook-takeover 2>/dev/null; \
            q=$$(pgrep -f 'appload/notebook/[n]otebook'); \
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
preview: build
	mkdir -p build
	chmod +x test/fake-pi.py
	docker build --platform linux/amd64 -t notebook-preview -f test/preview.Dockerfile test
	docker run --rm --platform linux/amd64 -v "$(CURDIR):/src" -w /src \
		notebook-preview python3 test/fake-qtfb.py $(BUILT) build/preview.png

# The takeover display host: timower/rM2-stuff's rm2fb server. Prefer the
# copy the collab app already fetched; else download the .ipk.
fetch-server:
	mkdir -p vendor build
	@if [ -f ../collab/vendor/rm2fb_server ]; then \
		cp ../collab/vendor/rm2fb_server ../collab/vendor/librm2fb_server.so vendor/; \
		echo 'copied from ../collab/vendor'; \
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
	$(SSH) '$(KILL_APP); mkdir -p $(DEVICE_DIR)'
	cat $(BUILT) | $(SSH) 'cat > $(DEVICE_DIR)/$(BIN) && chmod +x $(DEVICE_DIR)/$(BIN)'
	cat scripts/appload-launch.sh | $(SSH) 'cat > $(DEVICE_DIR)/appload-launch.sh && chmod +x $(DEVICE_DIR)/appload-launch.sh'
	cat scripts/takeover.sh | $(SSH) 'cat > $(DEVICE_DIR)/takeover.sh && chmod +x $(DEVICE_DIR)/takeover.sh'
	cat ext/notebook-canvas.ts | $(SSH) 'cat > $(DEVICE_DIR)/notebook-canvas.ts'
	cat vendor/rm2fb_server | $(SSH) 'cat > $(DEVICE_DIR)/rm2fb_server && chmod +x $(DEVICE_DIR)/rm2fb_server'
	cat vendor/librm2fb_server.so | $(SSH) 'cat > $(DEVICE_DIR)/librm2fb_server.so'
	cat app/external.manifest.json | $(SSH) 'cat > $(DEVICE_DIR)/external.manifest.json'
	cat app/icon.png | $(SSH) 'cat > $(DEVICE_DIR)/icon.png'
	@echo
	@echo 'Deployed. On the tablet: open the AppLoad menu -> tap "notebook".'
	@echo 'It stops xochitl and takes the panel over; top-edge swipe -> CLOSE.'
	@echo 'If the icon is missing (first install), run: make restart-ui'

vendor/rm2fb_server:
	@echo 'vendor/rm2fb_server missing — run: make fetch-server' && exit 1

# Pull pi's session traces + the app log off the device and render them
# into one HTML (tools/trace-viewer.py): the device log up top, then every
# pause — the page image pi saw, its thinking, per-turn cost — and each
# notebook_draw overlaid in red on that page image.
trace:
	mkdir -p build/sessions
	$(SSH) 'tar -C /home/root/.local/share/notebook/sessions -cf - .' | tar -C build/sessions -xf -
	$(SSH) 'cat /tmp/notebook.log 2>/dev/null' > build/notebook.log || true
	python3 tools/trace-viewer.py build/sessions --log build/notebook.log -o build/trace.html
	open build/trace.html 2>/dev/null || true

log:
	$(SSH) 'touch /tmp/notebook.log; tail -f /tmp/notebook.log'

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
