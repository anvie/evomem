.PHONY: build build-linux build-linux-gnu check test clippy fmt clean

CARGO := $(HOME)/.cargo/bin/cargo
SHELL := /bin/bash

# Ensure zig and cargo are on PATH
export PATH := /opt/homebrew/bin:$(HOME)/.cargo/bin:/usr/bin:/bin

# Default: native release build
build:
	$(CARGO) build --release

# Cross-compile: musl (fully static, no runtime deps)
build-linux:
	$(CARGO) zigbuild --release --target x86_64-unknown-linux-musl

# Cross-compile: glibc (requires glibc on target)
build-linux-gnu:
	$(CARGO) zigbuild --release --target x86_64-unknown-linux-gnu

check:
	$(CARGO) check

test:
	$(CARGO) test

clippy:
	$(CARGO) clippy -- -D warnings

fmt:
	$(CARGO) fmt --check

fmt-fix:
	$(CARGO) fmt

clean:
	$(CARGO) clean





