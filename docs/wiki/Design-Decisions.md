# Design decisions

Every significant decision, in roughly the order it was made, with the alternatives
and the reason. "User" means the project owner's explicit call.

## Scope and shape

### D1. Build a FUSE view, not a sync tool
- **Context:** cloud sync through rmfakecloud was failing ("Unable to sync"); the
  goal was to browse the tablet's documents as real files.
- **Decision:** a FUSE filesystem showing the real folder tree.
- **Alternatives:** fix sync; use the USB web interface; copy files with scripts.
- **Why:** a mount works with every tool (file managers, `cp`, viewers) and doesn't
  depend on the cloud path.

### D2. Rust, not Python
- **Alternatives:** Python with pyfuse3/fusepy (the first idea).
- **Decision (user):** Rust.
- **Why:** one fast static binary for the tablet, the same code for the format on
  both ends, and no interpreter on a 1 GB ARM device.

### D3. A custom agent on the tablet, not sshfs
- **Alternatives:** plain sshfs (works today); a Rust sshfs.
- **Decision (user):** an agent (`re-fsd`) plus a client (`re-fs`) with their own
  protocol.
- **Why:** sshfs doesn't know the layout (every `ls` is a round trip over flat UUID
  files, and names need JSON). With an agent, the client keeps the whole index in
  memory (browsing at local speed), gets change events pushed by inotify, and caches
  files across mounts. The transport can also skip SSH's encryption where a cable or
  VPN already provides it.

### D4. Three crates, later four
- `re-proto` (shared, no I/O), `re-fsd` (agent), `re-fs` (client), then `re-render`
  (strokes → PDF). Names chosen by the user.
- **Why:** the format, tree, naming and protocol can be tested without a tablet or a
  network, and both ends share one implementation.

### D5. Folder structure first, then PDF/EPUB, then strokes
- **Decision (user):** order the work so the most useful, least risky parts come
  first.

## The agent

### D6. mio, not tokio, on the tablet
- **Decision:** one thread, one `mio::Poll` over listeners, connections, inotify and
  a timeout.
- **Alternatives:** tokio (used on the client).
- **Why:** one client at a time on a small device; a runtime adds size and threads
  and hides the raw file descriptors `sendfile` needs. The loop is a few hundred
  lines.

### D7. Sans-io protocol crate
- **Decision:** `re-proto` turns bytes into frames and back but never does I/O.
- **Why:** mio (agent), tokio (client) and unit tests (one byte at a time) drive the
  same code.

### D8. The agent ships raw JSON
- **Decision:** entries carry the raw `.metadata`/`.content` bytes; the client
  parses.
- **Why:** format changes on the tablet only need a client update; the agent stays
  small and JSON-free (even plan preconditions compare file mtime and size, not
  JSON).

### D9. Generations and deltas
- **Decision:** a per-run `instance` plus a `generation` counter; `GetIndex{since}`
  returns only what changed.
- **Why:** reconnects after the tablet sleeps send a few hundred bytes, not 1.3 MB.

### D10. inotify with a 200 ms debounce (1 s cap)
- **Why:** xochitl writes files in bursts; one rescan per burst. A rename showed up in
  the mount ~0.3 s later.

### D11. `sendfile` for file data
- **Why:** zero-copy from the tablet's disk to the socket. TLS links can't use it;
  they stream 64 KiB chunks through the TLS session instead.

### D12. `--stdio` mode
- **Why:** `ssh HOST re-fsd --stdio` gives an SSH fallback with the same protocol, no
  second implementation, and no service needed.

### D13. Socket activation and idle exit
- **Decision (user asked for it):** systemd holds port 7745, starts re-fsd on the
  first connection (~0.3 s) and re-fsd exits after 5 idle minutes.
- **Alternatives:** an always-on service (the first version).
- **Why:** nothing runs on the tablet when unused (battery).

## Compression

### D14. lz4 on the wire, zstd on disk
- **Decision:** the index travels as lz4 (`lz4_flex`); the client's saved index is
  `index.zstd` (user's request).
- **Alternatives:** zstd on the wire (the first plan).
- **Why:** lz4_flex is pure Rust (no C in the armv7 build) and much faster on a
  Cortex-A7; JSON still shrinks 10 MB → 1.3 MB. On the x86 client zstd's better ratio
  costs nothing.

## The tree

### D15. Rebuild the tree on every change
- **Alternatives:** patch it incrementally.
- **Why:** 380 items rebuild in ~3 ms (after D16); a full rebuild can't drift.
  `Tree::diff` tells the kernel what to forget.

### D16. Parse only what the tree needs from `.content`
- **Why:** parsing every page list made each rebuild ~50 ms; reading just
  `fileType`, `pageCount` and `tags` made it 2.9 ms.

### D17. Stable inodes from UUIDs
- **Why:** file managers and editors behave when a document keeps its inode across
  remounts. Views get salted inodes; main-tree documents kept theirs when views were
  added.

### D18. Naming rules
- `/` → `∕` (U+2215), since `/` can't be in a filename (real names: `01/02/24`).
- The type's extension is added only if missing: 34 of 329 documents already end in
  `.pdf`/`.epub` (found on real data as `….pdf.pdf`).
- Clashes numbered ` (2)`, oldest first, so the plain name doesn't move between
  rebuilds.
- Trailing spaces and other characters kept, to match the tablet.

### D19. Each document once, plus derived files and mirror views
- **Decision (user):** main tree shows `X.pdf` and `X.annotated.pdf` (when
  annotated), `X.highlights.md`, notebooks as `X.pdf`; mirrors `.originals/` and
  `.rmdoc/`; virtual `.recent/`, `.pinned/`, `.tags/<tag>/`; `user.rm.*` xattrs.
- **Alternatives:** a `.rmdoc` next to every file.
- **Why:** a clean main tree that still offers lossless copies and useful views.

## Writes

### D20. Trash, never delete
- **Decision:** `rm`/`rmdir` set `parent = "trash"`; no permanent delete from the
  mount.
- **Why:** recoverable from the tablet; file managers' delete is safe too.

### D21. Plans with preconditions
- **Decision:** writes are plans of file writes plus "must not exist / must be
  unchanged" checks, built by pure functions; the agent applies them atomically (data,
  then `.content`, then `.metadata`, each via rename).
- **Why:** never overwrite an edit made on the tablet; an item appears only when
  complete; every mapping is unit-tested.

### D22. Debounced xochitl restart
- **Why:** xochitl only notices outside files after a restart; restarting once, 3 s
  after the last write, keeps a batch of copies to one UI reload.

### D23. Upload on `close()`
- **Why:** `cp` sees failures as close's return value; `release` errors are lost.

### D24. No overwriting documents; renaming to another extension allowed
- Overwriting would misalign annotations, so it's refused.
- Renaming `x.pdf` to `notes.epub` was first refused; the user asked for it to work,
  so the text is kept in the name and the type's extension still shown
  (`notes.epub.pdf`).

## Transports and security

### D25. Try transports in order at connect; never switch mid-session
- **Decision (user):** USB (if plugged in), `--tls`, `--tcp`, `--ssh`, in order, at
  connect time only.
- **Rejected (user):** dropping a working link to switch to USB when the cable
  appears.

### D26. USB detection
- **Decision:** a USB-backed network interface with a 10.11.99.x address.
- **Learned:** the tablet's DHCP gives a /27 (seen `10.11.99.7`), not a /29.

### D27. Wi-Fi access: from blocked to TLS with ssh keys
1. First version: Wi-Fi peers refused, because the protocol had no authentication.
   **User rejected** ("why are Wi-Fi peers blocked").
2. An ssh-key challenge login inside the protocol (agent sends a challenge, client
   signs with its ed25519 key, agent checks `authorized_keys`). Worked, but
   authentication only.
3. One-time tickets minted over `ssh … re-fsd ticket`. **User rejected** as a "weird
   tickets thing"; they wanted it automatic inside the connection.
4. Rolling our own ChaCha20-Poly1305 framing on top of the login was proposed.
   **User preferred TLS** as more reliable and safer than a home-made protocol.
5. **Decision:** TLS 1.3 where both identities are ed25519 keys: the client's
   certificate is made from `~/.ssh/id_ed25519` and must be in `authorized_keys`; the
   tablet's key is pinned once over ssh (`re-fs pair`). Plain TCP only from USB and
   loopback (WireGuard via wireproxy). XOR with a shared secret was explicitly ruled
   out as insecure.
- **Why not reuse Dropbear's host key:** cross-protocol key reuse is discouraged; a
  separate key costs nothing.

### D28. rustls + ring, cross-compiled without zig
- **Decision (user):** rustls with ring.
- **Problem:** ring has C and assembly, breaking the "no C cross compiler" build.
- **Rejected (user):** a zig dependency (cargo-zigbuild).
- **Considered:** prebuilt ring libraries (none exist); porting ring's C/assembly to
  inline Rust (not realistic); the pure-Rust crypto provider (less mature).
- **Decision:** compile ring's C/asm with nixpkgs' cached ARMv7 GCC against musl's
  headers, still linked by rust-lld into a static binary.

### D29. Prefer ChaCha20-Poly1305
- **Why:** the Cortex-A7 has no AES instructions and the x86 client prefers AES-GCM.
  Forcing ChaCha20 on the agent raised TLS over USB from 10 to 18–19 MiB/s.

### D30. Canonicalise addresses
- **Learned on the tablet:** systemd's socket is dual-stack, so IPv4 clients appeared
  as `::ffff:10.11.99.1` and plain TCP over USB was refused until addresses were
  canonicalised.

### D31. Optional OpenSSL backend
- **Decision (user asked for it):** a compile-time choice of TLS backend (cargo
  features `openssl` and `rustls`; Nix `re-fsd-armv7` / `re-fsd-armv7-rustls`,
  `re-fs` / `re-fs-rustls-agent`, `lib.agentFor`) linking the tablet's own OpenSSL
  3.5.6 instead of rustls + ring. OpenSSL then became the default.
- **Why:** binary size (788 KB vs 1.79 MB); the tablet already ships libssl and
  libcrypto, used by xochitl. It also avoids compiling ring's C for that build,
  without zig (rejected earlier).
- **How:** one buffer-oriented `tls::Session` with two implementations; OpenSSL runs
  over an in-memory pipe so the mio loop doesn't change; same `tls.key` format so the
  pin survives switching; a verify callback checks `authorized_keys`; ChaCha20 first,
  TLS 1.3 only. The binary is dynamic armv7 glibc against nixpkgs' cached ARMv7
  OpenSSL/glibc, interpreter `/lib/ld-linux-armhf.so.3`, no rpath, highest symbol
  `GLIBC_2.39` (the tablet has 2.39).
- **Trade-off:** depends on the tablet's glibc/OpenSSL across OS updates; the static
  rustls build (still available) depends on nothing.
- **Measured:** TLS over USB 23 MiB/s with OpenSSL vs 19 MiB/s with rustls.

## Reading and caching

### D32. Whole-file background download, ranged reads for far seeks
- **Why:** first reads wait only for their range; seeking deep into a large PDF
  doesn't wait for the beginning; later opens are local (16 ms for 16 MB).

### D33. Errors at `open`, not `read`
- **Learned:** read errors pass through the page cache and reach programs as `EIO`,
  so "Host is down" (offline, not cached) is returned at `open`.

### D34. Generated files: made on open, fingerprinted, direct I/O
- **Why:** rendering only what's opened; remade when sources change; their size isn't
  known up front, so direct I/O avoids reads being cut at a stale size, and the inode
  is invalidated afterwards.

### D35. Offline mount from the saved index
- **Why:** the tablet sleeps often; cached files stay usable.

## Rendering

### D36. A pure rendering crate with lopdf
- **Why:** testable with real `.rm` files and no I/O; lopdf both writes notebooks and
  edits existing PDFs.

### D37. Stroke placement on PDF pages: physical 226 dpi
- First guess: page fitted to the whole screen, centred. Second: fitted to the
  screen's width (from a thumbnail showing scrolling). **Both were wrong** when
  compared with xochitl's thumbnails of a real annotated A4 page.
- **Decision:** strokes are stored at 226 dpi with x relative to the page's centre and
  y from its top, independent of zoom; this matched stroke for stroke.

### D38. Grow notebook and inserted pages
- **Learned:** a real inserted page had strokes from y = −245 to 2266. Pages grow to
  hold every stroke.

### D39. Don't draw v6 eraser paths
- **Learned:** drawing them white blanked out PDF text where the tablet shows none.
  In v6, erasing already removed the strokes; v5's eraser still paints white.

### D40. Repair broken PDFs like viewers do
- **Learned:** a real PDF had a BOM added after writing, offsets that ignored it, and
  a truncated `%%E`. re-fs tries, in order: as is, junk stripped, header moved with
  offsets kept, `%%EOF` completed, and finally a rebuilt cross-reference table.

## Build

### D41. Static musl agent linked by rust-lld
- **Why:** independent of the tablet's glibc and survives OS updates; no C cross
  toolchain for pure-Rust builds. Since D31 this is the rustls build; the default
  OpenSSL build is dynamic glibc by design.

### D42. fuser without libfuse
- **Learned:** with libfuse the binary needed `libfuse3.so` at run time; the pure-Rust
  mount via `fusermount3` needs nothing.

### D43. The client embeds its agent
- **Why:** `re-fs deploy` installs exactly the agent it was built with (protocol
  versions match).
