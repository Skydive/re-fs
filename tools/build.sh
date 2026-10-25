#!/bin/sh
# Build re-fs and the tablet agent without Nix (tested on Arch, Debian 13,
# Ubuntu 24.04, Fedora, openSUSE Tumbleweed and Alpine).
#
#   tools/build.sh                         # build into target/
#   tools/build.sh --prefix ~/.local       # … and install re-fs and its agent
#   sudo tools/build.sh --prefix /usr/local
#
# Builds the client (x86_64/aarch64 glibc) and the static rustls agent
# (armv7 musl, the same as `nix build .#re-fsd-armv7-rustls`), then the client
# again with RE_FSD_ARMV7 pointing at the installed agent, so a plain
# `re-fs deploy root@10.11.99.1 --service` works.
#
# Needs rustup, a C compiler, clang, curl and make:
#   Arch:          pacman -S --needed rustup base-devel clang curl
#   Debian/Ubuntu: apt-get install rustup build-essential clang curl
#   Fedora:        dnf install rustup gcc clang curl make
#   openSUSE:      zypper in rustup gcc clang curl make
#   Alpine:        apk add rustup build-base clang curl
# and at run time fuse3 (fusermount3) and openssh.
#
# The agent's C (ring, zstd) is compiled by clang for armv7 against musl's headers,
# which are fetched once (musl's own `make install-headers`, no compiler needed)
# into target/musl-headers; the result is linked by rust-lld into a static binary.
set -eu

MUSL_VERSION=1.2.5
MUSL_SHA256=a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4
ARM=armv7-unknown-linux-musleabihf
prefix=

die() { echo "build.sh: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; prefix=$2; shift 2 ;;
    -h|--help) sed -n '2,/^set -eu$/{/^#/p}' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# Fedora's and Alpine's `rustup` packages install only rustup-init, which puts
# rustup and cargo in ~/.cargo/bin.
PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
if ! command -v rustup >/dev/null && command -v rustup-init >/dev/null; then
  rustup-init -y --profile minimal --default-toolchain stable --no-modify-path
fi

for tool in cargo rustup cc clang curl make; do
  command -v "$tool" >/dev/null || die "$tool isn't on PATH (see tools/build.sh --help)"
done
# A fresh `rustup` package has no default toolchain yet.
if ! rustc --version >/dev/null 2>&1; then
  # Ubuntu 24.04's rustup 1.26 installs the toolchain (and makes it the default),
  # then fails a self-update check, so judge by whether rustc works afterwards.
  rustup toolchain install stable --profile minimal || true
  rustc --version >/dev/null 2>&1 || rustup default stable
  rustc --version >/dev/null 2>&1 || die "rustup couldn't install a Rust toolchain"
fi
rustup target list --installed | grep -qx "$ARM" || rustup target add "$ARM"

# musl's headers for 32-bit ARM.
headers="$root/target/musl-headers"
if [ ! -f "$headers/include/stdlib.h" ]; then
  echo "Fetching musl $MUSL_VERSION headers…"
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL --retry 3 -o "$tmp/musl.tar.gz" "https://musl.libc.org/releases/musl-$MUSL_VERSION.tar.gz"
  echo "$MUSL_SHA256  $tmp/musl.tar.gz" | sha256sum -c - >/dev/null || die "musl-$MUSL_VERSION.tar.gz: checksum mismatch"
  tar -xzf "$tmp/musl.tar.gz" -C "$tmp"
  make -s -C "$tmp/musl-$MUSL_VERSION" ARCH=arm prefix="$headers" install-headers
fi

ar=$(command -v llvm-ar || ls /usr/bin/llvm-ar-* 2>/dev/null | head -n1 || true)
[ -n "$ar" ] || ar=ar

echo "Building the tablet agent (re-fsd, static armv7 musl)…"
CC_armv7_unknown_linux_musleabihf=clang \
AR_armv7_unknown_linux_musleabihf="$ar" \
CFLAGS_armv7_unknown_linux_musleabihf="-nostdinc -isystem $headers/include -isystem $(clang -print-resource-dir)/include" \
  cargo build --release --locked -p re-fsd --no-default-features --features rustls --target "$ARM"
agent="$root/target/$ARM/release/re-fsd"

if [ -n "$prefix" ]; then
  mkdir -p "$prefix/bin" "$prefix/lib/re-fs"
  prefix=$(cd "$prefix" && pwd)
  install -m 755 "$agent" "$prefix/lib/re-fs/re-fsd"
  agent="$prefix/lib/re-fs/re-fsd"
fi

echo "Building the client (re-fs)…"
RE_FSD_ARMV7="$agent" cargo build --release --locked -p re-fs

if [ -n "$prefix" ]; then
  install -m 755 target/release/re-fs "$prefix/bin/re-fs"
  echo "Installed $prefix/bin/re-fs (deploys $agent)."
else
  echo "Built target/release/re-fs (deploys $agent)."
fi
