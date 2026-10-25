# What a release ships for other distributions: the client as one static x86_64
# musl binary (no glibc or Nix store paths, so it runs on Debian, Arch, Fedora, …)
# with both tablet agents built in, the agents on their own, and the client
# packaged by nfpm as .tar.gz, .deb, .rpm and an Arch package.
{ pkgs, craneLib, src, targets, agents }:
let
  version = (craneLib.crateNameFromCargoToml { cargoToml = ../Cargo.toml; }).version;

  # ring's and zstd's C are compiled by the (cached) glibc GCC against musl's
  # headers, as for the rustls agents; rust-lld links (.cargo/config.toml).
  staticCommon = {
    inherit src;
    strictDeps = true;
    pname = "re-fs-static";
    cargoExtraArgs = "-p re-fs";
    CARGO_BUILD_TARGET = targets.linuxMusl;
    doCheck = false;
    "CC_x86_64_unknown_linux_musl" = "cc";
    preBuild = ''
      export CFLAGS_x86_64_unknown_linux_musl="-nostdinc -isystem ${pkgs.musl.dev}/include -isystem $(cc -print-file-name=include)"
    '';
  };
  re-fs-static = craneLib.buildPackage (staticCommon // {
    cargoArtifacts = craneLib.buildDepsOnly staticCommon;
    RE_FSD_ARMV7 = "${agents.armv7}/bin/re-fsd";
    RE_FSD_AARCH64 = "${agents.aarch64}/bin/re-fsd";
    # Fail the build if anything links dynamically.
    postInstall = ''
      if ${pkgs.binutils}/bin/readelf -l $out/bin/re-fs | grep -q INTERP; then
        echo "re-fs isn't static" >&2; exit 1
      fi
    '';
  });

  # Runtime needs per distribution: fusermount3 and ssh.
  nfpmConfig = pkgs.writeText "nfpm.yaml" (builtins.toJSON {
    name = "re-fs";
    arch = "amd64";
    platform = "linux";
    inherit version;
    release = "1";
    maintainer = "re-fs developers";
    description = "Your reMarkable's documents as an ordinary folder tree (FUSE)";
    homepage = "https://github.com/Skydive/re-fs";
    license = "MIT";
    contents = [
      { src = "${re-fs-static}/bin/re-fs"; dst = "/usr/bin/re-fs"; file_info.mode = 493; }
      { src = "${../LICENSE}"; dst = "/usr/share/licenses/re-fs/LICENSE"; }
      { src = "${../README.md}"; dst = "/usr/share/doc/re-fs/README.md"; }
      # USB auto-mount: run /usr/share/re-fs/host/install-usb-automount.sh as the user.
      { src = "${../deploy/host/70-re-fs-remarkable.rules}"; dst = "/usr/share/re-fs/host/70-re-fs-remarkable.rules"; }
      { src = "${../deploy/host/install-usb-automount.sh}"; dst = "/usr/share/re-fs/host/install-usb-automount.sh"; file_info.mode = 493; }
    ];
    overrides = {
      deb.depends = [ "fuse3" "openssh-client" ];
      rpm.depends = [ "fuse3" "openssh-clients" ];
      archlinux.depends = [ "fuse3" "openssh" ];
    };
  });

  dist = pkgs.runCommand "re-fs-dist-${version}" { nativeBuildInputs = [ pkgs.nfpm ]; } ''
    mkdir -p $out
    export SOURCE_DATE_EPOCH=315532800
    for p in deb rpm archlinux; do
      nfpm package --config ${nfpmConfig} --packager $p --target $out/
    done
    t=re-fs-${version}-x86_64-linux
    mkdir -p $t/host
    cp ${re-fs-static}/bin/re-fs ${../LICENSE} ${../README.md} $t/
    cp ${../deploy/host/70-re-fs-remarkable.rules} ${../deploy/host/install-usb-automount.sh} $t/host/
    chmod -R u+w $t
    tar --sort=name --mtime=@$SOURCE_DATE_EPOCH --owner=0 --group=0 -czf $out/$t.tar.gz $t
    install -m 755 ${agents.armv7}/bin/re-fsd $out/re-fsd-armv7
    install -m 755 ${agents.armv7-rustls}/bin/re-fsd $out/re-fsd-armv7-rustls
    install -m 755 ${agents.aarch64}/bin/re-fsd $out/re-fsd-aarch64
    (cd $out && sha256sum * > SHA256SUMS)
  '';
in
{
  inherit re-fs-static dist;
}
