.DEFAULT_GOAL := help
.PHONY: help build build-release build-all test fmt fmt-check lint lint-nursery audit ci clean install \
	build-linux-x86_64 build-linux-aarch64 \
	build-macos-x86_64 build-macos-aarch64 \
	build-windows-x86_64

# ---------------------------------------------------------------------------
# Development
# ---------------------------------------------------------------------------

help: ## Show this help
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2}'

build: ## Build debug binary for host platform
	cargo build

build-release: ## Build release binary for host platform
	cargo build --release

install: ## Install binary to ~/.cargo/bin
	cargo install --path .

clean: ## Remove build artifacts
	cargo clean

# ---------------------------------------------------------------------------
# Quality
# ---------------------------------------------------------------------------

test: ## Run all tests
	cargo test

fmt: ## Format code
	cargo fmt

fmt-check: ## Check formatting (CI)
	cargo fmt --check

lint: ## Run clippy lints (pedantic + all deny, matches CLAUDE.md quality bar)
	cargo clippy --all-targets --all-features -- \
		-D warnings \
		-D clippy::all \
		-D clippy::pedantic

lint-nursery: ## Run clippy nursery lints (warn-only, advisory)
	cargo clippy --all-targets --all-features -- -W clippy::nursery

audit: ## Audit dependencies for known vulnerabilities
	cargo audit

ci: fmt-check lint lint-nursery audit test ## Run full CI pipeline (fmt-check + lint + nursery + audit + test)

# ---------------------------------------------------------------------------
# Cross-compilation (release builds)
# ---------------------------------------------------------------------------

build-linux-x86_64: ## Build release for x86_64-unknown-linux-musl
	cargo build --release --target x86_64-unknown-linux-musl

build-linux-aarch64: ## Build release for aarch64-unknown-linux-musl
	cargo build --release --target aarch64-unknown-linux-musl

build-macos-x86_64: ## Build release for x86_64-apple-darwin
	cargo build --release --target x86_64-apple-darwin

build-macos-aarch64: ## Build release for aarch64-apple-darwin
	cargo build --release --target aarch64-apple-darwin

build-windows-x86_64: ## Build release for x86_64-pc-windows-msvc
	cargo build --release --target x86_64-pc-windows-msvc

build-all: build-linux-x86_64 build-linux-aarch64 build-macos-x86_64 build-macos-aarch64 build-windows-x86_64 ## Build release for all platforms
