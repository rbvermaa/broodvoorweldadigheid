# Thin wrappers around Nix. Build logic lives in flake.nix.
# Usage: make help
#   make db-up && make test     run the tests against a local PostgreSQL
#   make db-down                stop it (data stays in .pg/)
#   PG_PORT=54332 make db-up    use another port when one is taken

SHELL := bash
HOST_SYSTEM := $(shell nix eval --impure --raw --expr builtins.currentSystem 2>/dev/null)
ifeq ($(HOST_SYSTEM),)
$(error Could not get the Nix system. Is nix on PATH and the daemon running?)
endif
ifeq ($(findstring darwin,$(HOST_SYSTEM)),darwin)
ARCH ?= $(subst darwin,linux,$(HOST_SYSTEM))
else
ARCH ?= $(HOST_SYSTEM)
endif

NIX_DEV := nix develop --command
CHECK = nix build --no-link --print-build-logs .\#checks.$(HOST_SYSTEM).$(1)

PG_DIR := $(CURDIR)/.pg
PG_PORT ?= 54331
DATABASE_URL ?= postgres://postgres@127.0.0.1:$(PG_PORT)/postgres
export DATABASE_URL

.DEFAULT_GOAL := help
.PHONY: help check test fmt fmt-check lint layers unsafe-forbid server db-up db-down

help: ## List targets
	@grep -E '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-14s %s\n", $$1, $$2}'

check: ## Everything CI runs (nix flake check)
	nix flake check --print-build-logs

test: ## Fast test loop in the dev shell (needs make db-up)
	$(NIX_DEV) cargo nextest run --workspace --no-tests=pass

fmt: ## Format the code
	$(NIX_DEV) cargo fmt --all

fmt-check: ## Formatting check only
	$(call CHECK,fmt)

lint: ## Clippy check only
	$(call CHECK,clippy)

layers: ## Crate layer and dependency rules check
	$(call CHECK,layers)

unsafe-forbid: ## Every crate root forbids unsafe code
	$(call CHECK,unsafe-forbid)

server: ## Build the server binary
	nix build --print-build-logs .\#server

db-up: ## Start a local PostgreSQL in .pg/ on PG_PORT (default 54331)
	@mkdir -p $(PG_DIR)/sock
	@if [ ! -f $(PG_DIR)/data/PG_VERSION ]; then \
		$(NIX_DEV) initdb --pgdata=$(PG_DIR)/data --auth=trust --username=postgres \
			--no-instructions --encoding=UTF8 --locale=C >/dev/null; \
	fi
	$(NIX_DEV) pg_ctl --pgdata=$(PG_DIR)/data --wait --log=$(PG_DIR)/postgres.log \
		-o "-c listen_addresses=127.0.0.1 -c port=$(PG_PORT) -c unix_socket_directories=$(PG_DIR)/sock -c max_connections=200" start
	@echo "DATABASE_URL=$(DATABASE_URL)"

db-down: ## Stop the local PostgreSQL
	$(NIX_DEV) pg_ctl --pgdata=$(PG_DIR)/data --wait --mode=fast stop
