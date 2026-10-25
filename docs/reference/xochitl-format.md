# xochitl on-disk format (OS 3.x)

A one-page summary for working on the code; the full description is
[reMarkable storage format](../internals/storage-format.md).

Measured on a reMarkable 2 (OS build 20260827113527) on 2026-10-23, read-only over SSH.

## Location

`/home/root/.local/share/remarkable/xochitl/`, one flat directory. Every folder and document is a
group of files sharing a UUID prefix. The only subdirectories are `<uuid>/` (pages) and
`<uuid>.thumbnails/`.

| File | Contents |
|---|---|
| `<uuid>.metadata` | `visibleName`, `type` (`CollectionType` folder / `DocumentType`), `parent`, `lastModified`, `deleted`, `pinned`, … |
| `<uuid>.content` | `fileType` (`pdf`, `epub`, `notebook`), page list and order, PDF page mapping, tags. Folders may have none. |
| `<uuid>/<page-id>.rm` | Strokes for one page; absent for pages without writing |
| `<uuid>/<page-id>-metadata.json` | Layer names |
| `<uuid>.pdf` / `<uuid>.epub` | Imported original (no annotations). EPUBs also have a `.pdf` next to them, probably the tablet's rendering |
| `.pagedata`, `.local`, `.thumbnails/`, `.epubindex`, `.textconversion`, `.highlights/` | Not needed for a read-only view |

## Tree

`parent` is a folder UUID, `""` (top level) or `"trash"`. Path = names joined while walking up.
On the surveyed tablet: no orphans, no document is a parent, max depth 4.

## Page order

- **v1** (`pages` list, `formatVersion` 1 or absent): list order is page order;
  `redirectionPageMap[i]` = original PDF page at position `i`.
- **v2** (`cPages.pages`, `formatVersion` 2): sort by `idx.value` (string, fractional index),
  skip entries with `deleted.value`; `redir.value` = original PDF page, missing = inserted blank page.

Every `.rm` file on disk is listed in its document's `.content`.

## Strokes

Header `reMarkable .lines file, version=N`. Surveyed: 1384 v5 pages (OS 2, untouched since) and
1330 v6. rmscene/rmc handle v6 only; v5 needs its own parser.

## Names

- `/` occurs (`01/02/24`, `A/B`), shown as `∕` (U+2215).
- Trailing spaces, `:`, NBSP and typographic punctuation occur; keep them.
- Duplicate names within one folder seen only in the trash; disambiguate with ` (2)`.
- Add `.pdf` (PDFs, rendered notebooks) / `.epub`, unless the name already ends with it:
  34 of 329 imported documents keep their filename (`x.pdf`, `y.epub`) as `visibleName`.

## Tablet quirks

BusyBox userland: no `head -c` (use `dd bs=N count=1`).
