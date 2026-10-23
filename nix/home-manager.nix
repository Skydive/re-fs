# home-manager side of re-fs's USB auto-mount: a user service bound to the
# tablet's USB network device (named by nixos.nix). Plugging the tablet in starts
# `re-fs mount <mountPoint> --usb`; unplugging stops it, and re-fs unmounts.
self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.re-fs.usbMount;
  device = "sys-subsystem-net-devices-remarkable.device";
  re-fs = "${cfg.package}/bin/re-fs";
in
{
  options.services.re-fs.usbMount = {
    enable = lib.mkEnableOption "mounting the reMarkable with re-fs whenever it's plugged in by USB";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.re-fs;
      defaultText = lib.literalExpression "re-fs.packages.\${system}.re-fs";
      description = "The re-fs client.";
    };
    mountPoint = lib.mkOption {
      type = lib.types.str;
      default = "/run/media/${config.home.username}/reMarkable";
      defaultText = lib.literalExpression ''"/run/media/''${config.home.username}/reMarkable"'';
      description = ''
        Where to mount the tablet. The default is where file managers list removable
        media; it's created at boot by the NixOS module
        (services.re-fs.usbDevice.users). Elsewhere, any directory you own works.
      '';
    };
    extraArgs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "--rw" "-v" ];
      description = "More `re-fs mount` options.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.user.services.re-fs-usb = {
      Unit = {
        Description = "re-fs: the reMarkable, mounted while it's plugged in by USB";
        BindsTo = [ device ];
        After = [ device ];
      };
      Service = {
        # fusermount3 is a setuid wrapper on NixOS; ssh for any --ssh in extraArgs.
        Environment = [ "PATH=/run/wrappers/bin:${pkgs.openssh}/bin" ];
        ExecStartPre = "${pkgs.coreutils}/bin/mkdir -p ${lib.escapeShellArg cfg.mountPoint}";
        ExecStart = "${re-fs} mount ${lib.escapeShellArgs ([ cfg.mountPoint "--usb" ] ++ cfg.extraArgs)}";
        # If re-fs was killed rather than stopped, don't leave a dead mount behind.
        ExecStopPost = "-/run/wrappers/bin/fusermount3 -uz ${lib.escapeShellArg cfg.mountPoint}";
        # The interface appears a moment before the tablet's DHCP gives us an
        # address: re-fs reconnects by itself, and a first start that finds nothing
        # (no saved index yet) is simply retried.
        Restart = "on-failure";
        RestartSec = 2;
      };
      Install.WantedBy = [ device ];
    };
  };
}
