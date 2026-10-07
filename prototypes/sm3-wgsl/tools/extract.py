"""THROWAWAY extractor: pulls every SM3 VS/PS blob + technique context out of the stock MP zones.
usage: python3 extract.py <COD4/zone/english> <outdir>   (outdir must be OUTSIDE the repo)
Writes <outdir>/shaders/{vs,ps}_<hash>.bin and <outdir>/index.json. Original content is never written to the repo.

Approach: techset headers are found by a signature scan (148 B header, validated by the pointer rules);
each is decoded with the block/pointer rules from research/fastfiles.md. 'Offset' pointers (reused
techniques/shaders/decls) need the absolute VIRTUAL offset of the pointee; since we skip the non-techset
assets in between, run starts are solved by voting against the references (see solve_runs).
Cross-check: an independent byte scan for ps_3_0/vs_3_0 version tokens must find the same blob set."""
import sys, os, re, json, struct, hashlib, glob, zlib, collections
import ffparse

SIG = re.compile(rb'(?:\xff\xff\xff\xff|...[\x40-\x4f])[\x00-\x0b][\x00\x01]..', re.S)
NAME = re.compile(rb'[,A-Za-z0-9_/]{2,80}')

class Spec:   # speculative parser over the inflated stream; virtual offsets are run-local
    def __init__(self, d, pos, voff):
        self.d, self.pos, self.voff = d, pos, voff
        self.objs = []     # (local_off, kind, obj)
        self.refs = []     # (X, kind, holder, key)
    def al(self, n, a):
        o = (self.voff + a - 1) & ~(a-1); self.voff = o + n; return o
    def take(self, n, a=1):
        o = self.al(n, a); b = self.d[self.pos:self.pos+n]; assert len(b) == n; self.pos += n; return o, b
    def cstr(self):
        o = self.voff
        e = self.d.index(b'\0', self.pos); s = self.d[self.pos:e].decode('latin1')
        self.al(e-self.pos+1, 1); self.pos = e+1; self.objs.append((o, 'S', [s])); return s
    def name(self, ptr, holder, key):
        if ptr == 0xFFFFFFFF: holder[key] = self.cstr()
        elif ptr == 0: holder[key] = None
        else: holder[key] = None; self.refs.append((ptr-1, 'S', holder, key))

def parse_techset_at(d, pos, voff):
    s = Spec(d, pos, voff)
    # header lives in TEMP: read without VIRTUAL allocation
    h = d[pos:pos+148]; s.pos = pos + 148
    ts = {'techniques': [None]*34, 'worldVertFormat': h[4]}
    s.name(struct.unpack_from('<I', h, 0)[0], ts, 'name')
    tp = struct.unpack_from('<34I', h, 12)
    for i, p in enumerate(tp):
        if p == 0: continue
        if p == 0xFFFFFFFF: ts['techniques'][i] = tech(s)
        else: ts['techniques'][i] = {'ref': p-1}; s.refs.append((p-1, 'T', ts['techniques'], i))
    return s, ts

def tech(s):
    npass = struct.unpack_from('<H', s.d, s.pos+6)[0]
    assert 1 <= npass <= 4
    o, h = s.take(8+20*npass, 4)
    T = {'flags': struct.unpack_from('<H', h, 4)[0], 'passes': []}
    s.objs.append((o, 'T', T))
    namep = struct.unpack_from('<I', h, 0)[0]
    for k in range(npass):
        dp, vp, pp, n1, n2, n3, csf, ap = struct.unpack_from('<IIIBBBBI', h, 8+20*k)
        P = {'customSamplerFlags': csf, 'counts': [n1, n2, n3], 'args': []}
        if dp == 0xFFFFFFFF:
            o2, dd = s.take(100, 4)
            P['decl'] = {'streamCount': dd[0], 'hasOptionalSource': dd[1], 'routing': [[dd[4+2*i], dd[5+2*i]] for i in range(min(dd[0], 16))]}
            s.objs.append((o2, 'D', P['decl']))
        else: P['decl'] = {'ref': dp-1}; s.refs.append((dp-1, 'D', P, 'decl'))
        for key, kind, ptr in (('vs', 'V', vp), ('ps', 'P', pp)):
            if ptr == 0xFFFFFFFF:
                o2, sh = s.take(16, 4)
                nmp, _rt, progp, psz, _lf = struct.unpack('<IIIHH', sh)
                assert progp == 0xFFFFFFFF
                S = {}
                s.name(nmp, S, 'name')
                _, code = s.take(psz*4, 4)
                S['code'] = code; S['hash'] = hashlib.sha1(code).hexdigest()[:12]
                P[key] = S; s.objs.append((o2, kind, S))
            else: P[key] = {'ref': ptr-1}; s.refs.append((ptr-1, kind, P, key))
        n = n1+n2+n3
        if n:
            assert ap == 0xFFFFFFFF
            o2, ab = s.take(8*n, 4)
            for j in range(n):
                typ, dest, u = struct.unpack_from('<HHI', ab, 8*j)
                assert typ < 8
                A = {'type': typ, 'dest': dest}
                if typ in (1, 7):
                    if u == 0xFFFFFFFF:
                        o3, lit = s.take(16, 4); A['lit'] = list(struct.unpack('<4f', lit)); s.objs.append((o3, 'F', A))
                    else: A['lit'] = {'ref': u-1}; s.refs.append((u-1, 'F', A, 'lit'))
                elif typ in (3, 5): A['index'], A['firstRow'], A['rowCount'] = u & 0xFFFF, (u >> 16) & 0xFF, u >> 24
                elif typ == 4: A['sampler'] = u
                else: A['hash'] = u
                P['args'].append(A)
        T['passes'].append(P)
    s.name(namep, T, 'name')
    return T

def valid_header(d, p):
    if p + 148 > len(d): return False
    if d[p+4] > 11 or d[p+5] > 1: return False
    w = struct.unpack_from('<34I', d, p+12)
    return all(x == 0 or x == 0xFFFFFFFF or (x-1) >> 28 == 4 for x in w)

def find_techsets(d, start):
    """anchor: header whose name is inline (-1) + a sane identifier; then chain forward:
    a techset whose name is an *offset* (reused string) is only accepted when it begins exactly at the end of a previous one."""
    out = {}
    for m in SIG.finditer(d, start):
        p = m.start()
        if d[p:p+4] != b'\xff\xff\xff\xff' or not valid_header(d, p): continue
        e = d.find(b'\0', p+148, p+148+100)
        if e < 0 or not NAME.fullmatch(d[p+148:e]): continue
        try: s, _ = parse_techset_at(d, p, 0)
        except Exception: continue
        out[p] = s.pos
    changed = True
    while changed:
        changed = False
        for p, e in list(out.items()):
            if e in out: continue
            if valid_header(d, e) and (d[e+3] & 0xF0) == 0x40:
                try: s, _ = parse_techset_at(d, e, 0)
                except Exception: continue
                out[e] = s.pos; changed = True
    keep = []; last_end = 0
    for p in sorted(out):          # drop candidates nested inside an earlier techset's byte span
        if p < last_end: continue
        keep.append(p); last_end = out[p]
    return keep

def parse_run(d, start, phase, starts):
    """techsets that are stream-contiguous share one VIRTUAL allocator; voff is run-local (+phase)."""
    sets, objs, refs = [], [], []
    pos, voff = start, phase
    while True:
        s, ts = parse_techset_at(d, pos, voff)
        sets.append(ts); objs += s.objs; refs += s.refs
        pos, voff = s.pos, s.voff
        if pos not in starts: return sets, objs, refs

def decode_zone(d, nasset_techsets):
    pos0 = 60
    positions = find_techsets(d, pos0)
    pset = set(positions)
    # run starts = positions that are not the end of another techset
    ends = set()
    for p in positions:
        s, _ = parse_techset_at(d, p, 0); ends.add(s.pos)
    starts = [p for p in positions if p not in ends]
    R = []
    for st in starts:
        R.append({'start': st, 'ph': [parse_run(d, st, ph, pset) for ph in range(4)], 'S': None, 'phase': 0})
    allrefs = [(X, kind) for r in R for (X, kind, _h, _k) in r['ph'][0][2]]
    unresolved = sorted(set(allrefs))
    known = {}
    for r in R:
        objs = r['ph'][0][1]
        if not objs: continue
        cand = unresolved[:80]
        votes = collections.Counter()
        for (X, kind) in cand:
            for ph in range(4):
                for (L, k, _o) in r['ph'][ph][1]:
                    if k == kind: votes[(ph, X-L)] += 1
        best = None
        for (ph, S), v in votes.most_common(8):
            if S % 4 != ph: continue          # S is the absolute offset of the run start
            objmap = {(L + (S - ph), k): o for (L, k, o) in r['ph'][ph][1]}
            size = max(L for (L, _, _) in r['ph'][ph][1]) + 1
            inwin = [u for u in unresolved if S <= u[0] < S + size - ph]
            good = [u for u in inwin if u in objmap]
            if good and len(good) == len(inwin) and (best is None or len(good) > best[0]):
                best = (len(good), ph, S, objmap)
        if best:
            _, ph, S, objmap = best
            r['S'], r['phase'] = S, ph; known.update(objmap)
            unresolved = [u for u in unresolved if u not in objmap]
    # patch references (chosen-phase tree)
    nref = nres = 0
    sets = []
    for r in R:
        ts_list, objs, refs = r['ph'][r['phase']]
        for (X, kind, holder, key) in refs:
            nref += 1
            o = known.get((X, kind))
            if o is not None: holder[key] = o[0] if kind == 'S' else o; nres += 1
        sets += ts_list
    return sets, {'techsets': len(positions), 'runs': len(starts), 'runs_located': sum(r['S'] is not None for r in R if r['ph'][0][1]), 'refs': nref, 'refs_resolved': nres}

# ---------- shader blob decoding (token walk + CTAB) ----------
def walk_shader(code):
    """returns (version_token, ninstr, ctab dict) or raises."""
    w = struct.unpack('<%dI' % (len(code)//4), code)
    ver = w[0]; i = 1; n = 0; ctab = None
    while i < len(w):
        t = w[i]; op = t & 0xFFFF
        if op == 0xFFFF: return ver, n, ctab, i+1
        if op == 0xFFFE:
            ln = (t >> 16) & 0x7FFF
            blob = code[(i+1)*4:(i+1+ln)*4]
            if blob[:4] == b'CTAB': ctab = parse_ctab(blob[4:])
            i += 1+ln; continue
        i += 1 + ((t >> 24) & 0xF); n += 1
    raise ValueError('no END token')

def parse_ctab(b):
    size, creator, ver, ncon, coff, flags, toff = struct.unpack_from('<IIIIIII', b, 0)
    def cs(o): return b[o:b.index(b'\0', o)].decode('latin1')
    cons = []
    for k in range(ncon):
        no, rs, ri, rc, _r, to, do = struct.unpack_from('<IHHHHII', b, coff + 20*k)
        cls, typ, rows, cols, el, sm, so = struct.unpack_from('<HHHHHHI', b, to)
        cons.append({'name': cs(no), 'regset': rs, 'reg': ri, 'count': rc, 'class': cls, 'type': typ, 'rows': rows, 'cols': cols})
    return cons

def main():
    zdir, out = sys.argv[1], sys.argv[2]
    assert '/cod4-decomp' not in os.path.abspath(out)
    os.makedirs(out + '/shaders', exist_ok=True)
    pats = ['mp_*.ff', 'common_mp.ff', 'code_post_gfx_mp.ff', 'ui_mp.ff', 'localized_*mp*.ff']
    files = sorted({f for p in pats for f in glob.glob(os.path.join(zdir, p))})
    shaders = {}     # (kind,hash) -> info
    techsets = {}    # key hash -> {name, zones, json}
    stats = []
    scan_mismatch = []
    for f in files:
        zn = os.path.basename(f)[:-3]
        z = ffparse.Zone(f); z.header()
        d = z.d
        sets, st = decode_zone(d, 0)
        st['zone'] = zn; st['type5'] = sum(1 for t in z.types if t == 5)
        zset = set()
        for ts in sets:
            tj = json.dumps(ts, default=lambda b: None, sort_keys=True) if False else None
            for t in ts['techniques']:
                if not t: continue
                for P in t.get('passes', []):
                    for key in ('vs', 'ps'):
                        S = P.get(key)
                        if S and 'code' in S:
                            k = (key, S['hash'])
                            zset.add(k)
                            if k not in shaders:
                                try: ver, n, ctab, ln = walk_shader(S['code'])
                                except Exception as e: ver, n, ctab, ln = 0, 0, None, 0
                                shaders[k] = {'kind': key, 'hash': S['hash'], 'names': set(), 'zones': set(), 'version': '%08x' % ver, 'ninstr': n, 'bytes': len(S['code']), 'ctab': ctab}
                                open('%s/shaders/%s_%s.bin' % (out, key, S['hash']), 'wb').write(S['code'])
                            shaders[k]['names'].add(S['name']); shaders[k]['zones'].add(zn)
        # independent byte scan for SM3 blobs
        scan = set()
        for m in re.finditer(rb'\x00\x03\xfe\xff|\x00\x03\xff\xff', d):
            p = m.start()
            if p % 4 == 0 and False: pass
            try:
                ver, n, ctab, ln = walk_shader(d[p:p+4*4096])
            except Exception: continue
            if ctab is None: continue
            code = d[p:p+ln*4]
            scan.add(('vs' if ver >> 16 == 0xFFFE else 'ps', hashlib.sha1(code).hexdigest()[:12]))
        sm3_parsed = {k for k in zset if shaders[k]['version'] in ('fffe0300', 'ffff0300')}
        st['sm3_parsed'] = len(sm3_parsed); st['sm3_scan'] = len(scan)
        st['scan_minus_parsed'] = len(scan - sm3_parsed); st['parsed_minus_scan'] = len(sm3_parsed - scan)
        for ts in sets:
            # serialise techset context (code blobs replaced by hashes)
            def ser(o):
                if isinstance(o, dict):
                    r = {}
                    for k, v in o.items():
                        if k == 'code': continue
                        r[k] = ser(v)
                    return r
                if isinstance(o, list): return [ser(x) for x in o]
                return o
            sj = ser(ts)
            key = hashlib.sha1(json.dumps(sj, sort_keys=True).encode()).hexdigest()[:12]
            e = techsets.setdefault(key, {'name': ts['name'], 'zones': [], 'data': sj})
            e['zones'].append(zn)
        stats.append(st); print(st, flush=True)
    for v in shaders.values():
        v['names'] = sorted(v['names']); v['zones'] = sorted(v['zones'])
    json.dump({'shaders': list(shaders.values()), 'techsets': techsets, 'zone_stats': stats}, open(out + '/index.json', 'w'))
    sm3 = [v for v in shaders.values() if v['version'] in ('fffe0300', 'ffff0300')]
    print('unique blobs', len(shaders), 'SM3', len(sm3), 'vs3', sum(v['kind'] == 'vs' for v in sm3), 'ps3', sum(v['kind'] == 'ps' for v in sm3))

if __name__ == '__main__': main()
