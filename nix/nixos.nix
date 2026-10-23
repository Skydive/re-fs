# NixOS side of re-fs's USB auto-mount: give the reMarkable's USB network
# interface a fixed systemd name, sys-subsystem-net-devices-remarkable.device,
# so the home-manager service (home-manager.nix) can start when the tablet is
# plugged in and stop when it's unplugged.
{ config, lib, ... }:
let
  cfg = config.services.re-fs.usbDevice;
in
{
  options.services.re-fs.usbDevice = {
    enable = lib.mkEnableOption "a systemd device name for the reMarkable's USB network (for re-fs's USB auto-mount)";
    vendorId = lib.mkOption {
      type = lib.types.str;
      default = "04b3";
      description = "USB vendor id of the tablet's network gadget (check with `udevadm info /sys/class/net/<iface>`).";
    };
    productId = lib.mkOption {
      type = lib.types.str;
      default = "4010";
      description = "USB product id of the tablet's network gadget.";
    };
    users = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "alice" ];
      description = ''
        Users who get /run/media/<user>/reMarkable, the home-manager module's default
        mount point (where file managers list removable media). /run is cleared at
        boot and /run/media/<user> belongs to root, so it's created here.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.tmpfiles.rules = map (u: "d /run/media/${u}/reMarkable 0700 ${u} - - -") cfg.users;
    services.udev.extraRules = ''
      # reMarkable USB network (RNDIS/Ethernet gadget) -> sys-subsystem-net-devices-remarkable.device
      SUBSYSTEM=="net", ATTRS{idVendor}=="${cfg.vendorId}", ATTRS{idProduct}=="${cfg.productId}", TAG+="systemd", ENV{SYSTEMD_ALIAS}+="/sys/subsystem/net/devices/remarkable"
    '';
  };
}
