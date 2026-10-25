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

  # The builds live in nix/:
  #   toolchain.nix  Rust toolchain with the cross targets, crane, the source filter
  #   linux.nix      x86_64 Linux: the client, the native agent, checks, dev shell
  #   tablet.nix     the tablet agents: ARMv7 for rM1/2, aarch64 for the Paper Pro; OpenSSL (default) or rustls (static)
  #   windows.nix    the Windows client (MinGW)
  #   dist.nix       release files: the static client (agents built in), .deb/.rpm/Arch packages
  #   nixos.nix, home-manager.nix   USB auto-mount modules
  outputs = { self, nixpkgs, crane, fenix }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      inherit (import ./nix/toolchain.nix { inherit pkgs crane fenix system; }) craneLib src targets;
      # The one version number: Cargo.toml's [workspace.package] version. Releases
      # are tagged release/<version>, and CI checks the two match.
      version = (craneLib.crateNameFromCargoToml { cargoToml = ./Cargo.toml; }).version;
      tablet = import ./nix/tablet.nix { inherit pkgs craneLib src targets; };
      # Every agent by CPU and TLS backend, e.g. agents.aarch64.openssl; clients
      # carry them (see crates/re-fs/build.rs).
      inherit (tablet) agents;
      windows = import ./nix/windows.nix { inherit pkgs craneLib src targets agents; };
      dist = import ./nix/dist.nix { inherit pkgs craneLib src targets agents version; };
      linux = import ./nix/linux.nix { inherit pkgs craneLib src agents; };
    in
    {
      # USB auto-mount: the NixOS module names the tablet's USB network device; the
      # home-manager module mounts it while it's plugged in. See docs/wiki/Usage.md.
      nixosModules.default = import ./nix/nixos.nix;
      homeManagerModules.default = import ./nix/home-manager.nix self;

      # For other flakes: the version, every agent (re-fs.lib.x86_64-linux.agents.aarch64.openssl),
      # and the reMarkable 1/2 agent by TLS backend (agentFor "openssl").
      lib.${system} = { inherit version agents; inherit (tablet) agentFor; };

      packages.${system} = linux.packages // {
        inherit (tablet) re-fsd-armv7 re-fsd-armv7-rustls re-fsd-aarch64 re-fsd-aarch64-rustls;
        inherit (windows) re-fs-windows;
        inherit (dist) re-fs-static dist;
        default = self.packages.${system}.re-fs;
      };

      checks.${system} = linux.checks // {
        inherit (tablet) re-fsd-armv7 re-fsd-armv7-rustls re-fsd-aarch64 re-fsd-aarch64-rustls;
        inherit (windows) re-fs-windows;
        inherit (dist) re-fs-static;
      };

      devShells.${system}.default = linux.devShell;
    };
}
