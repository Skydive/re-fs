# The reMarkable 2 agent (re-fsd), cross-built for ARMv7 in two flavours.
# Both use nixpkgs' (cached) ARMv7 GCC for the C parts.
{ pkgs, craneLib, src, targets }:
let
  armPkgs = pkgs.pkgsCross.armv7l-hf-multiplatform;
  armCC = armPkgs.stdenv.cc;
  armPrefix = armCC.targetPrefix;

  # The rustls backend (optional): static musl, linked by rust-lld
  # (.cargo/config.toml). ring's and zstd's C are compiled against musl's headers;
  # the result is fully static.
  rustlsCommon = {
    inherit src;
    strictDeps = true;
    pname = "re-fsd-rustls";
    cargoExtraArgs = "-p re-fsd --no-default-features --features rustls";
    CARGO_BUILD_TARGET = targets.armMusl;
    doCheck = false;
    nativeBuildInputs = [ armCC ];
    "CC_armv7_unknown_linux_musleabihf" = "${armPrefix}cc";
    "AR_armv7_unknown_linux_musleabihf" = "${armPrefix}ar";
    preBuild = ''
      export CFLAGS_armv7_unknown_linux_musleabihf="-nostdinc -isystem ${armPkgs.musl.dev}/include -isystem $(${armPrefix}cc -print-file-name=include)"
    '';
  };
  re-fsd-armv7-rustls = craneLib.buildPackage (rustlsCommon // {
    cargoArtifacts = craneLib.buildDepsOnly rustlsCommon;
  });

  # The OpenSSL backend (the default): a dynamic glibc binary using the
  # reMarkable's /usr/lib/libssl.so.3 (OpenSSL 3.5) and libzstd.so.1 instead of
  # carrying its own. Built against nixpkgs' (cached) ARMv7 OpenSSL, zstd and
  # glibc, then pointed at the tablet's loader with no rpath.
  opensslCommon = {
    inherit src;
    strictDeps = true;
    pname = "re-fsd";
    cargoExtraArgs = "-p re-fsd";
    CARGO_BUILD_TARGET = targets.armGnu;
    doCheck = false;
    nativeBuildInputs = [ armCC pkgs.pkg-config pkgs.patchelf ];
    "CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER" = "${armPrefix}cc";
    "CC_armv7_unknown_linux_gnueabihf" = "${armPrefix}cc";
    OPENSSL_NO_VENDOR = "1";
    "ARMV7_UNKNOWN_LINUX_GNUEABIHF_OPENSSL_LIB_DIR" = "${armPkgs.openssl.out}/lib";
    "ARMV7_UNKNOWN_LINUX_GNUEABIHF_OPENSSL_INCLUDE_DIR" = "${armPkgs.openssl.dev}/include";
    # libzstd: the tablet's /usr/lib/libzstd.so.1 at run time (1.5.5; only stable
    # functions are used), nixpkgs' cached ARMv7 build to link against.
    PKG_CONFIG_ALLOW_CROSS = "1";
    "PKG_CONFIG_PATH_armv7_unknown_linux_gnueabihf" = "${armPkgs.zstd.dev}/lib/pkgconfig";
    "PKG_CONFIG_SYSROOT_DIR_armv7_unknown_linux_gnueabihf" = "/";
    "CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS" = "-L native=${armPkgs.zstd.out}/lib";
  };
  re-fsd-armv7 = craneLib.buildPackage (opensslCommon // {
    cargoArtifacts = craneLib.buildDepsOnly opensslCommon;
    dontPatchELF = true;
    postFixup = ''
      patchelf --set-interpreter /lib/ld-linux-armhf.so.3 --remove-rpath $out/bin/re-fsd
    '';
  });
in
{
  inherit re-fsd-armv7 re-fsd-armv7-rustls;

  # The agent by TLS backend: "openssl" (default) or "rustls" (static).
  agentFor = tls: { openssl = re-fsd-armv7; rustls = re-fsd-armv7-rustls; }.${tls};
}
