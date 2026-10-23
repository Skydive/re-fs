#!/usr/bin/env bash
# End-to-end: the lossless round trip through a real mount, against a local
# re-fsd on a temporary xochitl directory (no tablet involved).
#
#   cargo build --release -p re-fs -p re-fsd
#   cargo build --release -p re-render --example notebook_page --example annotate_doc
#   tests/roundtrip.sh
#
# Copies X.annotated.pdf, a rendered notebook and a .rmdoc out of the mount and
# back in, and checks each new document has the same page list and
# byte-identical page files. Also: a plain PDF still imports as a PDF, and a
# broken .rmdoc is refused with EINVAL. Then the write modes: by default the
# mount is writable but never replaces a document; --overwrite replaces one (the
# old copy goes to the trash); --ro refuses writes.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
bin=$here/target/release
rm_fixture=$here/crates/re-render/tests/data/sample-v6.rm
t=$(mktemp -d)
x=$t/xochitl
m=$t/mnt
mkdir -p "$x" "$m" "$t/out"
port=$((20000 + RANDOM % 20000))

cleanup() {
    fusermount3 -u "$m" 2>/dev/null || fusermount -u "$m" 2>/dev/null || true
    [ -n "${fsd:-}" ] && kill "$fsd" 2>/dev/null || true
    [ -n "${fs:-}" ] && kill "$fs" 2>/dev/null || true
    wait 2>/dev/null || true
    rm -rf "$t"
}
trap cleanup EXIT

doc=aaaaaaaa-0000-4000-8000-000000000001
nb=aaaaaaaa-0000-4000-8000-000000000002
p1=bbbbbbbb-0000-4000-8000-000000000001
p2=bbbbbbbb-0000-4000-8000-000000000002
p3=bbbbbbbb-0000-4000-8000-000000000003
now=1700000000000

# A two-page PDF with strokes on page 1, a blank page inserted after it, and
# page 2 of the PDF last: three pages in the tablet's order.
"$bin/examples/notebook_page" "$x/$doc.pdf" "$rm_fixture" "$rm_fixture"
cat >"$x/$doc.metadata" <<EOF
{"type":"DocumentType","visibleName":"Paper","parent":"","lastModified":"$now","deleted":false}
EOF
cat >"$x/$doc.content" <<EOF
{"fileType":"pdf","formatVersion":2,"cPages":{"pages":[
 {"id":"$p1","idx":{"timestamp":"1:2","value":"ba"},"redir":{"timestamp":"1:1","value":0}},
 {"id":"$p2","idx":{"timestamp":"1:2","value":"bb"}},
 {"id":"$p3","idx":{"timestamp":"1:2","value":"bc"},"redir":{"timestamp":"1:1","value":1}}]}}
EOF
printf 'Blank\nBlank\nBlank\n' >"$x/$doc.pagedata"
mkdir -p "$x/$doc"
cp "$rm_fixture" "$x/$doc/$p1.rm"
echo '{"layers":[{"name":"Layer 1"}]}' >"$x/$doc/$p1-metadata.json"

cat >"$x/$nb.metadata" <<EOF
{"type":"DocumentType","visibleName":"Sketches","parent":"","lastModified":"$now","deleted":false}
EOF
cat >"$x/$nb.content" <<EOF
{"fileType":"notebook","formatVersion":2,"cPages":{"pages":[
 {"id":"$p1","idx":{"timestamp":"1:2","value":"ba"}},
 {"id":"$p2","idx":{"timestamp":"1:2","value":"bb"}}]}}
EOF
mkdir -p "$x/$nb"
cp "$rm_fixture" "$x/$nb/$p1.rm"
cp "$rm_fixture" "$x/$nb/$p2.rm"

"$bin/re-fsd" --dir "$x" --listen "127.0.0.1:$port" --allow-local 127.0.0.1 &
fsd=$!
for _ in $(seq 100); do (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null && break; sleep 0.1; done
remount() { # extra re-fs mount options; no options = the default (writable)
    if [ -n "${fs:-}" ]; then
        fusermount3 -u "$m" 2>/dev/null || fusermount -u "$m"
        wait "$fs" 2>/dev/null || true
    fi
    "$bin/re-fs" mount "$m" --tcp "127.0.0.1:$port" --cache-dir "$t/cache" "$@" &
    fs=$!
    for _ in $(seq 100); do [ -e "$m/Paper.pdf" ] && break; sleep 0.1; done
    [ -e "$m/Paper.pdf" ] || { echo "FAIL: mount didn't come up"; exit 1; }
}
remount

cp "$m/Paper.annotated.pdf" "$t/out/"
cp "$m/Sketches.pdf" "$t/out/"
cp "$m/.rmdoc/Paper.rmdoc" "$t/out/"
cp "$m/.originals/Paper.pdf" "$t/out/plain.pdf"
# The same annotated PDF without the attachment, for comparison.
"$bin/examples/annotate_doc" "$x/$doc.pdf" "$t/out/bare.pdf" "0:$rm_fixture" - 1
echo "sizes: original $(stat -c %s "$x/$doc.pdf"), annotated without .rmdoc $(stat -c %s "$t/out/bare.pdf"), with $(stat -c %s "$t/out/Paper.annotated.pdf"); .rmdoc $(stat -c %s "$t/out/Paper.rmdoc")"

cp "$t/out/Paper.annotated.pdf" "$m/Paper back.annotated.pdf"
cp "$t/out/Sketches.pdf" "$m/Sketches back.pdf"
cp "$t/out/Paper.rmdoc" "$m/Paper rmdoc.rmdoc"
cp "$t/out/plain.pdf" "$m/Plain.pdf"
printf 'PK\003\004not a zip' >"$t/out/bad.rmdoc"
if cp "$t/out/bad.rmdoc" "$m/Bad.rmdoc" 2>"$t/out/err"; then
    echo "FAIL: a broken .rmdoc was accepted"; exit 1
fi
grep -q 'Invalid argument' "$t/out/err" || { echo "FAIL: broken .rmdoc: $(cat "$t/out/err")"; exit 1; }

# Each copy is a new document with its source's pages, page files and type.
python3 - "$x" "$doc" "$nb" <<'EOF'
import json, os, sys
x, doc, nb = sys.argv[1:]
def meta(u): return json.load(open(f"{x}/{u}.metadata"))
by_name = {meta(f[:-9])["visibleName"]: f[:-9] for f in os.listdir(x) if f.endswith(".metadata")}
def pages(u): return [p["id"] for p in json.load(open(f"{x}/{u}.content"))["cPages"]["pages"]]
def folder(u):
    d = f"{x}/{u}"
    return {n: open(f"{d}/{n}", "rb").read() for n in os.listdir(d)} if os.path.isdir(d) else {}
ok = True
for name, src in [("Paper back", doc), ("Sketches back", nb), ("Paper rmdoc", doc)]:
    new = by_name.get(name)
    if new is None or new == src:
        print(f"FAIL: no new document {name!r}"); ok = False; continue
    checks = {
        "page list": pages(new) == pages(src),
        "page files": folder(new) == folder(src),
        "type": json.load(open(f"{x}/{new}.content"))["fileType"] == json.load(open(f"{x}/{src}.content"))["fileType"],
        "original": (not os.path.exists(f"{x}/{src}.pdf")) or open(f"{x}/{new}.pdf", "rb").read() == open(f"{x}/{src}.pdf", "rb").read(),
        "pagedata": (not os.path.exists(f"{x}/{src}.pagedata")) or open(f"{x}/{new}.pagedata", "rb").read() == open(f"{x}/{src}.pagedata", "rb").read(),
        "parent": meta(new)["parent"] == "",
    }
    bad = [k for k, v in checks.items() if not v]
    print(f"{'FAIL' if bad else 'ok'}: {name}: {len(pages(new))} pages, {len(folder(new))} page files" + (f"; differs: {bad}" if bad else ""))
    ok &= not bad
plain = by_name.get("Plain")
if plain is None or os.path.isdir(f"{x}/{plain}") or json.load(open(f"{x}/{plain}.content")).get("fileType") != "pdf":
    print("FAIL: Plain.pdf didn't import as a plain PDF"); ok = False
else:
    print("ok: Plain: a plain PDF, no page folder")
if "Bad" in by_name:
    print("FAIL: Bad.rmdoc made a document"); ok = False
sys.exit(0 if ok else 1)
EOF

# Write modes. The default refuses to replace a document, by cp or by mv.
if cp "$t/out/Sketches.pdf" "$m/Plain.pdf" 2>/dev/null; then echo "FAIL: cp replaced a document by default"; exit 1; fi
if mv "$m/Sketches back.pdf" "$m/Plain.pdf" 2>/dev/null; then echo "FAIL: mv replaced a document by default"; exit 1; fi
echo "ok: default mount refuses to overwrite (cp and mv)"

# --overwrite: the copy replaces the document and the old one goes to the trash.
remount --overwrite
cp "$t/out/Sketches.pdf" "$m/Plain.pdf"
mv "$m/Paper rmdoc.pdf" "$m/Paper back.pdf"
python3 - "$x" <<'EOF'
import json, os, sys
x = sys.argv[1]
docs = [(f[:-9], json.load(open(f"{x}/{f}"))) for f in os.listdir(x) if f.endswith(".metadata")]
def named(name, trash): return [u for u, m in docs if m["visibleName"] == name and (m["parent"] == "trash") == trash]
bad = []
plain = named("Plain", False)
if len(plain) != 1 or json.load(open(f"{x}/{plain[0]}.content"))["fileType"] != "notebook":
    bad.append(f"cp: live Plain {plain}")
if len(named("Plain", True)) != 1:
    bad.append("cp: the old Plain isn't in the trash")
if len(named("Paper back", False)) != 1 or len(named("Paper back", True)) != 1 or named("Paper rmdoc", False):
    bad.append("mv: expected one live and one trashed Paper back")
for b in bad:
    print(f"FAIL: --overwrite {b}")
if not bad:
    print("ok: --overwrite replaces (cp and mv), old copies in the trash")
sys.exit(1 if bad else 0)
EOF

# --ro: nothing can be written.
remount --ro
if mkdir "$m/Nope" 2>/dev/null || cp "$t/out/plain.pdf" "$m/Nope.pdf" 2>/dev/null; then echo "FAIL: --ro allowed a write"; exit 1; fi
echo "ok: --ro refuses writes"
echo "roundtrip: all ok"
