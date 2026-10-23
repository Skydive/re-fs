# Linux x86_64: the client (re-fs), the native agent (for tests), the checks and
# the dev shell.
{ pkgs, craneLib, src, agentFor }:
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

  # `re-fs deploy` installs the armv7 agent it was built with.
  re-fs = member "re-fs" { RE_FSD_ARMV7 = "${agentFor "openssl"}/bin/re-fsd"; };
in
{
  packages = {
    inherit re-fs;
    # The same client, deploying the static rustls agent.
    re-fs-rustls-agent = member "re-fs" { RE_FSD_ARMV7 = "${agentFor "rustls"}/bin/re-fsd"; };
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
