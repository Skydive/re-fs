# TODO

## Windows, step 2: mount with WinFsp

**Status:** written and cross-compiling (`nix build .#re-fs-windows`), clippy-clean,
naming and tree tests pass on Linux. **Not yet run on Windows**: the "Done when"
checks below are the test plan. How it came out, where it differs from this plan:

- Bindings: option B, but loaded with `windows-sys` (`LoadLibraryW` from WinFsp's
  `InstallDir`) rather than `libloading`. Struct layouts are checked against the
  header sizes at compile time (`mount/winfsp/ffi.rs`).
- Invalidation by path: `Invalidate` got a default method, `tree_changed(old, new,
  inval)`; FUSE keeps the default, WinFsp turns entries into `FspFileSystemNotify`
  paths using the old tree for what went away.
- Deletes use `CanDelete` (checks) plus `Cleanup` with the delete flag (the trash).
- No prefetch on open: the first read downloads, so Explorer listing a folder (or
  opening files only for their attributes) fetches nothing.
- Auto-mount: `mount --follow-usb` (any platform) mounts while the USB link is up,
  unmounts 5 s after it goes, and waits again. `re-fs autostart` registers it to run
  at login (HKCU Run key) through `re-fsw.exe`, a windowless launcher logging to
  `%LOCALAPPDATA%\re-fs\re-fs.log`.

Step 1 is done: the client builds for Windows (`nix build .#re-fs-windows`), and
`ls`, `push`, `pair` and `deploy` work there (tested under Wine). `mount` says it
isn't supported yet. This step adds a mount backend on
[WinFsp](https://winfsp.dev), so the tablet appears as a drive (`R:`) or a folder
in Explorer, first read-only, then with writes.

### Why WinFsp

It's the closest thing Windows has to FUSE: the filesystem answers open, read,
directory listing and file-info calls, and can tell the OS when something changed
(`FspFileSystemNotify`). The current design maps over almost one to one. rclone and
sshfs-win use it. Users install the WinFsp driver once (an MSI from winfsp.dev).

Rejected for now: the Cloud Files API (OneDrive-style placeholders; native Explorer
integration, but a sync model in which re-fs would reconcile edits instead of
applying them live), ProjFS (read-mostly projection into a real folder; awkward
for writes), and Dokan (similar to WinFsp, smaller user base).

### Decisions to make first

- [x] **Bindings.** Option A: the `winfsp` crate (winfsp-rs). It's the easy path but
      expects an MSVC build and the WinFsp SDK at build time, which doesn't fit
      cross-compiling with MinGW from NixOS. Option B: our own small FFI to
      `winfsp-x64.dll`'s native API (`FspFileSystemCreate`,
      `FspFileSystemSetMountPoint`, `FspFileSystemStartDispatcher`, the
      `FSP_FILE_SYSTEM_INTERFACE` callbacks), loaded at run time with `libloading`.
      That needs no SDK at build time, keeps the MinGW cross-build, and gives a clear
      error when WinFsp isn't installed. **Leaning B**; prototype `GetVolumeInfo`,
      `Open` and `ReadDirectory` first to check the cost.
- [ ] **Test machine.** WinFsp doesn't run under Wine. We need a Windows 10/11
      machine or VM reachable over SSH (e.g. QEMU/quickemu on NixOS), with WinFsp
      and OpenSSH installed.

### W2.1 Groundwork (portable, tested on Linux)

- [x] `crates/re-fs/src/mount/`: move `fs.rs` to `mount/fuse.rs`, add
      `mount/winfsp.rs` behind `cfg(windows)`, and give `main.rs` one `mount`
      entry point that picks the backend.
- [x] **Windows file names** (`re_proto::index::naming`, a Windows mode chosen by
      the backend, with unit tests that run on Linux):
  - [x] Show `< > : " \ | ? *` as look-alikes (`＜ ＞ ꞉ ＂ ⧵ ｜ ？ ＊`) and map them back
        on rename.
  - [x] Keep trailing spaces and dots, which Windows strips, by showing them as
        U+2002 / U+2024 (the tablet has folders ending in a space).
  - [x] Reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`,
        `LPT1`–`LPT9`, with or without an extension) get a ` _` suffix.
  - [x] Detect name clashes case-insensitively (the volume is case-insensitive),
        so `Notes.pdf` and `notes.pdf` number like any other clash.
  - [x] Keep each name ≤ 255 UTF-16 code units (Windows counts UTF-16, not bytes).
- [x] **Paths for invalidation.** `session::Invalidate` is inode-based; WinFsp
      notifications take paths. Extend the trait (or `Tree::diff`) to also give the
      old and new paths, so a backend can notify on either.

### W2.2 Read-only mount

- [x] Callbacks: `GetVolumeInfo` (label "reMarkable", sizes as in `statfs`),
      `GetSecurityByName` / `GetSecurity` (read-only for the current user),
      `Open` / `Close`, `GetFileInfo`, `ReadDirectory` (with marker-based
      continuation), `Read`.
- [x] File context = the tree node's inode plus, for generated files, their
      `GenKey`. Attributes: `FILE_ATTRIBUTE_DIRECTORY` or `READONLY`; times as
      `FILETIME` from the tree's ms timestamps.
- [x] Reads go through the existing `Cache` (originals) and `generate::ensure`
      (annotated PDFs, notebooks, `.rmdoc`, highlights). Generated files are made
      in `Open`, which may block, because WinFsp's dispatcher has a thread pool.
      Their size is then known before the first `GetFileInfo`, which avoids the
      0-byte problem FUSE has.
- [x] Offline: uncached files fail `Open` with `STATUS_HOST_UNREACHABLE` (the
      equivalent of EHOSTDOWN).
- [x] Invalidation: implement `session::Invalidate` with `FspFileSystemNotify`
      (`FILE_ACTION_ADDED` / `REMOVED` / `RENAMED_*` / `MODIFIED`), so Explorer
      refreshes when the tablet changes.
- [x] `re-fs mount R: --usb --tls tablet.lan`, or a folder mount point; Ctrl-C or
      closing the console unmounts.
- [ ] **Done when** Explorer browses the tree, PDFs open in Edge and a PDF viewer,
      copying files out works, `.annotated.pdf` and notebooks open on first try,
      and a rename on the tablet shows up in Explorer.

### W2.3 Writes (`--rw`)

- [x] `Create` (file): collect into a staging file like the FUSE `create`; upload
      on `Cleanup`/`Close` through `write::import`. Only PDFs and EPUBs; anything
      else fails with `STATUS_ACCESS_DENIED` and a log line.
- [x] `Create` (directory) → `write::mkdir`.
- [x] `Rename` → `write::rename` (Windows names mapped back as in W2.1).
- [x] Delete (`SetDelete` / `Cleanup` with the delete flag) → `write::trash`;
      non-empty folders refused.
- [x] `write.rs` is Unix-only today because of errnos: return a small error enum
      that each backend maps (errno for FUSE, NTSTATUS for WinFsp).
- [ ] **Done when** Explorer can create a folder, drag a PDF in, rename it and
      delete it, all with the same tablet-side results as on Linux.

### W2.4 Packaging and docs

- [ ] A Windows release zip with `re-fs.exe`, and the armv7 agent embedded
      (`include_bytes!`) so `re-fs deploy` works without `--binary`.
- [x] Detect a missing WinFsp at `mount` and point to the installer.
- [x] Optional: start at login (Task Scheduler) or register with WinFsp's launcher
      (`launchctl-x64`).
- [x] Docs: a Windows section in the README and Usage wiki page (install WinFsp,
      `%APPDATA%\re-fs`, `%LOCALAPPDATA%\re-fs`, `%USERPROFILE%\.ssh`).

### Risks

- The bindings decision (A vs B) and MinGW compatibility.
- Explorer probes files hard (thumbnails, previews, alternate data streams): make
  sure that doesn't trigger PDF generation or downloads for every file in a
  folder, e.g. generate only on a real read-data open.
- Antivirus scanners reading every opened file, which forces full downloads.
- Testing needs a real Windows VM; nothing in step 2 can be checked under Wine.

## Other open items

- Page templates (ruled lines) and typed text aren't drawn in rendered PDFs.
- Passphrase-protected SSH keys and ssh-agent for `--tls`.
- Importing `.rmdoc` (copying one in, with annotations).
- Trash: restore and purge.
- Refuse `--tcp`/`--tls` addresses that are this machine's own (a silent offline
  mount otherwise).
- Generated files show 0 bytes until first opened on Linux.
