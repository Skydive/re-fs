import json, os, sys, collections as C
d = sys.argv[1]
M = {f[:-9]: json.load(open(os.path.join(d, f))) for f in os.listdir(d) if f.endswith('.metadata')}
ft = {u: json.load(open(os.path.join(d, u + '.content'))).get('fileType') for u in M if os.path.exists(os.path.join(d, u + '.content'))}
kids = C.defaultdict(list)
for u, m in M.items(): kids[m['parent']].append(u)
ext = {'pdf': '.pdf', 'epub': '.epub', 'notebook': '.pdf'}
def show(p, ind):
    fs = sorted((u for u in kids[p] if M[u]['type'] == 'CollectionType'), key=lambda u: M[u]['visibleName'].lower())
    ds = sorted((u for u in kids[p] if M[u]['type'] != 'CollectionType'), key=lambda u: M[u]['visibleName'].lower())
    for u in fs:
        print(f"{ind}{M[u]['visibleName']}/"); show(u, ind + '  ')
    for u in ds:
        print(f"{ind}{M[u]['visibleName'].replace('/', '∕')}{ext[ft[u]]}  [{ft[u]}]")
show('', '')
print(f"\n(trash: {len(kids['trash'])} items)")
