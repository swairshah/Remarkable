# Paper — build & deploy for the reMarkable 2 (takeover app, Rust)
#
# A pixel-faithful replication of the stock reMarkable UI with pi (AI)
# integrated: home document grid, status bar, right toolbar, lasso select,
# undo/redo — notebooks and books unified.
#
# Usual loop:   edit src/*.rs  ->  make deploy  ->  tap the icon on the tablet
#
#   make              cross-compile (cargo + rust-lld; no docker needed)
#   make preview      run under qemu with a FAKE pi -> build/preview*.png
#   make test-host    host-target unit tests (the PNG decoder round-trip)
#   make fetch-server download rm2fb_server (timower/rM2-stuff) into vendor/
#   make deploy       build + push binary/scripts/manifest/server/extension
#
#   make book FILE=~/papers/x.pdf [TITLE="..."] [SLUG=x] [MARGIN=90]  render + push
#   make mirror-list [ONLY=frag]     browse the tablet's OWN pdf library
#   make mirror ONLY="name frag"     import matching xochitl pdfs as docs
#
#   make fonts        extract xochitl's UI fonts from the device -> build/fonts/
#   make ref          capture xochitl reference screenshots -> build/ref/
#   make adopt        dry-run the legacy reader/notebook data import on-device
#
#   make trace        pull pi session traces + app log -> build/trace.html
#   make log          tail the app log on the device (/tmp/paper.log)
#   make log-server   tail the rm2fb server log (/tmp/rm2fb.log)
#   make kill         stop a running takeover session (restores xochitl)
#   make restore-ui   emergency: restart xochitl by hand
#   make uninstall    remove the app from the device
#   make clean        remove build output
#
# One-time setup: rustup target add armv7-unknown-linux-musleabihf
# Requires pi already on the tablet (../pi/pi-harness/install.sh).

HOST ?= root@192.168.1.2
BIN := paper
TARGET := armv7-unknown-linux-musleabihf
BUILT := target/$(TARGET)/release/$(BIN)
DEVICE_DIR := /home/root/xovi/exthome/appload/alt-ui
DATA_DIR := /home/root/.local/share/alt-ui
DOCS_DIR := $(DATA_DIR)/docs
SSH := ssh -o ConnectTimeout=8 $(HOST)
RM2FB_IPK := https://github.com/timower/rM2-stuff/releases/latest/download/rm2display.ipk
UVPY := uv run --with pymupdf --with numpy python3

# The device has pgrep but NO pkill. The [a] bracket stops pgrep -f from
# matching its own ssh command line. Stopping the transient unit first lets
# takeover.sh's trap restore xochitl cleanly.
KILL_APP := systemctl stop paper-takeover 2>/dev/null; \
            q=$$(pgrep -f 'appload/alt-ui/[p]aper'); \
            if [ -n "$$q" ]; then kill $$q; sleep 1; kill -9 $$q 2>/dev/null; fi

.PHONY: all build preview test-host testbook fetch-server deploy book book-rm mirror mirror-list fonts ref adopt trace log log-server kill restore-ui restart-ui uninstall clean

all: build

build:
	cargo build --release
	file $(BUILT)

# Host-arch tests (the .cargo config pins builds to armv7, so tests need an
# explicit native target). The PNG decoder test moved to libreink-core with
# the extraction; run the shared crates' suite too so this stays one command.
test-host:
	cargo test --target $$(rustc -vV | sed -n 's/^host: //p')
	cd ../../libreink && cargo test

# A tiny synthetic 2-page PDF -> a real book bundle, for the preview.
testbook:
	mkdir -p build/testbook
	$(UVPY) test/make-test-pdf.py build/testbook/demo.pdf
	$(UVPY) tools/mkbook.py build/testbook/demo.pdf -o build/testbook/docs/demo-paper --title "Demo Paper"

# No tablet needed: qemu runs the real arm binary, a fake pi (test/fake-pi.py)
# plays the RPC other end AND exercises the tool socket, and
# test/fake-qtfb.py scripts input + screenshots. Windowed (qtfb) backend
# only; takeover needs the device. SCENARIO picks the scripted session.
preview: build testbook
	mkdir -p build
	chmod +x test/fake-pi.py
	docker build --platform linux/arm64 -t paper-preview -f test/preview.Dockerfile test
	docker run --rm --platform linux/arm64 -v "$(CURDIR):/src" \
		-v "$(abspath $(CURDIR)/../../libreink):/libreink:ro" -e LIBREINK_PREVIEW=/libreink/tools/preview -w /src \
		-e PAPER_SCENARIO=$(or $(SCENARIO),m1) \
		paper-preview python3 test/fake-qtfb.py $(BUILT) build/preview.png

# The takeover display host: timower/rM2-stuff's rm2fb server. Prefer the
# copy a sibling app already fetched; else download the .ipk.
fetch-server:
	mkdir -p vendor build
	@if [ -f ../reader/vendor/rm2fb_server ]; then \
		cp ../reader/vendor/rm2fb_server ../reader/vendor/librm2fb_server.so vendor/; \
		echo 'copied from ../reader/vendor'; \
	elif [ -f ../notebook/vendor/rm2fb_server ]; then \
		cp ../notebook/vendor/rm2fb_server ../notebook/vendor/librm2fb_server.so vendor/; \
		echo 'copied from ../notebook/vendor'; \
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
	$(SSH) '$(KILL_APP); mkdir -p $(DEVICE_DIR); rm -f $(DEVICE_DIR)/alt-ui $(DEVICE_DIR)/alt-ui-canvas.ts'
	cat $(BUILT) | $(SSH) 'cat > $(DEVICE_DIR)/$(BIN) && chmod +x $(DEVICE_DIR)/$(BIN)'
	cat scripts/appload-launch.sh | $(SSH) 'cat > $(DEVICE_DIR)/appload-launch.sh && chmod +x $(DEVICE_DIR)/appload-launch.sh'
	cat scripts/takeover.sh | $(SSH) 'cat > $(DEVICE_DIR)/takeover.sh && chmod +x $(DEVICE_DIR)/takeover.sh'
	cat ext/paper-canvas.ts | $(SSH) 'cat > $(DEVICE_DIR)/paper-canvas.ts'
	cat ext/paper-metrics.ts | $(SSH) 'cat > $(DEVICE_DIR)/paper-metrics.ts'
	cat ext/paper-transcribe.ts | $(SSH) 'cat > $(DEVICE_DIR)/paper-transcribe.ts'
	cat vendor/rm2fb_server | $(SSH) 'cat > $(DEVICE_DIR)/rm2fb_server && chmod +x $(DEVICE_DIR)/rm2fb_server'
	cat vendor/librm2fb_server.so | $(SSH) 'cat > $(DEVICE_DIR)/librm2fb_server.so'
	cat app/external.manifest.json | $(SSH) 'cat > $(DEVICE_DIR)/external.manifest.json'
	cat app/icon.png | $(SSH) 'cat > $(DEVICE_DIR)/icon.png'
	@if [ -d build/fonts ]; then \
		$(SSH) 'mkdir -p $(DEVICE_DIR)/fonts'; \
		for f in build/fonts/*.ttf; do \
			cat $$f | $(SSH) "cat > $(DEVICE_DIR)/fonts/$$(basename $$f)"; \
		done; \
		echo 'pushed extracted UI fonts'; \
	fi
	@echo
	@echo 'Deployed. On the tablet: open the AppLoad menu -> tap "Paper".'
	@echo 'It stops xochitl and takes the panel over; top-edge swipe -> CLOSE.'

vendor/rm2fb_server:
	@echo 'vendor/rm2fb_server missing — run: make fetch-server' && exit 1

# Render a PDF into a book bundle and push it. SLUG defaults to the
# filename, lowercased, non-alphanumerics -> dashes.
book:
ifndef FILE
	@echo 'usage: make book FILE=~/papers/x.pdf [TITLE="Nice Title"] [SLUG=x] HOST=root@<ip>' && exit 1
endif
	$(eval SLUG ?= $(shell basename "$(FILE)" .pdf | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9]\{1,\}/-/g; s/^-//; s/-$$//'))
	$(UVPY) tools/mkbook.py "$(FILE)" -o "build/docs/$(SLUG)" $(if $(TITLE),--title "$(TITLE)") $(if $(MARGIN),--margin $(MARGIN)) $(if $(MARGIN_LEFT),--margin-left $(MARGIN_LEFT)) $(if $(MARGIN_TOP),--margin-top $(MARGIN_TOP)) $(if $(MARGIN_RIGHT),--margin-right $(MARGIN_RIGHT)) $(if $(MARGIN_BOTTOM),--margin-bottom $(MARGIN_BOTTOM))
	$(SSH) 'mkdir -p $(DOCS_DIR)'
	tar -C build/docs -cf - "$(SLUG)" | $(SSH) 'tar -C $(DOCS_DIR) -xf -'
	@echo "doc '$(SLUG)' pushed — it appears on the Paper home grid."

book-rm:
ifndef SLUG
	@echo 'usage: make book-rm SLUG=<slug> HOST=root@<ip>' && exit 1
endif
	$(SSH) 'rm -rf $(DOCS_DIR)/$(SLUG)'
	@echo "removed $(SLUG) (pages, text, ink and reading position)."

# Mirror PDFs the tablet ALREADY holds (xochitl's library) into docs.
mirror-list:
	$(UVPY) tools/mirror.py $(HOST) --list $(if $(ONLY),--only "$(ONLY)")

mirror:
	$(UVPY) tools/mirror.py $(HOST) $(if $(ONLY),--only "$(ONLY)") $(if $(FORCE),--force) $(if $(ALL),--all) $(if $(MARGIN),--margin $(MARGIN)) $(if $(MARGIN_LEFT),--margin-left $(MARGIN_LEFT)) $(if $(MARGIN_TOP),--margin-top $(MARGIN_TOP)) $(if $(MARGIN_RIGHT),--margin-right $(MARGIN_RIGHT)) $(if $(MARGIN_BOTTOM),--margin-bottom $(MARGIN_BOTTOM))

# ---- pixel-fidelity tooling (M6) ------------------------------------------

# Extract the UI fonts (reMarkableSans etc.) embedded in /usr/bin/xochitl.
# NEVER committed — per-machine, per-device.
fonts:
	mkdir -p build/fonts
	python3 tools/extract-fonts.py $(HOST) -o build/fonts

# Capture reference screenshots of the REAL xochitl UI (reSnap technique).
ref:
	mkdir -p build/ref
	python3 tools/xsnap.py $(HOST) -o build/ref

# Dry-run the legacy import (reader books + notebook pages -> paper docs).
adopt:
	cat $(BUILT) | $(SSH) 'cat > /tmp/paper-import && chmod +x /tmp/paper-import'
	$(SSH) 'PAPER_IMPORT_ONLY=1 /tmp/paper-import'

# ---- device utilities ------------------------------------------------------

trace:
	mkdir -p build/sessions
	rm -f build/sessions/*.jsonl   # mirror the device exactly — no stale sessions
	$(SSH) 'tar -C $(DATA_DIR)/sessions -cf - .' | tar -C build/sessions -xf -
	$(SSH) 'cat /tmp/paper.log 2>/dev/null' > build/paper.log || true
	$(SSH) 'cat $(DATA_DIR)/metrics.jsonl 2>/dev/null' > build/metrics.jsonl || true
	python3 tools/trace-viewer.py build/sessions --log build/paper.log --metrics build/metrics.jsonl -o build/trace.html
	open build/trace.html 2>/dev/null || true

log:
	$(SSH) 'touch /tmp/paper.log; tail -f /tmp/paper.log'

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
	@echo 'Removed $(DEVICE_DIR). Docs/ink under $(DATA_DIR) remain.'

clean:
	cargo clean
	rm -rf build
