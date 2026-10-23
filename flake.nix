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

      f = fenix.packages.${system};
      toolchain = f.combine [
        f.stable.cargo
        f.stable.rustc
        f.stable.clippy
        f.stable.rustfmt
        f.stable.rust-src
        f.targets.${armTarget}.stable.rust-std
      ];
      craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

      src = craneLib.cleanCargoSource ./.;
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
      armCommon = {
        inherit src;
        strictDeps = true;
        pname = "re-fsd";
        cargoExtraArgs = "-p re-fsd";
        CARGO_BUILD_TARGET = armTarget;
        doCheck = false;
      };
      re-fsd-armv7 = craneLib.buildPackage (armCommon // {
        cargoArtifacts = craneLib.buildDepsOnly armCommon;
      });
    in
    {
      packages.${system} = {
        # `re-fs deploy` installs the armv7 agent it was built with.
        re-fs = member "re-fs" { RE_FSD_ARMV7 = "${re-fsd-armv7}/bin/re-fsd"; };
        re-fsd = member "re-fsd" { };
        inherit re-fsd-armv7;
        default = self.packages.${system}.re-fs;
      };

      checks.${system} = {
        tests = craneLib.cargoTest (common // { inherit cargoArtifacts; });
        clippy = craneLib.cargoClippy (common // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--all-targets -- -D warnings";
        });
        inherit re-fsd-armv7;
      };

      devShells.${system}.default = craneLib.devShell {
        inputsFrom = [ self.packages.${system}.re-fs ];
        packages = [ pkgs.rust-analyzer pkgs.python3 ];
      };
    };
}
