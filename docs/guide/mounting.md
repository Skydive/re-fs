# Mounting

`re-fs mount` shows the tablet's documents as a folder tree. This page covers what
appears in the mount, how copying, moving and deleting change the tablet, and every
mount option.

```sh
mkdir -p ~/rm
re-fs mount ~/rm --usb            # stays in the foreground; Ctrl-C unmounts
fusermount3 -u ~/rm               # or unmount from another terminal
```

`--usb` is one of several [transports](connecting.md). To mount whenever the tablet is
plugged in, see [Auto-mount on USB](auto-mount.md).

## What's in the mount

Each document appears once, in its folder, named as on the tablet:

```
~/rm/Books/Paper.pdf               the PDF as imported
~/rm/Books/Paper.annotated.pdf     … with your strokes and highlights drawn on
~/rm/Books/Paper.highlights.md     … the highlighted text, as Markdown
~/rm/Books/Novel.epub              an EPUB
~/rm/Notes.pdf                     a notebook, rendered to PDF
```

and a few views, as hidden folders:

| Folder | What's in it |
|---|---|
| `.trash/` | the tablet's trash |
| `.originals/` | the folder tree again, with only the original PDFs and EPUBs |
| `.rmdoc/` | the folder tree again, every document as a lossless `.rmdoc` (the tablet's export format, annotations included) |
| `.recent/` | the 50 documents opened last (`ls -lt` sorts them) |
| `.pinned/` | your favourites |
| `.tags/<tag>/` | documents by tag |

`--hide-notebooks` leaves notebooks out and `--no-views` the hidden folders.

### Generated files

Annotated PDFs, rendered notebooks, `.rmdoc` files and `.highlights.md` are made the
first time you open them, then cached until the document changes. Until then they
show as 0 bytes. How they're drawn: [Rendering](../internals/rendering.md).

### Extended attributes

`getfattr -d FILE` shows a document's `user.rm.pages`, `user.rm.last_page`,
`user.rm.tags`, `user.rm.pinned` and `user.rm.uuid`.

### Live updates and offline use

A change made on the tablet shows up in the mount in about 0.3 s. Files you've
opened are cached in `~/.cache/re-fs`. If no transport answers when you mount but a
saved index exists, the mount comes up offline: cached files open, others fail with
"Host is down", and re-fs keeps reconnecting in the background. Details:
[Caching and offline](../internals/caching-and-offline.md).

## Changing the tablet

The mount is writable unless you give `--ro`:

| You do | On the tablet |
|---|---|
| `mkdir Books/New` | a new folder |
| copy a PDF, EPUB or `.rmdoc` in | the document is imported |
| `mv` a file or folder | it's moved or renamed |
| `rm`, `rmdir` | it goes to the tablet's trash; nothing is deleted for good |

Existing documents are never overwritten unless you mount with `--overwrite`. Then
copying or moving onto one replaces it, and the old one goes to the trash once the
new one is on the tablet. The tablet's reader (xochitl) restarts once, 3 s after the
last write, so it shows the changes. Only real PDFs, EPUBs and `.rmdoc` files can be
copied in; anything else fails with "Invalid argument".

### The lossless round trip

PDFs that re-fs makes (`X.annotated.pdf`, a notebook's `X.pdf`) carry the whole
document as an attached `.rmdoc`. Copying one back in restores the document, strokes
and page order included, instead of adding a flat PDF. Edits made to such a PDF in
another app aren't carried back (the attached original wins), and the PDF is larger
by roughly the original's size. Details:
[Writes and plans](../internals/writes-and-plans.md#the-lossless-round-trip).

## Options

```sh
re-fs mount MOUNTPOINT [transports…] [options]
```

| Option | Effect |
|---|---|
| `--ro` | read-only |
| `--overwrite` | let copying or moving onto an existing document replace it (the old one goes to the trash) |
| `--hide-notebooks` | leave notebooks out |
| `--no-views` | leave out `.originals`, `.rmdoc`, `.recent`, `.pinned` and `.tags` |
| `--cache-dir DIR` | where downloaded and generated files and the saved index live (default `~/.cache/re-fs`) |
| `--cache-mib N` | how large the cache may grow, in MiB (default 5120) |
| `--codec C` | compression for index and page replies: `auto` (by measured link speed, the default), `none`, `lz4` or `zstd` |
| `-v` | log connections, index updates, generated files and warnings |

Transport options (`--usb`, `--tls`, `--tcp`, `--ssh`, `--identity`, `--port`,
`--agent`) are on [Connecting](connecting.md). `--rw` is still accepted and does
nothing.
