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
      default = "${config.home.homeDirectory}/rm";
      defaultText = lib.literalExpression ''"''${config.home.homeDirectory}/rm"'';
      description = "Where to mount the tablet.";
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
        ExecStartPre = [
          "${pkgs.coreutils}/bin/mkdir -p ${lib.escapeShellArg cfg.mountPoint}"
          # The interface appears before the tablet's DHCP has given us an address.
          "${re-fs} wait-usb --timeout 30"
        ];
        ExecStart = "${re-fs} mount ${lib.escapeShellArgs ([ cfg.mountPoint "--usb" ] ++ cfg.extraArgs)}";
        # If re-fs was killed rather than stopped, don't leave a dead mount behind.
        ExecStopPost = "-/run/wrappers/bin/fusermount3 -uz ${lib.escapeShellArg cfg.mountPoint}";
        Restart = "on-failure";
        RestartSec = 3;
      };
      Install.WantedBy = [ device ];
    };
  };
}
