import json, os, sys, collections as C
d = sys.argv[1]
M = {}; Ct = {}
for f in os.listdir(d):
    u, ext = os.path.splitext(f)
    if ext == '.metadata': M[u] = json.load(open(os.path.join(d, f)))
    if ext == '.content':
        try: Ct[u] = json.load(open(os.path.join(d, f)))
        except Exception as e: Ct[u] = {'_err': str(e)}
print('metadata keys:', C.Counter(k for m in M.values() for k in m))
print('types:', C.Counter(m['type'] for m in M.values()))
print('deleted:', C.Counter(m.get('deleted') for m in M.values()))
par = C.Counter(m['parent'] if m['parent'] in ('', 'trash') else ('<folder>' if m['parent'] in M else '<MISSING>') for m in M.values())
print('parent kinds:', par)
print('parent targets that are not CollectionType:', sum(1 for m in M.values() if m['parent'] in M and M[m['parent']]['type'] != 'CollectionType'))
print('content keys:', C.Counter(k for c in Ct.values() for k in c))
print('fileType by type:', C.Counter((M[u]['type'], c.get('fileType')) for u, c in Ct.items() if u in M))
print('formatVersion:', C.Counter(c.get('formatVersion') for c in Ct.values()))
print('content w/o metadata:', [u for u in Ct if u not in M], 'metadata w/o content:', [(u, M[u]['type']) for u in M if u not in Ct])
sib = C.defaultdict(list)
for u, m in M.items(): sib[(m['parent'], m['visibleName'])].append(u)
print('name collisions:', [(M.get(p, {}).get('visibleName', p), n, len(v)) for (p, n), v in sib.items() if len(v) > 1])
print('names with / :', [m['visibleName'] for m in M.values() if '/' in m['visibleName']][:10], sum('/' in m['visibleName'] for m in M.values()))
print('other odd chars:', sorted({ch for m in M.values() for ch in m['visibleName'] if not (ch.isalnum() or ch in ' -_.,()[]&+\'')}))
def depth(u, s=0):
    p = M[u]['parent']; return s if p in ('', 'trash') or p not in M else depth(p, s + 1)
print('max depth:', max(depth(u) for u in M))
print('top-level:', sorted((m['type'][0], m['visibleName']) for m in M.values() if m['parent'] == ''))
