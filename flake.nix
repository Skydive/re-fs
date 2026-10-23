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
  #   tablet.nix     the reMarkable agent for ARMv7 (OpenSSL default, rustls static)
  #   windows.nix    the Windows client (MinGW)
  #   nixos.nix, home-manager.nix   USB auto-mount modules
  outputs = { self, nixpkgs, crane, fenix }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      inherit (import ./nix/toolchain.nix { inherit pkgs crane fenix system; }) craneLib src targets;
      tablet = import ./nix/tablet.nix { inherit pkgs craneLib src targets; };
      windows = import ./nix/windows.nix { inherit pkgs craneLib src targets; };
      linux = import ./nix/linux.nix { inherit pkgs craneLib src; inherit (tablet) agentFor; };
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
        inherit (tablet) re-fsd-armv7 re-fsd-armv7-rustls;
        inherit (windows) re-fs-windows;
        default = self.packages.${system}.re-fs;
      };

      checks.${system} = linux.checks // {
        inherit (tablet) re-fsd-armv7 re-fsd-armv7-rustls;
        inherit (windows) re-fs-windows;
      };

      devShells.${system}.default = linux.devShell;
    };
}
