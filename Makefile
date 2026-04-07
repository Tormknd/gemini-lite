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
	install -Dm644 gemini-lite.desktop ~/.local/share/applications/gemini-lite.desktop
	update-desktop-database ~/.local/share/applications || true
