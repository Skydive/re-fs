import json, os, sys, collections as C
d, tsv = sys.argv[1], sys.argv[2]
rm = C.defaultdict(set)
for line in open(tsv):
    p = line.split('\t')[0]
    doc, f = p.split('/')
    if f.endswith('.rm'): rm[doc].add(f[:-3])
M = {f[:-9]: json.load(open(os.path.join(d, f))) for f in os.listdir(d) if f.endswith('.metadata')}
stats = C.Counter(); ex = {}
for u, m in M.items():
    if m['type'] != 'DocumentType': continue
    c = json.load(open(os.path.join(d, u + '.content')))
    if 'cPages' in c:
        fmt = 'v2'
        pg = [p for p in c['cPages']['pages'] if not p.get('deleted', {}).get('value')]
        dels = len(c['cPages']['pages']) - len(pg)
        pg.sort(key=lambda p: p['idx']['value'])
        ids = [p['id'] for p in pg]
        redir = [p.get('redir', {}).get('value') for p in pg]
        stats['v2 deleted page entries'] += dels
        stats['v2 page keys ' + ','.join(sorted(pg[0])) if pg else 'v2 empty'] += 1
    elif 'pages' in c:
        fmt = 'v1'; ids = c['pages']; redir = c.get('redirectionPageMap')
    else:
        fmt = 'none'; ids = []; redir = None
    have = rm.get(u, set())
    stats[(fmt, c['fileType'])] += 1
    stats[fmt + ' pages listed'] += len(ids)
    stats[fmt + ' pages with .rm'] += len(set(ids) & have)
    stats[fmt + ' .rm not listed'] += len(have - set(ids))
    if c['fileType'] == 'pdf' and redir:
        stats[fmt + ' pdf inserted blank pages (redir -1/None)'] += sum(1 for r in redir if r in (-1, None))
    if fmt == 'none' and have: ex.setdefault('none-with-rm', (m['visibleName'], c['fileType'], len(have), c.get('pageCount')))
    if fmt == 'v2' and c['fileType'] == 'pdf': ex.setdefault('v2pdf', (m['visibleName'], pg[:2]))
for k, v in sorted(stats.items(), key=str): print(k, v)
for k, v in ex.items(): print(k, json.dumps(v)[:900])
