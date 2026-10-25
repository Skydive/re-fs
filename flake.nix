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
  #   tablet.nix     the tablet agent: ARMv7 for rM1/2 (OpenSSL default, rustls static), aarch64 for the Paper Pro
  #   windows.nix    the Windows client (MinGW)
  #   dist.nix       release files: the static client (agents built in), .deb/.rpm/Arch packages
  #   nixos.nix, home-manager.nix   USB auto-mount modules
  outputs = { self, nixpkgs, crane, fenix }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      inherit (import ./nix/toolchain.nix { inherit pkgs crane fenix system; }) craneLib src targets;
      tablet = import ./nix/tablet.nix { inherit pkgs craneLib src targets; };
      # The agents every client build carries (see crates/re-fs/build.rs).
      agents = {
        armv7 = tablet.agentFor "openssl";
        armv7-rustls = tablet.agentFor "rustls";
        aarch64 = tablet.re-fsd-aarch64;
      };
      windows = import ./nix/windows.nix { inherit pkgs craneLib src targets agents; };
      dist = import ./nix/dist.nix { inherit pkgs craneLib src targets agents; };
      linux = import ./nix/linux.nix {
        inherit pkgs craneLib src;
        inherit (tablet) agentFor;
        aarch64Agent = tablet.re-fsd-aarch64;
      };
    in
    {
      # USB auto-mount: the NixOS module names the tablet's USB network device; the
      # home-manager module mounts it while it's plugged in. See docs/wiki/Usage.md.
      nixosModules.default = import ./nix/nixos.nix;
      homeManagerModules.default = import ./nix/home-manager.nix self;

      # For other flakes: the tablet agent by TLS backend, e.g.
      # re-fs.lib.x86_64-linux.agentFor "openssl".
      lib.${system} = { inherit (tablet) agentFor; };

      packages.${system} = linux.packages // {
        inherit (tablet) re-fsd-armv7 re-fsd-armv7-rustls re-fsd-aarch64;
        inherit (windows) re-fs-windows;
        inherit (dist) re-fs-static dist;
        default = self.packages.${system}.re-fs;
      };

      checks.${system} = linux.checks // {
        inherit (tablet) re-fsd-armv7 re-fsd-armv7-rustls re-fsd-aarch64;
        inherit (windows) re-fs-windows;
        inherit (dist) re-fs-static;
      };

      devShells.${system}.default = linux.devShell;
    };
}
