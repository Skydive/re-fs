# The tablet agent (re-fsd), cross-built per CPU:
#   armv7    reMarkable 1 and 2: OpenSSL (default, the tablet's libssl) or rustls (static)
#   aarch64  Paper Pro and Paper Pro Move: rustls (static) only, since nothing here
#            depends on that OS's glibc or OpenSSL
# All use nixpkgs' (cached) cross GCCs for the C parts.
{ pkgs, craneLib, src, targets }:
let
  armPkgs = pkgs.pkgsCross.armv7l-hf-multiplatform;
  armCC = armPkgs.stdenv.cc;
  armPrefix = armCC.targetPrefix;

  # The rustls backend: static musl, linked by rust-lld (.cargo/config.toml).
  # ring's and zstd's C are compiled by the glibc cross GCC against musl's headers;
  # the result is fully static.
  rustlsAgent = { pname, target, crossPkgs }:
    let
      cc = crossPkgs.stdenv.cc;
      prefix = cc.targetPrefix;
      t = builtins.replaceStrings [ "-" ] [ "_" ] target;
      common = {
        inherit src pname;
        strictDeps = true;
        cargoExtraArgs = "-p re-fsd --no-default-features --features rustls";
        CARGO_BUILD_TARGET = target;
        doCheck = false;
        nativeBuildInputs = [ cc ];
        "CC_${t}" = "${prefix}cc";
        "AR_${t}" = "${prefix}ar";
        preBuild = ''
          export CFLAGS_${t}="-nostdinc -isystem ${crossPkgs.musl.dev}/include -isystem $(${prefix}cc -print-file-name=include)"
        '';
      };
    in
    craneLib.buildPackage (common // { cargoArtifacts = craneLib.buildDepsOnly common; });

  re-fsd-armv7-rustls = rustlsAgent {
    pname = "re-fsd-rustls";
    target = targets.armMusl;
    crossPkgs = armPkgs;
  };
  re-fsd-aarch64 = rustlsAgent {
    pname = "re-fsd-aarch64";
    target = targets.aarch64Musl;
    crossPkgs = pkgs.pkgsCross.aarch64-multiplatform;
  };

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
  inherit re-fsd-armv7 re-fsd-armv7-rustls re-fsd-aarch64;

  # The reMarkable 1/2 agent by TLS backend: "openssl" (default) or "rustls" (static).
  agentFor = tls: { openssl = re-fsd-armv7; rustls = re-fsd-armv7-rustls; }.${tls};
}
