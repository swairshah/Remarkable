# sample-app — build & deploy for the reMarkable 2 (AppLoad app)
#
# Usual loop:   edit src/main.c  ->  make deploy  ->  tap the icon on the tablet
#
#   make            cross-compile with the toltec docker image (repo standard)
#   make build-zig  same, but with `zig cc` (no docker needed, much faster)
#   make deploy     build + push binary/manifest/icon to the device
#   make log        tail xochitl's journal (your printf()s land there)
#   make restart-ui restart xochitl+xovi (only needed if the icon won't appear)
#   make kill       kill a running instance on the device
#   make uninstall  remove the app from the device
#   make clean      remove local build output

#HOST ?= root@10.11.99.1
HOST ?= root@192.168.1.7
BIN := sample-app
DEVICE_DIR := /home/root/xovi/exthome/appload/sample-app
IMAGE := ghcr.io/toltec-dev/base:v4.0
SSH := ssh -o ConnectTimeout=8 $(HOST)

SRC := src/main.c src/qtfb.h src/font5x7.h
# make CFLAGS_EXTRA=-DDEBUG_INPUT deploy  ->  log every input packet (make log)
CFLAGS := -O2 -Wall -Wextra -static $(CFLAGS_EXTRA)

.PHONY: all build build-zig preview deploy log restart-ui kill uninstall clean

all: build

build: build/$(BIN)

# The toltec toolchain targets the reMarkable's glibc; -static so we never
# worry about library versions on the device. -lrt for shm_open (old glibc).
build/$(BIN): $(SRC)
	mkdir -p build
	docker run --rm --platform linux/amd64 -v "$(CURDIR):/src" -w /src $(IMAGE) \
		bash -c '. /opt/x-tools/switch-arm.sh && \
			arm-linux-gnueabihf-gcc $(CFLAGS) -o build/$(BIN) src/main.c -lrt && \
			arm-linux-gnueabihf-strip build/$(BIN)'
	file build/$(BIN)

# Alternative: zig bundles a full arm cross toolchain (musl libc, static).
build-zig: $(SRC)
	mkdir -p build
	zig cc -target arm-linux-musleabihf -mcpu=cortex_a9 $(CFLAGS) \
		-o build/$(BIN) src/main.c
	file build/$(BIN)

# No tablet needed: run the app under qemu against a fake qtfb server with
# scripted touch input, and screenshot the framebuffer to build/preview.png.
preview: build/$(BIN)
	docker build --platform linux/amd64 -t sample-app-preview -f test/preview.Dockerfile test
	docker run --rm --platform linux/amd64 -v "$(CURDIR):/src" -w /src \
		sample-app-preview python3 test/fake-qtfb.py build/$(BIN) build/preview.png

# The device has pgrep but NO pkill. The [s] bracket keeps pgrep -f from
# matching this very ssh command line (which contains the pattern too).
PGREP := pgrep -f 'appload/sample-app/[s]ample-app'
KILL_APP := p=$$($(PGREP)); if [ -n "$$p" ]; then kill $$p; sleep 1; kill -9 $$p 2>/dev/null; fi

# Streamed over plain ssh (no scp: the device sshd has no sftp subsystem).
# Kill first: overwriting a running binary fails with "text file busy".
# TERM, a grace period, then KILL — a doodle pad has no state to lose.
deploy: build/$(BIN)
	$(SSH) '$(KILL_APP); mkdir -p $(DEVICE_DIR)'
	cat build/$(BIN) | $(SSH) 'cat > $(DEVICE_DIR)/$(BIN) && chmod +x $(DEVICE_DIR)/$(BIN)'
	cat app/external.manifest.json | $(SSH) 'cat > $(DEVICE_DIR)/external.manifest.json'
	cat app/icon.png | $(SSH) 'cat > $(DEVICE_DIR)/icon.png'
	@echo
	@echo 'Deployed. On the tablet: open the AppLoad menu -> tap "sample".'
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
	rm -rf build
