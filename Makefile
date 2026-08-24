.PHONY: build test lint fmt check release install clean

build:
	cargo build --locked

test:
	cargo test --locked --all-targets

lint:
	cargo fmt -- --check
	cargo clippy --locked --all-targets -- -D warnings

fmt:
	cargo fmt

check: lint test

release:
	cargo build --locked --release

install: check release
	cp target/release/teams ~/.local/bin/teams

clean:
	cargo clean
