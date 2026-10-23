{
  description = "re-fs: FUSE view of a reMarkable tablet, with the re-fsd agent";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, crane, fenix }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      armTarget = "armv7-unknown-linux-musleabihf";
      # The OpenSSL agent links the tablet's own glibc and libssl.so.3 dynamically.
      armGnuTarget = "armv7-unknown-linux-gnueabihf";
      winTarget = "x86_64-pc-windows-gnu";

      f = fenix.packages.${system};
      toolchain = f.combine [
        f.stable.cargo
        f.stable.rustc
        f.stable.clippy
        f.stable.rustfmt
        f.stable.rust-src
        f.targets.${armTarget}.stable.rust-std
        f.targets.${armGnuTarget}.stable.rust-std
        f.targets.${winTarget}.stable.rust-std
      ];
      craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

      # Cargo sources, plus deploy/ (embedded in re-fs) and test data.
      src = pkgs.lib.cleanSourceWith {
        src = ./.;
        filter = path: type:
          craneLib.filterCargoSources path type || pkgs.lib.hasInfix "/deploy" path || pkgs.lib.hasInfix "/tests/data" path;
      };
      common = {
        inherit src;
        strictDeps = true;
        # re-fsd's default TLS backend is the system OpenSSL.
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.openssl ];
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.openssl ];
      };
      cargoArtifacts = craneLib.buildDepsOnly common;

      member = name: extra: craneLib.buildPackage (common // {
        inherit cargoArtifacts;
        pname = name;
        cargoExtraArgs = "-p ${name}";
        doCheck = false; # tests run once, in checks.tests
      } // extra);

      # The tablet agent with the rustls backend (optional): static musl, linked by
      # rust-lld (.cargo/config.toml). ring's C and assembly are compiled by nixpkgs'
      # (cached) ARMv7 GCC against musl's headers; the result is fully static.
      armPkgs = pkgs.pkgsCross.armv7l-hf-multiplatform;
      armCC = armPkgs.stdenv.cc;
      armPrefix = armCC.targetPrefix;
      armCommon = {
        inherit src;
        strictDeps = true;
        pname = "re-fsd-rustls";
        cargoExtraArgs = "-p re-fsd --no-default-features --features rustls";
        CARGO_BUILD_TARGET = armTarget;
        doCheck = false;
        nativeBuildInputs = [ armCC ];
        "CC_armv7_unknown_linux_musleabihf" = "${armPrefix}cc";
        "AR_armv7_unknown_linux_musleabihf" = "${armPrefix}ar";
        preBuild = ''
          export CFLAGS_armv7_unknown_linux_musleabihf="-nostdinc -isystem ${armPkgs.musl.dev}/include -isystem $(${armPrefix}cc -print-file-name=include)"
        '';
      };
      re-fsd-armv7-rustls = craneLib.buildPackage (armCommon // {
        cargoArtifacts = craneLib.buildDepsOnly armCommon;
      });

      # The tablet agent with the OpenSSL backend (the default): a dynamic glibc
      # binary using the reMarkable's /usr/lib/libssl.so.3 (OpenSSL 3.5) instead of
      # carrying rustls + ring. Built against nixpkgs' (cached) ARMv7 OpenSSL 3.5 and
      # glibc, then pointed at the tablet's loader with no rpath.
      armGnuCommon = {
        inherit src;
        strictDeps = true;
        pname = "re-fsd";
        cargoExtraArgs = "-p re-fsd";
        CARGO_BUILD_TARGET = armGnuTarget;
        doCheck = false;
        nativeBuildInputs = [ armCC pkgs.pkg-config pkgs.patchelf ];
        "CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER" = "${armPrefix}cc";
        "CC_armv7_unknown_linux_gnueabihf" = "${armPrefix}cc";
        OPENSSL_NO_VENDOR = "1";
        "ARMV7_UNKNOWN_LINUX_GNUEABIHF_OPENSSL_LIB_DIR" = "${armPkgs.openssl.out}/lib";
        "ARMV7_UNKNOWN_LINUX_GNUEABIHF_OPENSSL_INCLUDE_DIR" = "${armPkgs.openssl.dev}/include";
      };
      re-fsd-armv7 = craneLib.buildPackage (armGnuCommon // {
        cargoArtifacts = craneLib.buildDepsOnly armGnuCommon;
        dontPatchELF = true;
        postFixup = ''
          patchelf --set-interpreter /lib/ld-linux-armhf.so.3 --remove-rpath $out/bin/re-fsd
        '';
      });
      # The client for Windows (x86_64, MinGW): ls, push, pair and deploy; no mount
      # yet (that needs a WinFsp backend). Linked by nixpkgs' (cached) MinGW GCC,
      # which also compiles ring's and zstd's C.
      winPkgs = pkgs.pkgsCross.mingwW64;
      winCC = winPkgs.stdenv.cc;
      winCommon = {
        inherit src;
        strictDeps = true;
        pname = "re-fs-windows";
        cargoExtraArgs = "-p re-fs";
        CARGO_BUILD_TARGET = winTarget;
        doCheck = false;
        nativeBuildInputs = [ winCC ];
        "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER" = "${winCC.targetPrefix}cc";
        "CC_x86_64_pc_windows_gnu" = "${winCC.targetPrefix}cc";
        "AR_x86_64_pc_windows_gnu" = "${winCC.targetPrefix}ar";
        "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS" = "-L native=${winPkgs.windows.pthreads}/lib";
      };
      re-fs-windows = craneLib.buildPackage (winCommon // {
        cargoArtifacts = craneLib.buildDepsOnly winCommon;
      });

      # A re-fsd for the tablet by TLS backend: "openssl" (default) or "rustls" (static).
      agentFor = tls: { openssl = re-fsd-armv7; rustls = re-fsd-armv7-rustls; }.${tls};
    in
    {
      # For other flakes: the tablet agent by TLS backend, e.g.
      # re-fs.lib.x86_64-linux.agentFor "openssl".
      lib.${system} = { inherit agentFor; };

      packages.${system} = {
        # `re-fs deploy` installs the armv7 agent it was built with.
        re-fs = member "re-fs" { RE_FSD_ARMV7 = "${agentFor "openssl"}/bin/re-fsd"; };
        # The same client, deploying the static rustls agent.
        re-fs-rustls-agent = member "re-fs" { RE_FSD_ARMV7 = "${agentFor "rustls"}/bin/re-fsd"; };
        inherit re-fsd-armv7-rustls;
        re-fsd = member "re-fsd" { };
        inherit re-fsd-armv7 re-fs-windows;
        default = self.packages.${system}.re-fs;
      };

      checks.${system} = {
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
        inherit re-fsd-armv7 re-fsd-armv7-rustls re-fs-windows;
      };

      devShells.${system}.default = craneLib.devShell {
        inputsFrom = [ self.packages.${system}.re-fs ];
        packages = [ pkgs.rust-analyzer pkgs.python3 pkgs.pkg-config pkgs.openssl ];
        # re-fsd's tests link the system OpenSSL (its default backend).
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.openssl ];
      };
    };
}
