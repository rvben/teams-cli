.PHONY: build test lint fmt check release install windows-helper clean release-patch release-minor release-major

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

# Cross-builds teams.exe and installs it where WSL looks for the desktop helper.
# Needs cargo-xwin and the x86_64-pc-windows-msvc target.
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
HELPER_DIR := $(or $(XDG_CACHE_HOME),$(HOME)/.cache)/teams-cli/windows-helper/$(VERSION)

# Windows keeps running the old program for a path while a helper started from it is
# alive, so a rebuild installed over it would crash. WSL only; skipped elsewhere.
POWERSHELL := /mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe

windows-helper:
	@if [ -x "$(POWERSHELL)" ] && ! "$(POWERSHELL)" -NoProfile -Command "if (Get-Process teams -ErrorAction SilentlyContinue | Where-Object { \$$_.Path -like '*\teams-cli\windows-helper\$(VERSION)\teams.exe' }) { exit 1 }"; then \
		echo "A teams-cli $(VERSION) desktop helper is still running; close open teams sessions (or stop that teams.exe) and try again." >&2; \
		exit 1; \
	fi
	cargo xwin build --locked --release --target x86_64-pc-windows-msvc
	mkdir -p $(HELPER_DIR)
	install -m 755 target/x86_64-pc-windows-msvc/release/teams.exe $(HELPER_DIR)/teams.exe

clean:
	cargo clean

release-patch:
	vership bump patch

release-minor:
	vership bump minor

release-major:
	vership bump major
