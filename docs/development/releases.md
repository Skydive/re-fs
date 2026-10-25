# Releases

How re-fs is released: what CI builds, what a release contains, and how to cut one.

## Cutting a release

The version lives only in `Cargo.toml` (`[workspace.package] version`); the flake
reads it (`lib.x86_64-linux.version`), and so do the package names and
`re-fs --version`.

1. Set the version in `Cargo.toml` and run `cargo update --workspace` to sync
   `Cargo.lock`.
2. Commit and push to `master`.
3. Tag the commit `v<version>` (or `release/<version>`) and push the tag on its own:

   ```sh
   git tag -a v0.2.0 -m "re-fs 0.2.0"
   git push github v0.2.0
   ```

CI checks that the tag matches `Cargo.toml` and fails the run if not. Then it builds
everything and publishes a GitHub release named after the tag, with the files below
attached. A tag pushed in the same `git push` as its branch didn't start a run here
once; pushing it separately has worked.

## What CI builds

There's one build of each agent, made by Nix in CI (`.github/workflows/build.yml`,
on a GitHub x86_64 runner). Users of other distributions download it instead of
cross-compiling:

```
nix build .#re-fs-static           # the client as a static x86_64 musl binary, agents built in
nix build .#dist                   # that client as .tar.gz, .deb, .rpm, Arch package; the four agents; SHA256SUMS
```

- **The agents are built into the client.** `crates/re-fs/build.rs` reads the files
  that `RE_FSD_ARMV7` and `RE_FSD_AARCH64` name and embeds them with
  `include_bytes!`. A copied `re-fs` therefore deploys without any agent files or
  store paths. An unset variable embeds nothing, and `deploy` then needs `--binary`.
  This costs about 3.3 MB of client size.
- **The portable client is static.** `re-fs-static` targets
  `x86_64-unknown-linux-musl`, like the rustls agents: ring's and zstd's C is
  compiled by the cached GCC against `pkgs.musl.dev`'s headers, and rust-lld links it
  self-contained. The build fails if the result has an `INTERP` header. The client
  needs no C libraries at run time (fuser calls `fusermount3`), so there's nothing
  to patchelf and no `nix bundle`.
- **Packages come from nfpm.** `nix/dist.nix` describes one package (`/usr/bin/re-fs`,
  licence, README, `/usr/share/re-fs/host/` with the USB auto-mount rule and script)
  with per-format dependencies: `fuse3` plus `openssh-client` (deb), `openssh-clients`
  (rpm) or `openssh` (Arch).

CI runs `nix flake check` (tests, clippy, all agents, the static and Windows
clients), then `nix build .#dist .#re-fs-windows`, and uploads the files. The cross
builds need no ARM runners. nixpkgs' binary cache has the ARM GCCs, OpenSSL, zstd and
glibc. fenix's toolchains, including the ARM and musl standard libraries, are
prebuilt downloads from static.rust-lang.org. So only re-fs's crates compile.
`nix-community/cache-nix-action` keeps the store in the GitHub Actions cache,
keyed by the lock files and `nix/`.

## Release files

| File | What it is |
|---|---|
| `re-fs_*_amd64.deb`, `re-fs-*.x86_64.rpm`, `re-fs-*-x86_64.pkg.tar.zst` | the static client, packaged; depend on fuse3 and an ssh client |
| `re-fs-*-x86_64-linux.tar.gz` | the static client with LICENSE, README and `host/` (USB auto-mount files) |
| `re-fs-windows-x86_64.exe` | the Windows client, agents built in |
| `re-fsd-armv7`, `re-fsd-aarch64` | the default (OpenSSL) agents |
| `re-fsd-armv7-rustls`, `re-fsd-aarch64-rustls` | the static agents |
| `SHA256SUMS` | checksums |

Every CI run, not only tagged ones, uploads the same files as the `re-fs` artifact
of that run (kept 90 days).
