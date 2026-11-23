# Thin wrappers around Nix. Build logic lives in flake.nix.
# Usage: make help

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

.DEFAULT_GOAL := help
.PHONY: help check test fmt fmt-check lint layers unsafe-forbid deny audit

help: ## List targets
	@grep -E '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-14s %s\n", $$1, $$2}'

check: ## Everything CI runs (nix flake check)
	nix flake check --print-build-logs

test: ## Fast test loop in the dev shell
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
