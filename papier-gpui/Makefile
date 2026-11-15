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

install: build
	install -Dm755 $(BIN) $(HOME)/.local/bin/papier
	install -Dm644 assets/papier.desktop $(HOME)/.local/share/applications/papier.desktop
