# Thin wrappers around Nix. Build logic lives in flake.nix.
# Usage: make help
#   make db-up && make test     run the tests against a local PostgreSQL
#   make db-down                stop it (data stays in .pg/)
#   PG_PORT=54332 make db-up    use another port when one is taken
#   make images                 build every image for ARCH and docker load it
#   make image-server LINUX_STORE=ssh-ng://dev2   build one image on another Linux host

SHELL := bash
HOST_SYSTEM := $(shell nix eval --impure --raw --expr builtins.currentSystem 2>/dev/null)
ifeq ($(HOST_SYSTEM),)
$(error Could not get the Nix system. Is nix on PATH and the daemon running?)
endif
# On macOS, images build on a remote Linux store and run in Docker Desktop under x86_64
# emulation (ADR 0002).
ifeq ($(findstring darwin,$(HOST_SYSTEM)),darwin)
ARCH ?= x86_64-linux
LINUX_STORE ?= ssh-ng://dev1
else
ARCH ?= $(HOST_SYSTEM)
LINUX_STORE ?=
endif

NIX_DEV := nix develop --command
CHECK = nix build --no-link --print-build-logs .\#checks.$(HOST_SYSTEM).$(1)

IMAGES := server postgres kanidm caddy backup
IMAGE_BUILD = nix build --no-link --print-out-paths --print-build-logs \
	$(if $(LINUX_STORE),--eval-store auto --store $(LINUX_STORE)) .\#packages.$(ARCH).$(1)-image
IMAGE_CAT = $(if $(LINUX_STORE),nix store cat --store $(LINUX_STORE),cat)

PG_DIR := $(CURDIR)/.pg
PG_PORT ?= 54331
DATABASE_URL ?= postgres://postgres@127.0.0.1:$(PG_PORT)/postgres
export DATABASE_URL

.DEFAULT_GOAL := help
.PHONY: help check test fmt fmt-check lint layers unsafe-forbid deny audit coverage server db-up db-down \
	images $(addprefix image-,$(IMAGES))

help: ## List targets
	@grep -E '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-14s %s\n", $$1, $$2}'
	@echo "  image-NAME     Build one image and docker load it; NAME is one of: $(IMAGES)"
	@echo
	@echo "Variables:"
	@echo "  ARCH=$(ARCH)  system the images are built for"
	@echo "  LINUX_STORE=$(LINUX_STORE)  Nix store that builds the images (empty: local)"

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

deny: ## Licence, ban and source checks (cargo-deny)
	$(call CHECK,deny)

audit: ## Known-vulnerability check (cargo-audit)
	$(call CHECK,audit)

coverage: ## Line coverage gate (80% overall, 95% in strict crates)
	$(call CHECK,coverage)

server: ## Build the server binary
	nix build --print-build-logs .\#server

images: $(addprefix image-,$(IMAGES)) ## Build every image for ARCH and docker load it

$(addprefix image-,$(IMAGES)): image-%:
	@out=$$($(call IMAGE_BUILD,$*)) && echo "loading $$out" && $(IMAGE_CAT) $$out | docker load

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
