CARGO := $(HOME)/.cargo/bin/cargo
SHELL := /bin/bash

# Ensure zig and cargo are on PATH
export PATH := /opt/homebrew/bin:$(HOME)/.cargo/bin:/usr/bin:/bin

# Derive version from Cargo.toml
VERSION := $(shell grep '^version' Cargo.toml | sed 's/version = "\(.*\)"/\1/')

# Distribution targets
DIST_TARGETS := x86_64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin
DIST_DIR     := dist

.PHONY: build build-linux-musl build-linux-gnu build-darwin-arm build-darwin-x64
.PHONY: dist check test clippy fmt fmt-fix clean

# ---------------------------------------------------------------------------
# Build targets
# ---------------------------------------------------------------------------

# Default: native release build
build:
	$(CARGO) build --release

# Cross-compile: musl (fully static, no runtime deps)
build-linux-musl:
	$(CARGO) zigbuild --release --target x86_64-unknown-linux-musl

# Cross-compile: glibc (requires glibc on target)
build-linux-gnu:
	$(CARGO) zigbuild --release --target x86_64-unknown-linux-gnu

# Cross-compile: Apple Silicon
build-darwin-arm:
	$(CARGO) zigbuild --release --target aarch64-apple-darwin

# Cross-compile: Intel Mac
build-darwin-x64:
	$(CARGO) zigbuild --release --target x86_64-apple-darwin

# ---------------------------------------------------------------------------
# Distribution: build all targets and produce per-platform zip archives
# ---------------------------------------------------------------------------

dist: build-linux-musl build-darwin-arm build-darwin-x64
	@echo "Assembling distribution packages..."
	@mkdir -p $(DIST_DIR)
	@for target in $(DIST_TARGETS); do \
		bin="target/$$target/release/evomem"; \
		name="evomem-$(VERSION)-$$target"; \
		cp "$$bin" "evomem"; \
		zip "$(DIST_DIR)/$$name.zip" evomem; \
		rm evomem; \
	done
	@echo ""
	@echo "=== Distribution packages in $(DIST_DIR)/ ==="
	@ls -lh $(DIST_DIR)/

# ---------------------------------------------------------------------------
# Quality
# ---------------------------------------------------------------------------

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





