{
  description = "Brood voor Weldadigheid bakery platform";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
      advisory-db,
    }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (mkContext system));

      mkContext =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          inherit (pkgs) lib;
          craneLib = (crane.mkLib pkgs).overrideToolchain (_: toolchain);
          htmx = pkgs.callPackage ./nix/htmx.nix { };
          fonts = pkgs.callPackage ./nix/fonts.nix { };
          lucide = pkgs.callPackage ./nix/lucide.nix { };
          assetEnv = {
            HTMX_JS = "${htmx}";
            FONTS_DIR = "${fonts}";
            LUCIDE_DIR = "${lucide}";
          };
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./crates/db/migrations
              ./.sqlx
              (lib.fileset.fileFilter (file: file.hasExt "ftl") ./locales)
              ./design
              ./templates
              ./apps/server/tests/snapshots
            ];
          };
          commonArgs = {
            inherit src;
            pname = "bakflow";
            version = "0.1.0";
            strictDeps = true;
            # Query macros read .sqlx/, never a live database (make sqlx-prepare).
            SQLX_OFFLINE = "true";
          }
          // assetEnv;
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          postgresql = pkgs.postgresql_17;
          postgres = import ./nix/postgres.nix { inherit postgresql; };
          cli = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              pname = "cli";
              cargoExtraArgs = "--locked --package cli";
              doCheck = false;
              meta.mainProgram = "cli";
            }
          );
        in
        {
          inherit
            pkgs
            toolchain
            craneLib
            src
            commonArgs
            cargoArtifacts
            postgresql
            postgres
            assetEnv
            cli
            ;
        };

      images =
        { pkgs, postgresql, ... }:
        pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
          import ./nix/images.nix {
            inherit pkgs postgresql;
            inherit (self.packages.${pkgs.stdenv.hostPlatform.system}) server cli;
          }
        );
    in
    {
      checks = forAllSystems (
        {
          pkgs,
          craneLib,
          src,
          commonArgs,
          cargoArtifacts,
          postgres,
          cli,
          ...
        }@ctx:
        (images ctx).checks or { }
        // {
          fmt = craneLib.cargoFmt { inherit (commonArgs) src pname version; };

          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--workspace --all-targets --all-features -- --deny warnings";
            }
          );

          nextest = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoNextestExtraArgs = "--workspace --all-features --no-tests=pass";
              doInstallCargoArtifacts = false;
              TZDIR = "${pkgs.tzdata}/share/zoneinfo";
              nativeBuildInputs = postgres.packages ++ [ pkgs.rustic ];
              preCheck = postgres.start;
              postCheck = postgres.stop;
              # The tests bind 127.0.0.1; the macOS sandbox blocks that without this.
              __darwinAllowLocalNetworking = true;
            }
          );

          deny = craneLib.cargoDeny {
            inherit (commonArgs) pname version;
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                (craneLib.fileset.commonCargoSources ./.)
                ./deny.toml
              ];
            };
          };

          audit = craneLib.cargoAudit {
            inherit (commonArgs) src pname version;
            inherit advisory-db;
          };

          layers = craneLib.mkCargoDerivation (
            commonArgs
            // {
              inherit cargoArtifacts;
              pnameSuffix = "-layers";
              buildPhaseCargoCommand = "cargo xtask layers";
              installPhaseCommand = "mkdir -p $out";
              doInstallCargoArtifacts = false;
            }
          );

          unsafe-forbid = craneLib.mkCargoDerivation (
            commonArgs
            // {
              inherit cargoArtifacts;
              pnameSuffix = "-unsafe-forbid";
              buildPhaseCargoCommand = "cargo xtask unsafe-forbid";
              installPhaseCommand = "mkdir -p $out";
              doInstallCargoArtifacts = false;
            }
          );

          coverage-report = craneLib.cargoLlvmCov (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoLlvmCovExtraArgs = "--workspace --all-features --remap-path-prefix --lcov --summary-only --output-path $out";
              TZDIR = "${pkgs.tzdata}/share/zoneinfo";
              nativeBuildInputs = postgres.packages ++ [ pkgs.rustic ];
              preBuild = postgres.start;
              postBuild = postgres.stop;
              __darwinAllowLocalNetworking = true;
            }
          );

          sqlx-prepare = craneLib.mkCargoDerivation (
            commonArgs
            // {
              inherit cargoArtifacts;
              pnameSuffix = "-sqlx-prepare";
              SQLX_OFFLINE = "false";
              nativeBuildInputs = postgres.packages ++ [ pkgs.sqlx-cli ];
              preBuild = postgres.start;
              buildPhaseCargoCommand = ''
                sqlx migrate run --source crates/db/migrations
                cargo sqlx prepare --workspace --check -- --all-targets --all-features
              '';
              postBuild = postgres.stop;
              installPhaseCommand = "mkdir -p $out";
              doInstallCargoArtifacts = false;
            }
          );

          restore = import ./nix/restore-check.nix { inherit pkgs cli postgres; };

          coverage = craneLib.mkCargoDerivation (
            commonArgs
            // {
              inherit cargoArtifacts;
              pnameSuffix = "-coverage";
              coverageReport = self.checks.${pkgs.stdenv.hostPlatform.system}.coverage-report;
              buildPhaseCargoCommand = "cargo xtask coverage $coverageReport";
              installPhaseCommand = "mkdir -p $out";
              doInstallCargoArtifacts = false;
            }
          );
        }
      );

      packages = forAllSystems (
        {
          craneLib,
          commonArgs,
          cargoArtifacts,
          cli,
          ...
        }@ctx:
        (images ctx).packages or { }
        // rec {
          server = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              pname = "server";
              cargoExtraArgs = "--locked --package server";
              doCheck = false;
              meta.mainProgram = "server";
            }
          );
          inherit cli;
          default = server;
        }
      );

      devShells = forAllSystems (
        {
          pkgs,
          craneLib,
          assetEnv,
          ...
        }:
        {
          default = craneLib.devShell (
            assetEnv
            // {
              # Only the cargo checks: the image and restore checks would build the
              # workspace, so a stale .sqlx/ would keep the shell from starting.
              checks = removeAttrs self.checks.${pkgs.stdenv.hostPlatform.system} [
                "restore"
                "image-contents"
              ];
              SQLX_OFFLINE = "true";
              packages = with pkgs; [
                gnumake
                postgresql_17
                sqlx-cli
                cargo-nextest
                cargo-llvm-cov
                cargo-deny
                cargo-audit
                rustic
                curl
                sops
                age
                typst
              ];
            }
          );
        }
      );

      formatter = forAllSystems ({ pkgs, ... }: pkgs.nixfmt-rfc-style);
    };
}
