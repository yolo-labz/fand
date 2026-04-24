# nix/package.nix — fand Nix derivation.
#
# FR-010: rustPlatform.buildRustPackage with IOKit + CoreFoundation.
# FR-013: sandbox profile installed to $out/share/fand/.
# FR-014: cargoLock.lockFile for pinned dependencies.
# FR-034: doCheck enables cargo test during nix build.
# FR-035: source filtering excludes non-source files.
# FR-036: default hardening (fortify, stackprotector, PIE) NOT disabled.
# FR-037: no explicit codesign — Darwin stdenv handles ad-hoc signing via sigtool.
# FR-039: meta attributes for discoverability.
# FR-048: SOURCE_DATE_EPOCH passthrough from flake.
{
  lib,
  rustPlatform,
  darwin,
  sourceEpoch ? 0,
}:
let
  # FR-035: filter source tree to exclude non-source files.
  # Prevents README edits from triggering full rebuilds.
  srcFilter = name: type:
    let baseName = builtins.baseNameOf name; in
    !(
      baseName == "result" ||
      baseName == "target" ||
      baseName == ".git" ||
      baseName == "specs" ||
      baseName == ".specify" ||
      baseName == "wip" ||
      baseName == "CLAUDE.md" ||
      lib.hasSuffix ".md" baseName && baseName != "README.md"
    );
  filteredSrc = lib.cleanSourceWith {
    src = ../.;
    filter = srcFilter;
  };
in
rustPlatform.buildRustPackage {
  pname = "fand";
  version = "0.3.4";
  src = filteredSrc;
  cargoLock.lockFile = ../Cargo.lock;

  # FR-048: reproducible builds via SOURCE_DATE_EPOCH from flake.
  env.SOURCE_DATE_EPOCH = toString sourceEpoch;

  buildInputs = [
    darwin.apple_sdk.frameworks.IOKit
    darwin.apple_sdk.frameworks.CoreFoundation
  ];

  # FR-034: run cargo test --lib during nix build.
  doCheck = true;
  checkPhase = ''
    cargo test --lib --locked
  '';

  # FR-013: install sandbox profile for the nix-darwin module.
  postInstall = ''
    mkdir -p $out/share/fand
    cp nix/sandbox-profiles/fand-set.sb $out/share/fand/fand-set.sb
  '';

  # FR-037: NO explicit postFixup codesign.
  # Darwin stdenv's sigtool handles ad-hoc signing automatically.
  # The scripts/sign-release.sh remains for manual builds outside nix.

  doInstallCheck = true;
  installCheckPhase = ''
    $out/bin/fand version
    $out/bin/fand --help
  '';

  # FR-039: meta attributes.
  meta = with lib; {
    description = "Apple Silicon fan control daemon";
    homepage = "https://github.com/yolo-labz/fand";
    license = licenses.mit;
    maintainers = [];
    platforms = [ "aarch64-darwin" ];
    mainProgram = "fand";
  };
}
