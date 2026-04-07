.PHONY: dev build lint fmt install

dev:
	RUST_LOG=debug cargo run

build:
	cargo build --release

lint:
	cargo clippy -- -D warnings
	cargo fmt -- --check

fmt:
	cargo fmt

install: build
	install -Dm755 target/release/gemini-lite ~/.local/bin/gemini-lite
	install -Dm644 assets/Logo.png ~/.local/share/icons/hicolor/512x512/apps/gemini-lite.png
	mkdir -p ~/.local/share/applications
	sed "s|Exec=gemini-lite|Exec=$$HOME/.local/bin/gemini-lite|g" gemini-lite.desktop > ~/.local/share/applications/gemini-lite.desktop
	chmod 644 ~/.local/share/applications/gemini-lite.desktop
	update-desktop-database ~/.local/share/applications || true
	gtk-update-icon-cache ~/.local/share/icons/hicolor || true
