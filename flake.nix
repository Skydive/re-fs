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

      f = fenix.packages.${system};
      toolchain = f.combine [
        f.stable.cargo
        f.stable.rustc
        f.stable.clippy
        f.stable.rustfmt
        f.stable.rust-src
        f.targets.${armTarget}.stable.rust-std
        f.targets.${armGnuTarget}.stable.rust-std
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
      };
      cargoArtifacts = craneLib.buildDepsOnly common;

      member = name: extra: craneLib.buildPackage (common // {
        inherit cargoArtifacts;
        pname = name;
        cargoExtraArgs = "-p ${name}";
        doCheck = false; # tests run once, in checks.tests
      } // extra);

      # The tablet agent: static musl, linked by rust-lld (.cargo/config.toml).
      # ring's C and assembly are compiled by nixpkgs' (cached) ARMv7 GCC against
      # musl's headers; the result still links as a static musl binary.
      armPkgs = pkgs.pkgsCross.armv7l-hf-multiplatform;
      armCC = armPkgs.stdenv.cc;
      armPrefix = armCC.targetPrefix;
      armCommon = {
        inherit src;
        strictDeps = true;
        pname = "re-fsd";
        cargoExtraArgs = "-p re-fsd";
        CARGO_BUILD_TARGET = armTarget;
        doCheck = false;
        nativeBuildInputs = [ armCC ];
        "CC_armv7_unknown_linux_musleabihf" = "${armPrefix}cc";
        "AR_armv7_unknown_linux_musleabihf" = "${armPrefix}ar";
        preBuild = ''
          export CFLAGS_armv7_unknown_linux_musleabihf="-nostdinc -isystem ${armPkgs.musl.dev}/include -isystem $(${armPrefix}cc -print-file-name=include)"
        '';
      };
      re-fsd-armv7 = craneLib.buildPackage (armCommon // {
        cargoArtifacts = craneLib.buildDepsOnly armCommon;
      });

      # The agent with the OpenSSL backend (`tls = "openssl"`): a dynamic glibc
      # binary using the reMarkable's /usr/lib/libssl.so.3 (OpenSSL 3.5) instead of
      # carrying rustls + ring. Built against nixpkgs' (cached) ARMv7 OpenSSL 3.5 and
      # glibc, then pointed at the tablet's loader with no rpath.
      armGnuCommon = {
        inherit src;
        strictDeps = true;
        pname = "re-fsd-openssl";
        cargoExtraArgs = "-p re-fsd --no-default-features --features openssl";
        CARGO_BUILD_TARGET = armGnuTarget;
        doCheck = false;
        nativeBuildInputs = [ armCC pkgs.pkg-config pkgs.patchelf ];
        "CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER" = "${armPrefix}cc";
        "CC_armv7_unknown_linux_gnueabihf" = "${armPrefix}cc";
        OPENSSL_NO_VENDOR = "1";
        "ARMV7_UNKNOWN_LINUX_GNUEABIHF_OPENSSL_LIB_DIR" = "${armPkgs.openssl.out}/lib";
        "ARMV7_UNKNOWN_LINUX_GNUEABIHF_OPENSSL_INCLUDE_DIR" = "${armPkgs.openssl.dev}/include";
      };
      re-fsd-armv7-openssl = craneLib.buildPackage (armGnuCommon // {
        cargoArtifacts = craneLib.buildDepsOnly armGnuCommon;
        dontPatchELF = true;
        postFixup = ''
          patchelf --set-interpreter /lib/ld-linux-armhf.so.3 --remove-rpath $out/bin/re-fsd
        '';
      });
      # A re-fsd for the tablet by TLS backend: "rustls" (static) or "openssl".
      agentFor = tls: { rustls = re-fsd-armv7; openssl = re-fsd-armv7-openssl; }.${tls};
    in
    {
      # For other flakes: the tablet agent by TLS backend, e.g.
      # re-fs.lib.x86_64-linux.agentFor "openssl".
      lib.${system} = { inherit agentFor; };

      packages.${system} = {
        # `re-fs deploy` installs the armv7 agent it was built with.
        re-fs = member "re-fs" { RE_FSD_ARMV7 = "${agentFor "rustls"}/bin/re-fsd"; };
        # The same client, deploying the OpenSSL agent.
        re-fs-openssl-agent = member "re-fs" { RE_FSD_ARMV7 = "${agentFor "openssl"}/bin/re-fsd"; };
        inherit re-fsd-armv7-openssl;
        re-fsd = member "re-fsd" { };
        inherit re-fsd-armv7;
        default = self.packages.${system}.re-fs;
      };

      checks.${system} = {
        tests = craneLib.cargoTest (common // { inherit cargoArtifacts; });
        # The agent's transmission tests again, with the OpenSSL backend.
        tests-openssl = craneLib.cargoTest (common // {
          inherit cargoArtifacts;
          cargoTestExtraArgs = "-p re-fsd --no-default-features --features openssl";
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.openssl ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.openssl ];
        });
        clippy = craneLib.cargoClippy (common // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--all-targets -- -D warnings";
        });
        inherit re-fsd-armv7 re-fsd-armv7-openssl;
      };

      devShells.${system}.default = craneLib.devShell {
        inputsFrom = [ self.packages.${system}.re-fs ];
        packages = [ pkgs.rust-analyzer pkgs.python3 pkgs.pkg-config pkgs.openssl ];
        # For `cargo test -p re-fsd --no-default-features --features openssl`.
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.openssl ];
      };
    };
}
