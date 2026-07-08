# reader — build & deploy for the reMarkable 2 (takeover app, Rust)
#
# Usual loop:   edit src/*.rs  ->  make deploy  ->  tap the icon on the tablet
#
#   make              cross-compile (cargo + rust-lld; no docker needed)
#   make preview      run under qemu with a FAKE pi -> build/preview*.png
#   make test-host    host-target unit tests (the PNG decoder round-trip)
#   make fetch-server download rm2fb_server (timower/rM2-stuff) into vendor/
#   make deploy       build + push binary/scripts/manifest/server/extension
#
#   make book FILE=~/papers/x.pdf [TITLE="..."] [SLUG=x]   render + push a PDF
#   make books        list the books on the device
#   make book-rm SLUG=x                                    delete one (and its ink!)
#   make mirror-list [ONLY=frag]     browse the tablet's OWN pdf library
#   make mirror ONLY="name frag"     import matching xochitl pdfs as books
#                [FORCE=1] [ALL=1]   (re-render / lift the 10-doc cap)
#
#   make trace        pull pi session traces + app log -> build/trace.html
#   make log          tail the app log on the device (/tmp/reader.log)
#   make log-server   tail the rm2fb server log (/tmp/rm2fb.log)
#   make kill         stop a running takeover session (restores xochitl)
#   make restore-ui   emergency: restart xochitl by hand
#   make uninstall    remove the app from the device
#   make clean        remove build output
#
# One-time setup: rustup target add armv7-unknown-linux-musleabihf
# Requires pi already on the tablet (../pi/pi-harness/install.sh) — reader
# runs `/home/root/bin/pi --mode rpc` in the background with the
# reader-canvas.ts extension (its page tools). Book rendering needs `uv`
# on this machine (pymupdf + numpy are fetched on the fly).

HOST ?= root@192.168.1.7
BIN := reader
TARGET := armv7-unknown-linux-musleabihf
BUILT := target/$(TARGET)/release/$(BIN)
DEVICE_DIR := /home/root/xovi/exthome/appload/reader
BOOKS_DIR := /home/root/.local/share/reader/books
SSH := ssh -o ConnectTimeout=8 $(HOST)
RM2FB_IPK := https://github.com/timower/rM2-stuff/releases/latest/download/rm2display.ipk
UVPY := uv run --with pymupdf --with numpy python3

# The device has pgrep but NO pkill. The [r] bracket stops pgrep -f from
# matching its own ssh command line. Stopping the transient unit first lets
# takeover.sh's trap restore xochitl cleanly.
KILL_APP := systemctl stop reader-takeover 2>/dev/null; \
            q=$$(pgrep -f 'appload/reader/[r]eader'); \
            if [ -n "$$q" ]; then kill $$q; sleep 1; kill -9 $$q 2>/dev/null; fi

.PHONY: all build preview test-host testbook fetch-server deploy book books book-rm mirror mirror-list trace log log-server kill restore-ui restart-ui uninstall clean

all: build

build:
	cargo build --release
	file $(BUILT)

# Host-arch tests (the .cargo config pins builds to armv7, so tests need an
# explicit native target; the PNG decoder test shells out to python3).
test-host:
	cargo test --target $$(rustc -vV | sed -n 's/^host: //p')

# A tiny synthetic 2-page PDF -> a real book bundle, for the preview.
testbook:
	mkdir -p build/testbook
	$(UVPY) test/make-test-pdf.py build/testbook/demo.pdf
	$(UVPY) tools/mkbook.py build/testbook/demo.pdf -o build/testbook/books/demo-paper --title "Demo Paper"

# No tablet needed: qemu runs the real arm binary, a fake pi (test/fake-pi.py)
# plays the RPC other end AND exercises the tool socket (underline, margin
# note, note-page insertion), and test/fake-qtfb.py scripts input +
# screenshots. This exercises the WINDOWED (qtfb) backend; takeover needs
# the device.
preview: build testbook
	mkdir -p build
	chmod +x test/fake-pi.py
	docker build --platform linux/amd64 -t reader-preview -f test/preview.Dockerfile test
	docker run --rm --platform linux/amd64 -v "$(CURDIR):/src" -w /src \
		reader-preview python3 test/fake-qtfb.py $(BUILT) build/preview.png

# The takeover display host: timower/rM2-stuff's rm2fb server. Prefer the
# copy a sibling app already fetched; else download the .ipk.
fetch-server:
	mkdir -p vendor build
	@if [ -f ../notebook/vendor/rm2fb_server ]; then \
		cp ../notebook/vendor/rm2fb_server ../notebook/vendor/librm2fb_server.so vendor/; \
		echo 'copied from ../notebook/vendor'; \
	elif [ -f ../collab/vendor/rm2fb_server ]; then \
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
	cat ext/reader-canvas.ts | $(SSH) 'cat > $(DEVICE_DIR)/reader-canvas.ts'
	cat vendor/rm2fb_server | $(SSH) 'cat > $(DEVICE_DIR)/rm2fb_server && chmod +x $(DEVICE_DIR)/rm2fb_server'
	cat vendor/librm2fb_server.so | $(SSH) 'cat > $(DEVICE_DIR)/librm2fb_server.so'
	cat app/external.manifest.json | $(SSH) 'cat > $(DEVICE_DIR)/external.manifest.json'
	cat app/icon.png | $(SSH) 'cat > $(DEVICE_DIR)/icon.png'
	@echo
	@echo 'Deployed. On the tablet: open the AppLoad menu -> tap "reader".'
	@echo 'It stops xochitl and takes the panel over; top-edge swipe -> CLOSE.'
	@echo 'Push a book with: make book FILE=paper.pdf HOST=$(HOST)'

vendor/rm2fb_server:
	@echo 'vendor/rm2fb_server missing — run: make fetch-server' && exit 1

# Render a PDF into a book bundle and push it. SLUG defaults to the
# filename, lowercased, non-alphanumerics -> dashes.
book:
ifndef FILE
	@echo 'usage: make book FILE=~/papers/x.pdf [TITLE="Nice Title"] [SLUG=x] HOST=root@<ip>' && exit 1
endif
	$(eval SLUG ?= $(shell basename "$(FILE)" .pdf | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9]\{1,\}/-/g; s/^-//; s/-$$//'))
	$(UVPY) tools/mkbook.py "$(FILE)" -o "build/books/$(SLUG)" $(if $(TITLE),--title "$(TITLE)")
	$(SSH) 'mkdir -p $(BOOKS_DIR)'
	tar -C build/books -cf - "$(SLUG)" | $(SSH) 'tar -C $(BOOKS_DIR) -xf -'
	@echo "book '$(SLUG)' pushed — it appears on the reader home screen (ALL BOOKS)."

books:
	@$(SSH) 'for d in $(BOOKS_DIR)/*/; do \
	  [ -f "$$d/meta.json" ] || continue; \
	  slug=$$(basename "$$d"); \
	  title=$$(sed -n "s/.*\"title\": *\"\([^\"]*\)\".*/\1/p" "$$d/meta.json"); \
	  pages=$$(sed -n "s/.*\"pages\": *\([0-9]*\).*/\1/p" "$$d/meta.json"); \
	  echo "$$slug  -  $$title ($$pages pages)"; \
	done' 2>/dev/null || echo '(no books)'

book-rm:
ifndef SLUG
	@echo 'usage: make book-rm SLUG=<slug> HOST=root@<ip>   (see: make books)' && exit 1
endif
	$(SSH) 'rm -rf $(BOOKS_DIR)/$(SLUG)'
	@echo "removed $(SLUG) (pages, text, ink and reading position)."

# Mirror PDFs the tablet ALREADY holds (xochitl's library) into books:
# pull over ssh, render with mkbook, push the bundle back. Selective on
# purpose — mirror what you're reading, not all 300 PDFs.
mirror-list:
	$(UVPY) tools/mirror.py $(HOST) --list $(if $(ONLY),--only "$(ONLY)")

mirror:
	$(UVPY) tools/mirror.py $(HOST) $(if $(ONLY),--only "$(ONLY)") $(if $(FORCE),--force) $(if $(ALL),--all)

# Pull pi's session traces + the app log off the device and render them
# into one HTML (tools/trace-viewer.py): the device log up top, then every
# pause — the page image pi saw, its thinking, per-turn cost — and each
# draw overlaid in red on that page image.
trace:
	mkdir -p build/sessions
	$(SSH) 'tar -C /home/root/.local/share/reader/sessions -cf - .' | tar -C build/sessions -xf -
	$(SSH) 'cat /tmp/reader.log 2>/dev/null' > build/reader.log || true
	python3 tools/trace-viewer.py build/sessions --log build/reader.log -o build/trace.html
	open build/trace.html 2>/dev/null || true

log:
	$(SSH) 'touch /tmp/reader.log; tail -f /tmp/reader.log'

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
	@echo 'Removed $(DEVICE_DIR). Books/ink under ~/.local/share/reader remain.'

clean:
	cargo clean
	rm -rf build
