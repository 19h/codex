# Convenience targets for building the Cargo workspace in codex-rs/.
#
# The v8 crate is built with the sandbox enabled, and upstream rusty_v8 does not
# publish prebuilt archives for that configuration. `make v8` downloads the
# Codex-built ones (the same artifacts CI uses), verifies them against the
# checksums in third_party/v8/, and caches them outside target/ so `make clean`
# keeps them. Every Cargo target points the v8 build script at that cache.
#
# codex-voice-host links the system GStreamer (>= 1.28) and is skipped unless
# pkg-config can find it.
#
#   make build                     # debug build of the workspace
#   make release                   # optimized build
#   make build PKG=codex-tui       # build specific packages instead
#   make test PKG="codex-core codex-tui"
#   make run ARGS="exec 'say hi'"  # run the codex CLI
#   make install                   # release build into ~/local/bin
#   make install VERSION=0.164.0   # override the local build version

ROOT        := $(patsubst %/,%,$(dir $(abspath $(lastword $(MAKEFILE_LIST)))))
CARGO_DIR   := $(ROOT)/codex-rs
TARGET_DIR  := $(or $(CARGO_TARGET_DIR),$(CARGO_DIR)/target)
CARGO       ?= cargo
CARGO_FLAGS ?=
VERSION     ?= 0.163.0
PYTHON      ?= python3
ARGS        ?=
PKG         ?=
PREFIX      ?= $(HOME)/local
BINDIR      ?= $(PREFIX)/bin

# codex looks for codex-code-mode-host next to its own executable when it is
# not running from a packaged install, so the two are installed side by side.
INSTALL_BINS := codex codex-code-mode-host

CARGO_CMD   := cd "$(CARGO_DIR)" && $(CARGO)
HOST_TRIPLE ?= $(shell cd "$(CARGO_DIR)" && rustc -vV | sed -n 's/^host: //p')
SHA256      := $(if $(shell command -v sha256sum 2>/dev/null),sha256sum,shasum -a 256)

ifeq ($(shell pkg-config --exists 'gstreamer-1.0 >= 1.28' 2>/dev/null && echo yes),yes)
WORKSPACE := --workspace
else
WORKSPACE := --workspace --exclude codex-voice-host
endif
SCOPE := $(if $(PKG),$(foreach p,$(PKG),-p $(p)),$(WORKSPACE))

V8_VERSION        := $(shell awk '/^name = "v8"$$/ { getline; gsub(/"/, "", $$3); print $$3 }' "$(CARGO_DIR)/Cargo.lock")
V8_PROFILE        := ptrcomp_sandbox_release
V8_CACHE          ?= $(or $(XDG_CACHE_HOME),$(HOME)/.cache)/codex-rusty-v8
V8_DIR            := $(V8_CACHE)/$(V8_VERSION)/$(HOST_TRIPLE)
V8_STAMP          := $(V8_DIR)/.verified
V8_URL            := https://github.com/openai/codex/releases/download/rusty-v8-v$(V8_VERSION)
V8_TRUSTED        := $(ROOT)/third_party/v8/rusty_v8_$(subst .,_,$(V8_VERSION))_release_manifests.sha256
V8_ARCHIVE_NAME   := librusty_v8_$(V8_PROFILE)_$(HOST_TRIPLE).a.gz
V8_BINDING_NAME   := src_binding_$(V8_PROFILE)_$(HOST_TRIPLE).rs
V8_CHECKSUMS_NAME := rusty_v8_$(V8_PROFILE)_$(HOST_TRIPLE).sha256

export RUSTY_V8_ARCHIVE          := $(V8_DIR)/$(V8_ARCHIVE_NAME)
export RUSTY_V8_SRC_BINDING_PATH := $(V8_DIR)/$(V8_BINDING_NAME)

.DEFAULT_GOAL := help
.PHONY: help version build release codex run install uninstall check test clippy fix fmt fmt-check clean clean-v8 distclean v8

help: ## Show this help
	@echo "Usage: make <target> [VERSION=$(VERSION)] [PKG=<crate>...] [CARGO_FLAGS=...] [ARGS=...]"
	@echo
	@awk 'BEGIN { FS = ":.*## " } /^[a-z0-9-]+:.*## / { printf "  %-10s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

# Cargo supplies CARGO_PKG_VERSION itself; exporting an environment variable
# cannot override it. Stamp the shared manifest before any compilation. Cargo
# updates the workspace entries in Cargo.lock to match during the build.
version: ## Set the workspace version (default 0.163.0; edits Cargo.toml)
	$(PYTHON) "$(ROOT)/scripts/set-build-version.py" "$(CARGO_DIR)/Cargo.toml" "$(VERSION)"

build release codex run install check test clippy fix: version

build: v8 ## Debug build of the workspace (or PKG=...)
	$(CARGO_CMD) build $(SCOPE) $(CARGO_FLAGS)

release: v8 ## Optimized build of the workspace (or PKG=...)
	$(CARGO_CMD) build --release $(SCOPE) $(CARGO_FLAGS)

codex: v8 ## Build only the codex CLI binary
	$(CARGO_CMD) build --bin codex $(CARGO_FLAGS)

run: v8 ## Run the codex CLI with ARGS
	$(CARGO_CMD) run --bin codex $(CARGO_FLAGS) -- $(ARGS)

install: v8 ## Release-build codex and copy it to BINDIR (~/local/bin)
	$(CARGO_CMD) build --release $(foreach b,$(INSTALL_BINS),--bin $(b)) $(CARGO_FLAGS)
	mkdir -p "$(BINDIR)"
	install -m 755 $(foreach b,$(INSTALL_BINS),"$(TARGET_DIR)/release/$(b)") "$(BINDIR)/"

uninstall: ## Remove the installed codex binaries from BINDIR
	rm -f $(foreach b,$(INSTALL_BINS),"$(BINDIR)/$(b)")

check: v8 ## Type-check code and tests without building binaries
	$(CARGO_CMD) check --tests $(SCOPE) $(CARGO_FLAGS)

# Mirrors `just test`; falls back to `cargo test` without cargo-nextest.
test: v8 ## Run tests (uses cargo-nextest when installed)
	@set -e; cd "$(CARGO_DIR)"; \
	export RUST_MIN_STACK=8388608 NEXTEST_PROFILE=local; \
	if $(CARGO) nextest --version >/dev/null 2>&1; then \
	  set -x; $(CARGO) nextest run --no-fail-fast $(SCOPE) $(CARGO_FLAGS); \
	else \
	  set -x; $(CARGO) test --no-fail-fast $(SCOPE) $(CARGO_FLAGS); \
	fi

clippy: v8 ## Lint with clippy
	$(CARGO_CMD) clippy --tests $(SCOPE) $(CARGO_FLAGS)

fix: v8 ## Apply clippy fixes (allows a dirty tree)
	$(CARGO_CMD) clippy --fix --tests --allow-dirty $(SCOPE) $(CARGO_FLAGS)

fmt: ## Format Rust sources
	$(CARGO_CMD) fmt

fmt-check: ## Check Rust formatting
	$(CARGO_CMD) fmt --check

clean: ## Remove Cargo build output (keeps the V8 cache)
	$(CARGO_CMD) clean

clean-v8: ## Remove the cached V8 artifacts
	rm -rf "$(V8_CACHE)"

distclean: clean clean-v8 ## clean + clean-v8

v8: $(V8_STAMP) ## Download and verify the Codex-built rusty_v8 artifacts

# Download into a scratch directory and only move it into place once every
# checksum matches, so an interrupted fetch never looks complete.
$(V8_STAMP):
	@echo "Fetching Codex-built rusty_v8 $(V8_VERSION) for $(HOST_TRIPLE)"
	@set -e; \
	tmp="$(V8_DIR).partial"; \
	rm -rf "$$tmp"; mkdir -p "$$tmp"; \
	expected="$$(grep -F "  $(V8_CHECKSUMS_NAME)" "$(V8_TRUSTED)" | cut -d ' ' -f 1)"; \
	if [ -z "$$expected" ]; then \
	  echo "error: no trusted rusty_v8 checksums for $(HOST_TRIPLE) in $(V8_TRUSTED)" >&2; exit 1; \
	fi; \
	curl -fsSL "$(V8_URL)/$(V8_CHECKSUMS_NAME)" -o "$$tmp/$(V8_CHECKSUMS_NAME)"; \
	actual="$$($(SHA256) "$$tmp/$(V8_CHECKSUMS_NAME)" | cut -d ' ' -f 1)"; \
	if [ "$$expected" != "$$actual" ]; then \
	  echo "error: $(V8_CHECKSUMS_NAME) does not match $(V8_TRUSTED)" >&2; exit 1; \
	fi; \
	curl -fsSL "$(V8_URL)/$(V8_ARCHIVE_NAME)" -o "$$tmp/$(V8_ARCHIVE_NAME)"; \
	curl -fsSL "$(V8_URL)/$(V8_BINDING_NAME)" -o "$$tmp/$(V8_BINDING_NAME)"; \
	(cd "$$tmp" && tr -d '\r' < "$(V8_CHECKSUMS_NAME)" | $(SHA256) -c -); \
	rm -rf "$(V8_DIR)"; mv "$$tmp" "$(V8_DIR)"; touch "$(V8_STAMP)"
