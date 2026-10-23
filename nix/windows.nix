# The client for Windows (x86_64, MinGW): ls, push, pair and deploy; no mount yet
# (that needs a WinFsp backend). Linked by nixpkgs' (cached) MinGW GCC, which also
# compiles ring's and zstd's C. Further Windows outputs (an installer, a WinFsp
# build) go here.
{ pkgs, craneLib, src, targets }:
let
  winPkgs = pkgs.pkgsCross.mingwW64;
  winCC = winPkgs.stdenv.cc;
  winCommon = {
    inherit src;
    strictDeps = true;
    pname = "re-fs-windows";
    cargoExtraArgs = "-p re-fs";
    CARGO_BUILD_TARGET = targets.windows;
    doCheck = false;
    nativeBuildInputs = [ winCC ];
    "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER" = "${winCC.targetPrefix}cc";
    "CC_x86_64_pc_windows_gnu" = "${winCC.targetPrefix}cc";
    "AR_x86_64_pc_windows_gnu" = "${winCC.targetPrefix}ar";
    "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS" = "-L native=${winPkgs.windows.pthreads}/lib";
  };
in
{
  re-fs-windows = craneLib.buildPackage (winCommon // {
    cargoArtifacts = craneLib.buildDepsOnly winCommon;
  });
}
