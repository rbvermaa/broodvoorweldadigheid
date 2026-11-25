# Thin wrappers around Nix. Build logic lives in flake.nix.
# Usage: make help
#   make db-up && make test     run the tests against a local PostgreSQL
#   make db-down                stop it (data stays in .pg/)
#   make db-up && make sqlx-prepare   regenerate .sqlx/ after changing a query or migration
#   PG_PORT=54332 make db-up    use another port when one is taken
#   make migrations-lock        record new migrations; a db test refuses edited or unlisted ones
#   make logo                   remake design/logo.png, favicon.png and apple-touch-icon.png
#   make up / make down         run the Compose stack in deploy/ (needs deploy/secrets, see deploy/README.md)
#   make staff-add SUB=<uuid> NAME="Bas van 't Hof" ROLE=baker   let a person into the back office (stack must be up)
#   make kanidm-setup           register the staff login client in the running stack (once)
#   make kanidm-person NAME=bas DISPLAY_NAME="Bas" MAIL=bas@example.org   a person + passkey link
#   make smoke                  throwaway stack: health, Kanidm, backup and restore test
#   make smoke SMOKE_ARGS=--keep-on-failure   leave a failed stack running to inspect it
#   make images                 build every image for ARCH, docker load it, tag it BVW_IMAGE_TAG
#   make linux-check            run every x86_64-linux check on LINUX_BUILDER
#   make e2e                    browser tests in a NixOS VM (x86_64-linux with kvm; LINUX_BUILDER on macOS)
#   make e2e-local              the same flows against a server and geckodriver you run yourself (e2e/README.md)
#   make image-server LINUX_STORE=ssh-ng://dev2   build one image with dev2 as the store
#   MOLLIE_TEST_API_KEY_FILE=path make mollie-contract   contract test against Mollie test mode

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

IMAGES := server postgres kanidm caddy backup mailpit
LINUX_NIX = $(if $(LINUX_STORE),--eval-store auto --store $(LINUX_STORE),$(if $(LINUX_BUILDER),\
	--option builders '$(LINUX_BUILDER)' --option external-builders '[]' --option builders-use-substitutes true))
IMAGE_BUILD = nix build --no-link --print-out-paths --print-build-logs $(LINUX_NIX) .\#packages.$(ARCH).$(1)-image
LINUX_CHECKS = fmt clippy nextest deny audit layers unsafe-forbid adr-numbers coverage-report coverage restore image-contents \
	$(if $(filter x86_64-linux,$(ARCH)),e2e)
IMAGE_CAT = $(if $(LINUX_STORE),nix store cat --store $(LINUX_STORE),cat)

PG_DIR := $(CURDIR)/.pg
PG_PORT ?= 54331
DATABASE_URL ?= postgres://postgres@127.0.0.1:$(PG_PORT)/postgres
export DATABASE_URL
SQLX_DATABASE_URL ?= postgres://postgres@127.0.0.1:$(PG_PORT)/bvw_sqlx_prepare
# apalis-postgres checks its queries against DATABASE_URL whenever SQLX_OFFLINE is false, so
# sqlx prepare needs its tables too, and a fresh build of it so its query data is written
# (ADR 0011).
APALIS_MIGRATIONS := cargo metadata --format-version 1 --locked | grep -o "\"manifest_path\":\"[^\"]*apalis-postgres-[^/\"]*/Cargo.toml\"" \
	| sed -e "s/\"manifest_path\":\"//" -e "s|/Cargo.toml\"$$|/migrations|"

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
COMPOSE := docker compose --file deploy/compose.yaml --project-name $(COMPOSE_PROJECT) --profile mail
# Compose resolves a relative BVW_SECRETS_DIR against deploy/.
SECRETS_DIR := $(if $(BVW_SECRETS_DIR),$(if $(filter /%,$(BVW_SECRETS_DIR)),$(BVW_SECRETS_DIR),deploy/$(BVW_SECRETS_DIR)),deploy/secrets)
OIDC_DIR := $(SECRETS_DIR)/oidc
SHOP_URL := https://localhost:$(BVW_HTTPS_PORT)
IDM_URL := https://idm.localhost:$(BVW_HTTPS_PORT)

.DEFAULT_GOAL := help
.PHONY: help check test fmt fmt-check lint nextest layers unsafe-forbid adr-numbers deny audit coverage-report coverage server db-up \
	db-down sqlx-prepare sqlx-check migrations-lock permissions-doc logo restore-test mollie-contract ci-deps image-contents linux-check \
	images $(addprefix image-,$(IMAGES)) up down smoke staff-add e2e e2e-local \
	kanidm-setup kanidm-person

help: ## List targets
	@grep -E '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-16s %s\n", $$1, $$2}'
	@echo "  image-NAME       Build one image and docker load it; NAME is one of: $(IMAGES)"
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

nextest: ## Test suite check only (nextest in the Nix sandbox)
	$(call CHECK,nextest)

layers: ## Crate layer and dependency rules check
	$(call CHECK,layers)

unsafe-forbid: ## Every crate root forbids unsafe code
	$(call CHECK,unsafe-forbid)

adr-numbers: ## Every ADR in docs/decisions has its own number and a title
	$(call CHECK,adr-numbers)

deny: ## Licence, ban and source checks (cargo-deny)
	$(call CHECK,deny)

audit: ## Known-vulnerability check (cargo-audit)
	$(call CHECK,audit)

coverage-report: ## Coverage report only (cargo llvm-cov)
	$(call CHECK,coverage-report)

coverage: ## Line coverage gate (80% overall, 95% in strict crates)
	$(call CHECK,coverage)

sqlx-prepare: ## Regenerate .sqlx/ (offline query data) against a scratch database (needs make db-up)
	$(NIX_DEV) bash -c 'set -e; export DATABASE_URL=$(SQLX_DATABASE_URL) SQLX_OFFLINE=false; \
		sqlx database drop -y; sqlx database create; \
		sqlx migrate run --source "$$($(APALIS_MIGRATIONS))"; \
		sqlx migrate run --ignore-missing --source crates/db/migrations; \
		cargo clean --package apalis-postgres; \
		cargo sqlx prepare --workspace -- --all-targets --all-features'

sqlx-check: ## Check .sqlx/ matches the queries and migrations
	$(call CHECK,sqlx-prepare)

restore-test: ## Back up a seeded database, restore it and verify (Gate 1)
	$(call CHECK,restore)

mollie-contract: ## Contract test against Mollie test mode (needs MOLLIE_TEST_API_KEY_FILE)
	@test -n "$$MOLLIE_TEST_API_KEY_FILE" || { echo "Set MOLLIE_TEST_API_KEY_FILE to a file holding a Mollie test_ key" >&2; exit 1; }
	$(NIX_DEV) cargo nextest run --package payments --test mollie_contract --no-capture

server: ## Build the server binary
	nix build --print-build-logs .\#server

up: ## Start the Compose stack on BVW_HTTP_PORT/BVW_HTTPS_PORT (default 8080/8443)
	$(COMPOSE) up --detach --wait

down: ## Stop the Compose stack (volumes stay)
	$(COMPOSE) down

# The values reach the cli as environment variables, never spliced into the command line,
# so names like "Bas van 't Hof" survive.
staff-add: ## Let a person into the back office: SUB (their Kanidm UUID), NAME, ROLE (owner, baker, counter), EMAIL
	@test -n "$$SUB" -a -n "$$NAME" -a -n "$$ROLE" || { echo "usage: make staff-add SUB=<uuid> NAME=\"Bas van 't Hof\" ROLE=baker [EMAIL=...]"; exit 2; }
	STAFF_SUB="$$SUB" STAFF_NAME="$$NAME" STAFF_ROLE="$$ROLE" STAFF_EMAIL="$$EMAIL" \
		$(COMPOSE) run --rm --no-deps -e STAFF_SUB -e STAFF_NAME -e STAFF_ROLE -e STAFF_EMAIL migrate staff add

kanidm-setup: ## Register the staff OAuth2 client in Kanidm; writes deploy/secrets/oidc/ (stack must be up)
	@mkdir -p $(OIDC_DIR) && chmod 755 $(OIDC_DIR)
	$(COMPOSE) exec -T kanidm cli kanidm ca > $(OIDC_DIR)/.kanidm-ca.pem && chmod 644 $(OIDC_DIR)/.kanidm-ca.pem && mv $(OIDC_DIR)/.kanidm-ca.pem $(OIDC_DIR)/kanidm-ca.pem
	$(COMPOSE) exec -T kanidm cli kanidm oauth2-client --url $(IDM_URL) \
		--landing-url $(SHOP_URL)/staff --redirect-url $(SHOP_URL)/staff/oidc/callback \
		> $(OIDC_DIR)/.client-secret && chmod 644 $(OIDC_DIR)/.client-secret && mv $(OIDC_DIR)/.client-secret $(OIDC_DIR)/client-secret
	@echo "staff login is set up; next: make kanidm-person NAME=... DISPLAY_NAME=..."

kanidm-person: ## Create a Kanidm person in bvw_staff and print their sub and passkey link: NAME, DISPLAY_NAME, MAIL
	@test -n "$$NAME" -a -n "$$DISPLAY_NAME" || { echo "usage: make kanidm-person NAME=bas DISPLAY_NAME=\"Bas van 't Hof\" [MAIL=...]"; exit 2; }
	$(COMPOSE) exec -T -e "PERSON_NAME=$$NAME" -e "PERSON_DISPLAY_NAME=$$DISPLAY_NAME" -e "PERSON_MAIL=$$MAIL" \
		kanidm cli kanidm person --url $(IDM_URL)

smoke: ## Compose smoke test against the loaded images (Gate 1); runs beside `make up`
	$(NIX_DEV) cargo xtask compose-smoke --project $(SMOKE_PROJECT) --main-project $(COMPOSE_PROJECT) --tag $(BVW_IMAGE_TAG) --http-port $(SMOKE_HTTP_PORT) --https-port $(SMOKE_HTTPS_PORT) --subnet $(SMOKE_SUBNET) $(SMOKE_ARGS)

# The logo and the icons cut from its inner disc (ADR 0023). The outputs are committed.
LOGO_SRC := docs/media/image1.png
LOGO_TOOLS := nix shell --inputs-from . nixpkgs\#imagemagick nixpkgs\#oxipng --command
LOGO_DISC := \( -size 600x600 xc:black -fill white -draw "circle 299.5,299.5 299.5,0" \) \
	-alpha off -compose CopyOpacity -composite -background white -compose Over -flatten
logo: ## Remake the logo PNGs in design/ from docs/media/image1.png
	$(LOGO_TOOLS) magick $(LOGO_SRC) -colorspace Gray -filter Lanczos -resize 420x420 -strip design/logo.png
	$(LOGO_TOOLS) magick $(LOGO_SRC) -colorspace Gray -crop 600x600+213+206 +repage $(LOGO_DISC) -resize 64x64 -strip design/favicon.png
	$(LOGO_TOOLS) magick $(LOGO_SRC) -colorspace Gray -crop 600x600+213+206 +repage $(LOGO_DISC) -resize 180x180 -strip design/apple-touch-icon.png
	$(LOGO_TOOLS) oxipng --opt max --strip all design/logo.png design/favicon.png design/apple-touch-icon.png

images: $(addprefix image-,$(IMAGES)) ## Build every image for ARCH, docker load it, tag it BVW_IMAGE_TAG

ci-deps: ## Build the dependency artifacts and toolchain every check shares (CI deps job)
	nix build --no-link --print-build-logs .\#packages.$(HOST_SYSTEM).ci-deps

image-contents: ## Image contents check only, for ARCH on the Linux builder
	nix build --no-link --print-build-logs $(LINUX_NIX) .\#checks.$(ARCH).image-contents

e2e: ## Browser tests (thirtyfour, axe-core) in a NixOS VM; x86_64-linux with kvm only
	nix build --no-link --print-build-logs $(LINUX_NIX) .\#checks.x86_64-linux.e2e

e2e-local: ## Browser tests against a local server, DATABASE_URL and geckodriver (e2e/README.md); not in CI
	$(NIX_DEV) cargo run --package e2e --

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

permissions-doc: ## Regenerate docs/permissions.md from the permission table in crates/auth
	$(NIX_DEV) env BVW_WRITE_PERMISSIONS_DOC=1 cargo nextest run --package auth --test permissions_doc

migrations-lock: ## Append new migrations to crates/db/migrations/migrations.lock; never rewrites entries
	$(NIX_DEV) bash -c 'set -e; cd crates/db/migrations; touch migrations.lock; \
		if [ -s migrations.lock ] && [ -n "$$(tail -c 1 migrations.lock)" ]; then echo >> migrations.lock; fi; \
		for f in *.sql; do \
			awk -v f="$$f" "\$$2 == f { found = 1 } END { exit !found }" migrations.lock \
				|| sha384sum "$$f" >> migrations.lock; \
		done'
