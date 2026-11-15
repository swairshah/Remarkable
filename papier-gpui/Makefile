BIN := target/release/papier

.PHONY: build run dev test install fullscreen

build:
	cargo build --release

run: build
	$(BIN)

fullscreen: build
	PAPIER_FULLSCREEN=1 $(BIN)

dev:
	cargo run

test:
	cargo test

# Symlinks, so the launcher always runs the latest `make build`.
install: build
	mkdir -p $(HOME)/.local/bin $(HOME)/.local/share/applications $(HOME)/.local/share/icons/hicolor/512x512/apps
	ln -sf $(CURDIR)/$(BIN) $(HOME)/.local/bin/papier
	ln -sf $(CURDIR)/assets/papier.desktop $(HOME)/.local/share/applications/papier.desktop
	ln -sf $(CURDIR)/assets/icon.png $(HOME)/.local/share/icons/hicolor/512x512/apps/papier.png
	-update-desktop-database $(HOME)/.local/share/applications 2>/dev/null
	-gtk-update-icon-cache $(HOME)/.local/share/icons/hicolor 2>/dev/null
