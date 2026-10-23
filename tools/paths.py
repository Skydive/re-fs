"""Expected re-fs paths for a mirror, computed independently of the Rust code.

Usage: python3 -I tools/paths.py MIRROR [--notebooks]  (trash and duplicates not modelled)
"""
import json, os, sys

d = sys.argv[1]
notebooks = "--notebooks" in sys.argv
M = {f[:-9]: json.load(open(os.path.join(d, f))) for f in os.listdir(d) if f.endswith(".metadata")}
def ftype(u):
    p = os.path.join(d, u + ".content")
    return json.load(open(p)).get("fileType") if os.path.exists(p) else None
ext = {"pdf": ".pdf", "epub": ".epub", "notebook": ".pdf", "": ".pdf", None: ".pdf"}
def path(u):
    parts = []
    while True:
        m = M[u]
        parts.append(m["visibleName"].replace("/", "∕"))
        p = m["parent"]
        if p == "": break
        if p == "trash": return None
        if p not in M or M[p]["type"] != "CollectionType": parts.append(".lost+found"); break
        u = p
    return "/".join(reversed(parts))
out = []
for u, m in M.items():
    if m.get("deleted"): continue
    if m["type"] == "CollectionType":
        p = path(u); p and out.append(p + "/")
    else:
        t = ftype(u)
        if t in ("notebook", "", None) and not notebooks: continue
        p = path(u)
        if p: out.append(p[: -len(ext[t])] + ext[t] if p.lower().endswith(ext[t]) else p + ext[t])
print("\n".join(sorted(out, key=lambda s: s.encode())))
