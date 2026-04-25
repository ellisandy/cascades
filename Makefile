# Cascades — top-level orchestration.
#
# This Makefile is a thin wrapper around `cargo`, `bun`, and the Pi
# installer scripts. Day-to-day development still uses cargo / bun
# directly; the Makefile exists so that "deploying to a Pi" is a
# discoverable, single-command flow.
#
# Common targets:
#   make build              - cargo build --release (the binary the installer needs)
#   make install            - sudo ./scripts/install.sh   (Pi only)
#   make uninstall          - sudo ./scripts/uninstall.sh
#   make uninstall PURGE=1  - also wipes /opt/cascades + the cascades user
#   make status             - systemctl status for all three services
#   make logs               - tail -f for all three services
#   make test               - cargo test
#   make help               - show this list

.DEFAULT_GOAL := help

# Marker so `make install PURGE=1` works without quoting headaches.
PURGE ?= 0

# ─── Build ────────────────────────────────────────────────────────────────

.PHONY: build
build: ## Build the release binary (the one the installer copies to the Pi)
	@command -v cargo >/dev/null 2>&1 || { \
		echo "cargo not found. Install rustup from https://rustup.rs first."; \
		exit 1; \
	}
	cargo build --release
	@printf "\n✓ Binary: target/release/cascades\n"

.PHONY: build-arm64
build-arm64: ## Cross-compile for aarch64-linux (Pi) — needs `cross` + Docker
	@command -v cross >/dev/null 2>&1 || { \
		echo "cross not found."; \
		echo "  Apple Silicon: cargo install cross --git https://github.com/cross-rs/cross --locked"; \
		echo "                 (the crates.io 0.2.5 release is broken on aarch64 hosts —"; \
		echo "                  see https://github.com/cross-rs/cross/issues/1628)"; \
		echo "  x86_64 Linux:  cargo install cross --locked"; \
		exit 1; \
	}
	@docker info >/dev/null 2>&1 || { \
		echo "Docker daemon not running. Start Docker Desktop / colima / podman first."; \
		exit 1; \
	}
	cross build --release --target aarch64-unknown-linux-gnu
	@printf "\n✓ ARM64 binary: target/aarch64-unknown-linux-gnu/release/cascades\n"
	@printf "  Ship to the Pi:\n"
	@printf "    scp target/aarch64-unknown-linux-gnu/release/cascades \\\\\n"
	@printf "        jackellis@<pi-host>:/srv/cascades/target/release/cascades\n"
	@printf "  Then on the Pi: sudo ./scripts/install.sh\n"

# ─── Install / uninstall ─────────────────────────────────────────────────

.PHONY: install
install: build ## Build + run the Pi installer (idempotent)
	@if [ "$$(id -u)" -eq 0 ]; then \
		./scripts/install.sh; \
	else \
		sudo ./scripts/install.sh; \
	fi

.PHONY: uninstall
uninstall: ## Stop services + remove binaries (preserves data unless PURGE=1)
	@if [ "$(PURGE)" = "1" ]; then \
		ARGS="--purge"; \
	else \
		ARGS=""; \
	fi; \
	if [ "$$(id -u)" -eq 0 ]; then \
		./scripts/uninstall.sh $$ARGS; \
	else \
		sudo ./scripts/uninstall.sh $$ARGS; \
	fi

# ─── Operations ──────────────────────────────────────────────────────────

.PHONY: status
status: ## systemctl status for both Cascades services
	@for u in cascades-sidecar cascades; do \
		printf "\n\033[1;36m▶ %s\033[0m\n" "$$u"; \
		systemctl --no-pager status $$u 2>/dev/null || echo "(not installed)"; \
	done

.PHONY: logs
logs: ## Tail -f both service logs (Ctrl+C to exit)
	journalctl -f -u cascades -u cascades-sidecar

.PHONY: restart
restart: ## Restart both services in dependency order
	sudo systemctl restart cascades-sidecar
	sudo systemctl restart cascades

# ─── Dev ─────────────────────────────────────────────────────────────────

.PHONY: test
test: ## cargo test (lib + integration)
	cargo test --lib --tests

.PHONY: clippy
clippy: ## cargo clippy
	cargo clippy --lib --tests

.PHONY: dev
dev: ## Run the dev server in fixture mode (no live API calls)
	./scripts/dev-server.sh

# ─── Help ────────────────────────────────────────────────────────────────

.PHONY: help
help: ## Show this help
	@printf "\n\033[1mCascades — make targets\033[0m\n\n"
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z0-9_-]+:.*?## / { \
		printf "  \033[1;32m%-22s\033[0m %s\n", $$1, $$2 \
	}' $(MAKEFILE_LIST)
	@printf "\n"
