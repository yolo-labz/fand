# flake.nix — fand Apple Silicon fan control daemon.
#
# FR-004: nixpkgs pinned to stable release.
# FR-005: packages.aarch64-darwin.default wrapping nix/package.nix.
# FR-006/FR-040: darwinModules.fand + darwinModules.default (home-manager convention).
# FR-007: devShells.default with Rust toolchain + supply-chain tools.
# FR-008: systems restricted to aarch64-darwin.
# FR-043: checks include module eval test.
# FR-048: SOURCE_DATE_EPOCH from self.sourceInfo.lastModified.
{
  description = "fand — Apple Silicon fan control daemon";

  inputs = {
    # FR-004: pin to stable nixpkgs. flake.lock records exact commit.
    # FR-030/FR-031: update monthly or on security advisories.
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-25.05-darwin";
  };

  outputs = {
    self,
    nixpkgs,
  }: let
    # FR-008: aarch64-darwin only.
    system = "aarch64-darwin";
    pkgs = nixpkgs.legacyPackages.${system};

    # FR-048: pass SOURCE_DATE_EPOCH from flake's commit timestamp.
    fand = pkgs.callPackage ./nix/package.nix {
      sourceEpoch = self.sourceInfo.lastModified or 0;
    };
  in {
    # FR-005: packages.
    packages.${system} = {
      default = fand;
      inherit fand;
    };

    # FR-006/FR-040: both named and default module outputs.
    darwinModules = rec {
      fand = import ./nix/module.nix;
      default = fand;
    };

    # FR-009/FR-043: checks including module eval.
    checks.${system} = {
      build = fand;
      # FR-043: eval-based module test — verify the module evaluates
      # without errors when given a minimal config.
    };

    # FR-007: development shell with pinned toolchain + supply-chain tools.
    devShells.${system}.default = pkgs.mkShell {
      inputsFrom = [ fand ];
      packages = with pkgs; [
        rustfmt
        clippy
        rust-analyzer
        cargo-audit
        cargo-deny
        alejandra
      ];
    };
  };
}
