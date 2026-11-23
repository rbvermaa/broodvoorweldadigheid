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
          craneLib = (crane.mkLib pkgs).overrideToolchain (_: toolchain);
          src = craneLib.cleanCargoSource ./.;
          commonArgs = {
            inherit src;
            pname = "bakflow";
            version = "0.1.0";
            strictDeps = true;
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        in
        {
          inherit
            pkgs
            toolchain
            craneLib
            src
            commonArgs
            cargoArtifacts
            ;
        };
    in
    {
      checks = forAllSystems (
        {
          craneLib,
          src,
          commonArgs,
          cargoArtifacts,
          ...
        }:
        {
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
              cargoNextestExtraArgs = "--workspace --no-tests=pass";
            }
          );

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
        }
      );

      devShells = forAllSystems (
        { pkgs, craneLib, ... }:
        {
          default = craneLib.devShell {
            checks = self.checks.${pkgs.stdenv.hostPlatform.system};
            packages = with pkgs; [
              gnumake
              postgresql_17
              sqlx-cli
              cargo-nextest
              cargo-llvm-cov
              cargo-deny
              cargo-audit
              rustic
              sops
              age
              typst
            ];
          };
        }
      );

      formatter = forAllSystems ({ pkgs, ... }: pkgs.nixfmt-rfc-style);
    };
}
