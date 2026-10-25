# The Rust toolchain (fenix, stable, with the cross targets) and crane on top of
# it, plus the filtered source tree every build uses.
{ pkgs, crane, fenix, system }:
let
  targets = {
    # The tablet agent with rustls: static musl.
    armMusl = "armv7-unknown-linux-musleabihf";
    # The tablet agent with OpenSSL: links the tablet's own glibc and libssl.so.3.
    armGnu = "armv7-unknown-linux-gnueabihf";
    # The Paper Pro / Paper Pro Move agent: static musl, rustls.
    aarch64Musl = "aarch64-unknown-linux-musl";
    # The Windows client (MinGW).
    windows = "x86_64-pc-windows-gnu";
  };

  f = fenix.packages.${system};
  toolchain = f.combine [
    f.stable.cargo
    f.stable.rustc
    f.stable.clippy
    f.stable.rustfmt
    f.stable.rust-src
    f.targets.${targets.armMusl}.stable.rust-std
    f.targets.${targets.armGnu}.stable.rust-std
    f.targets.${targets.aarch64Musl}.stable.rust-std
    f.targets.${targets.windows}.stable.rust-std
  ];
  craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
in
{
  inherit targets craneLib;

  # Cargo sources, plus deploy/ (embedded in re-fs) and test data.
  src = pkgs.lib.cleanSourceWith {
    src = ../.;
    filter = path: type:
      craneLib.filterCargoSources path type || pkgs.lib.hasInfix "/deploy" path || pkgs.lib.hasInfix "/tests/data" path;
  };
}
