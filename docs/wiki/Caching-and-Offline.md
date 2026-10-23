# Caching and offline

```
~/.cache/re-fs/objects/<uuid>.pdf@<mtime>        complete originals
~/.cache/re-fs/partial/<uuid>.pdf@<mtime>        downloads in progress
~/.cache/re-fs/generated/<uuid>.<kind>@<version> annotated PDFs, rendered notebooks, .rmdoc, highlights
~/.cache/re-fs/staging/                          files being copied in
~/.cache/re-fs/index.zstd                        the last index, for mounting offline
```

## Originals

- The first `open` of a PDF/EPUB starts one background download of the whole file:
  4 MiB reads, four in flight, written in order so the contiguous prefix on disk
  only grows.
- Reads wait only until their own range has arrived. A read more than 16 MiB past
  the download front is fetched on its own (seeking deep into a large PDF).
- Finished files move to `objects/` and are answered on the fuser thread with
  `pread`. Older versions of the same file are deleted.
- Files open with `FOPEN_KEEP_CACHE`, so the kernel's page cache survives reopening;
  a change on the tablet invalidates the inode.
- The cache is capped (`--cache-mib`, default 5120); least recently used files go
  first (opening a file touches its mtime).

## Generated files

Annotated PDFs, rendered notebooks, `.rmdoc` and `.highlights.md` are made on the
first `open`, keyed by a fingerprint of every file they're made from (sizes and
mtimes of the document's files and page files). A change on the tablet changes the
fingerprint, so they're remade next time.

- One generation per file at a time; others wait for it.
- Rendering runs off the async workers (`spawn_blocking`).
- Their size isn't known until made, so they show as 0 bytes, open with
  `FOPEN_DIRECT_IO` (reads aren't cut at the cached size), and the inode is
  invalidated afterwards so `ls -l` shows the real size.

## Offline

- After every change the index is saved as zstd over postcard (`index.zstd`, written
  off the critical path; ~770 KB for the tablet's 380 items).
- If no transport works at mount time and a saved index exists, the mount comes up
  read-only from it. Cached files open; anything else fails at `open` with "Host is
  down" (`EHOSTDOWN`).
- Errors are returned at `open` rather than `read` on purpose: read errors go through
  the page cache and reach programs as a generic `EIO`.
- The session keeps reconnecting with backoff (1 s doubling to 30 s); once
  connected it fetches the index (a full one, since the agent's instance changed or a
  delta otherwise) and the mount carries on.
