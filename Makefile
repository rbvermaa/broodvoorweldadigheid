# Thin wrappers around Nix. Build logic lives in flake.nix.
# Usage: make help
#   make db-up && make test     run the tests against a local PostgreSQL
#   make db-down                stop it (data stays in .pg/)
#   PG_PORT=54332 make db-up    use another port when one is taken
#   make migrations-lock        record new migrations; a db test refuses edited or unlisted ones
#   make up / make down         run the Compose stack in deploy/ (needs deploy/secrets, see deploy/README.md)
#   make smoke                  throwaway stack: health, Kanidm, backup and restore test
#   make smoke SMOKE_ARGS=--keep-on-failure   leave a failed stack running to inspect it
#   make images                 build every image for ARCH, docker load it, tag it BVW_IMAGE_TAG
#   make linux-check            run every x86_64-linux check on LINUX_BUILDER
#   make image-server LINUX_STORE=ssh-ng://dev2   build one image with dev2 as the store

SHELL := bash
HOST_SYSTEM := $(shell nix eval --impure --raw --expr builtins.currentSystem 2>/dev/null)
ifeq ($(HOST_SYSTEM),)
$(error Could not get the Nix system. Is nix on PATH and the daemon running?)
endif
# On macOS, images build on a remote Linux builder through the local Nix daemon and run in
# Docker Desktop under x86_64 emulation (ADR 0002). The daemon uses its own SSH key, so no
# YubiKey is needed. LINUX_STORE=ssh-ng://host builds in that store instead.
ifeq ($(findstring darwin,$(HOST_SYSTEM)),darwin)
ARCH ?= x86_64-linux
LINUX_BUILDER ?= ssh://dev1 x86_64-linux - 32 1 big-parallel,kvm
LINUX_STORE ?=
else
ARCH ?= $(HOST_SYSTEM)
LINUX_BUILDER ?=
LINUX_STORE ?=
endif

NIX_DEV := nix develop --command
CHECK = nix build --no-link --print-build-logs .\#checks.$(HOST_SYSTEM).$(1)

IMAGES := server postgres kanidm caddy backup
LINUX_NIX = $(if $(LINUX_STORE),--eval-store auto --store $(LINUX_STORE),$(if $(LINUX_BUILDER),\
	--option builders '$(LINUX_BUILDER)' --option external-builders '[]' --option builders-use-substitutes true))
IMAGE_BUILD = nix build --no-link --print-out-paths --print-build-logs $(LINUX_NIX) .\#packages.$(ARCH).$(1)-image
LINUX_CHECKS = fmt clippy nextest deny audit layers unsafe-forbid coverage-report coverage restore image-contents
IMAGE_CAT = $(if $(LINUX_STORE),nix store cat --store $(LINUX_STORE),cat)

PG_DIR := $(CURDIR)/.pg
PG_PORT ?= 54331
DATABASE_URL ?= postgres://postgres@127.0.0.1:$(PG_PORT)/postgres
export DATABASE_URL

COMPOSE_PROJECT ?= bvw
SMOKE_PROJECT ?= bvw-smoke
SMOKE_HTTP_PORT ?= 18080
SMOKE_HTTPS_PORT ?= 18443
SMOKE_SUBNET ?= 10.89.1.0/24
SMOKE_ARGS ?=
BVW_IMAGE_TAG ?= dev
BVW_HTTP_PORT ?= 8080
BVW_HTTPS_PORT ?= 8443
export BVW_HTTP_PORT BVW_HTTPS_PORT BVW_IMAGE_TAG
COMPOSE := docker compose --file deploy/compose.yaml --project-name $(COMPOSE_PROJECT)

.DEFAULT_GOAL := help
.PHONY: help check test fmt fmt-check lint layers unsafe-forbid deny audit coverage server db-up db-down \
	migrations-lock restore-test linux-check images $(addprefix image-,$(IMAGES)) up down smoke

help: ## List targets
	@grep -E '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-14s %s\n", $$1, $$2}'
	@echo "  image-NAME     Build one image and docker load it; NAME is one of: $(IMAGES)"
	@echo
	@echo "Variables:"
	@echo "  ARCH=$(ARCH)  system the images are built for"
	@echo "  LINUX_BUILDER=$(LINUX_BUILDER)  remote builder for ARCH (empty: local)"
	@echo "  LINUX_STORE=$(LINUX_STORE)  Nix store that builds instead of LINUX_BUILDER"

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

restore-test: ## Back up a seeded database, restore it and verify (Gate 1)
	$(call CHECK,restore)

server: ## Build the server binary
	nix build --print-build-logs .\#server

up: ## Start the Compose stack on BVW_HTTP_PORT/BVW_HTTPS_PORT (default 8080/8443)
	$(COMPOSE) up --detach --wait

down: ## Stop the Compose stack (volumes stay)
	$(COMPOSE) down

smoke: ## Compose smoke test against the loaded images (Gate 1); runs beside `make up`
	$(NIX_DEV) cargo xtask compose-smoke --project $(SMOKE_PROJECT) --main-project $(COMPOSE_PROJECT) --tag $(BVW_IMAGE_TAG) --http-port $(SMOKE_HTTP_PORT) --https-port $(SMOKE_HTTPS_PORT) --subnet $(SMOKE_SUBNET) $(SMOKE_ARGS)

images: $(addprefix image-,$(IMAGES)) ## Build every image for ARCH, docker load it, tag it BVW_IMAGE_TAG

linux-check: ## Run every ARCH check (images, restore test, ...) on the Linux builder
	nix build --no-link --print-build-logs $(LINUX_NIX) $(foreach c,$(LINUX_CHECKS),.\#checks.$(ARCH).$(c))

$(addprefix image-,$(IMAGES)): image-%:
	@out=$$($(call IMAGE_BUILD,$*)) && echo "loading $$out" && $(IMAGE_CAT) $$out | docker load \
		$(if $(filter-out dev,$(BVW_IMAGE_TAG)),&& docker tag bvw-$*:dev bvw-$*:$(BVW_IMAGE_TAG))

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

migrations-lock: ## Append new migrations to crates/db/migrations/migrations.lock; never rewrites entries
	$(NIX_DEV) bash -c 'set -e; cd crates/db/migrations; touch migrations.lock; \
		for f in *.sql; do \
			awk -v f="$$f" "\$$2 == f { found = 1 } END { exit !found }" migrations.lock \
				|| sha384sum "$$f" >> migrations.lock; \
		done'
