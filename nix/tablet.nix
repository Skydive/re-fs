# The tablet agent (re-fsd), cross-built per CPU:
#   armv7    reMarkable 1 and 2: OpenSSL (default, the tablet's libssl) or rustls (static)
#   aarch64  Paper Pro and Paper Pro Move: OpenSSL (default, the tablet's libssl) or
#            rustls (static)
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
  re-fsd-aarch64-rustls = rustlsAgent {
    pname = "re-fsd-aarch64-rustls";
    target = targets.aarch64Musl;
    crossPkgs = pkgs.pkgsCross.aarch64-multiplatform;
  };

  # The OpenSSL backend (the default): a dynamic glibc binary using the tablet's
  # /usr/lib/libssl.so.3 (OpenSSL 3.5 on the reMarkable 2) and libzstd.so.1 instead
  # of carrying its own. Built against nixpkgs' (cached) cross OpenSSL, zstd and
  # glibc, then pointed at the tablet's loader with no rpath.
  opensslAgent = { pname, target, crossPkgs, interpreter }:
    let
      cc = crossPkgs.stdenv.cc;
      prefix = cc.targetPrefix;
      t = builtins.replaceStrings [ "-" ] [ "_" ] target;
      T = pkgs.lib.toUpper t;
      common = {
        inherit src pname;
        strictDeps = true;
        cargoExtraArgs = "-p re-fsd";
        CARGO_BUILD_TARGET = target;
        doCheck = false;
        nativeBuildInputs = [ cc pkgs.pkg-config pkgs.patchelf ];
        "CARGO_TARGET_${T}_LINKER" = "${prefix}cc";
        "CC_${t}" = "${prefix}cc";
        OPENSSL_NO_VENDOR = "1";
        "${T}_OPENSSL_LIB_DIR" = "${crossPkgs.openssl.out}/lib";
        "${T}_OPENSSL_INCLUDE_DIR" = "${crossPkgs.openssl.dev}/include";
        # libzstd: the tablet's /usr/lib/libzstd.so.1 at run time (1.5.5 on the
        # reMarkable 2; only stable functions are used), nixpkgs' cached build to
        # link against.
        PKG_CONFIG_ALLOW_CROSS = "1";
        "PKG_CONFIG_PATH_${t}" = "${crossPkgs.zstd.dev}/lib/pkgconfig";
        "PKG_CONFIG_SYSROOT_DIR_${t}" = "/";
        "CARGO_TARGET_${T}_RUSTFLAGS" = "-L native=${crossPkgs.zstd.out}/lib";
      };
    in
    craneLib.buildPackage (common // {
      cargoArtifacts = craneLib.buildDepsOnly common;
      dontPatchELF = true;
      postFixup = ''
        patchelf --set-interpreter ${interpreter} --remove-rpath $out/bin/re-fsd
      '';
    });

  re-fsd-armv7 = opensslAgent {
    pname = "re-fsd";
    target = targets.armGnu;
    crossPkgs = armPkgs;
    interpreter = "/lib/ld-linux-armhf.so.3";
  };
  re-fsd-aarch64 = opensslAgent {
    pname = "re-fsd-aarch64";
    target = targets.aarch64Gnu;
    crossPkgs = pkgs.pkgsCross.aarch64-multiplatform;
    interpreter = "/lib/ld-linux-aarch64.so.1";
  };

  # Every agent, by CPU and TLS backend.
  agents = {
    armv7 = { openssl = re-fsd-armv7; rustls = re-fsd-armv7-rustls; };
    aarch64 = { openssl = re-fsd-aarch64; rustls = re-fsd-aarch64-rustls; };
  };
in
{
  inherit agents re-fsd-armv7 re-fsd-armv7-rustls re-fsd-aarch64 re-fsd-aarch64-rustls;

  # The reMarkable 1/2 agent by TLS backend: "openssl" (default) or "rustls"
  # (static). Kept for other flakes; `agents.<cpu>.<tls>` has them all.
  agentFor = tls: agents.armv7.${tls};
}
