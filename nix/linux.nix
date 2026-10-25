# Linux x86_64: the client (re-fs), the native agent (for tests), the checks and
# the dev shell.
{ pkgs, craneLib, src, agentFor, aarch64Agent }:
let
  common = {
    inherit src;
    strictDeps = true;
    # re-fsd's default build links the system OpenSSL and libzstd.
    nativeBuildInputs = [ pkgs.pkg-config ];
    buildInputs = [ pkgs.openssl pkgs.zstd ];
    LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.openssl pkgs.zstd ];
  };
  cargoArtifacts = craneLib.buildDepsOnly common;

  member = name: extra: craneLib.buildPackage (common // {
    inherit cargoArtifacts;
    pname = name;
    cargoExtraArgs = "-p ${name}";
    doCheck = false; # tests run once, in checks.tests
  } // extra);

  # Both agents are built into the client (crates/re-fs/build.rs); `re-fs deploy`
  # installs the one for the tablet's CPU: armv7 (reMarkable 1/2) with the given
  # TLS backend, or aarch64 (Paper Pro).
  client = tls: member "re-fs" {
    RE_FSD_ARMV7 = "${agentFor tls}/bin/re-fsd";
    RE_FSD_AARCH64 = "${aarch64Agent}/bin/re-fsd";
  };
  re-fs = client "openssl";
in
{
  packages = {
    inherit re-fs;
    # The same client, deploying the static rustls agent to a reMarkable 1/2 too.
    re-fs-rustls-agent = client "rustls";
    re-fsd = member "re-fsd" { };
  };

  checks = {
    tests = craneLib.cargoTest (common // { inherit cargoArtifacts; });
    # The agent's transmission tests again, with the rustls backend.
    tests-rustls = craneLib.cargoTest (common // {
      inherit cargoArtifacts;
      cargoTestExtraArgs = "-p re-fsd --no-default-features --features rustls";
    });
    clippy = craneLib.cargoClippy (common // {
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--all-targets -- -D warnings";
    });
  };

  devShell = craneLib.devShell {
    inputsFrom = [ re-fs ];
    packages = [ pkgs.rust-analyzer pkgs.python3 pkgs.pkg-config pkgs.openssl pkgs.zstd ];
    # re-fsd's tests link the system OpenSSL and libzstd.
    LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.openssl pkgs.zstd ];
  };
}
