"""THROWAWAY world-geometry extractor for mp_crash (python3.9, stdlib only).
usage: python3 world_extract.py <COD4/zone/english/mp_crash.ff> <outdir>   (outdir must be OUTSIDE the repo)

Pass 1 (decode_prefix) decodes every asset before the GfxWorld (techsets, xmodels, comworld, light defs) only to
advance the exact absolute VIRTUAL allocation offset; it lands byte-exactly on the GfxWorld header (stream pos and
slot addresses are cross-checked). Pass 2 (decode) decodes the GfxWorld field by field. Only the VIRTUAL allocator
is tracked (INSERT alias slots live there); RUNTIME members consume no stream bytes; TEMP is scratch.
Facts verified against mp_crash (see NOTES.txt in the output dir):
  * GfxWorld header = 732 B. GfxWorld.name is an OFFSET ref (reused ComWorld string); baseName "mp_crash" is inline.
  * Member load order = declaration order, EXCEPT the stream carries vd (vertices) + vld right after materialMemory
    (i.e. before sun/outdoorImage/shadowGeom/lightRegion/dpvs). Whole GfxWorld ends exactly at the next asset header.
  * Pointer fields: -1 FOLLOW, -2 INSERT (4 B VIRTUAL alias slot, taken before the pointee loads; applies to ALL
    reusable pointers, e.g. GfxImage.texture.loadDef, tables, collision trees, brush planes), else offset
    ((block<<28)|off)+1. An asset pointer that was -1 inside a VIRTUAL array is referenced later by the address of
    that pointer FIELD (e.g. materialMemory[k].material); a -2 one by its slot; top-level assets by their XAsset
    array entry (+4).
  * GfxWorldVertex (44 B): f32 xyz[3]; f32 binormalSign; u8 color[4]; f32 texCoord[2]; f32 lmapCoord[2];
    u32 normal; u32 tangent (PackedUnitVec: bytes x,y,z biased 127, scale=(w+192)/32385 with w=top byte)
  * GfxSurface (48 B, NOT 56): i32 vertexLayerData; i32 firstVertex; u16 vertexCount; u16 triCount; i32 baseIndex;
    Material* material; i8 lightmapIndex, reflectionProbeIndex, primaryLightIndex, flags; f32 bounds[2][3]
    baseIndex is absolute into the index array; indices are RELATIVE to firstVertex (use base_vertex = firstVertex).
"""
import sys, os, re, json, struct, collections
import ffparse

U32 = struct.Struct('<I')
FOLLOW, INSERT = 0xFFFFFFFF, 0xFFFFFFFE
STRS = {}                      # absolute VIRTUAL offset -> string (reused names are offset pointers to these)

# GfxWorld header offsets (derived from IW3_Assets.h natural layout; confirmed by counts)
H = dict(name=0, baseName=4, planeCount=8, nodeCount=12, indexCount=16, indices=20, surfaceCount=24,
         skySurfCount=32, skyStartSurfs=36, skyImage=40, skySampler=44, vertexCount=48, vdVertices=52,
         vertexLayerDataSize=60, vldData=64, sunParse=72, sunLight=200, sunColorFromBsp=204,
         sunPrimaryLightIndex=216, primaryLightCount=220, cullGroupCount=224, reflectionProbeCount=228,
         reflectionProbes=232, reflectionProbeTextures=236, cellCount=240, planes=244, nodes=248,
         sceneEntCellBits=252, cellBitsCount=256, cells=260, lightmapCount=264, lightmaps=268,
         lightGrid=272, lightmapPrimaryTextures=328, lightmapSecondaryTextures=332, modelCount=336,
         models=340, mins=344, maxs=356, checksum=368, materialMemoryCount=372, materialMemory=376,
         sun=380, outdoorLookupMatrix=476, outdoorImage=540, dpvs=580)
HDR = 732


class World:
    def __init__(self, d, pos, v0=0, hdr=HDR):
        self.d, self.pos0 = d, pos
        self.pos = pos + hdr
        self.v = v0                     # VIRTUAL offset (relative to GfxWorld start, or absolute when v0 is the absolute start)
        self.slots = []                 # (rel, kind, obj) INSERTed asset slots
        self.refs = []                  # (abs_offset, kind, holder, key) unresolved offset refs
        self.strict = True              # GfxWorld decode: offset ptrs for arrays are unexpected (reusable pointers only in earlier assets)
        self.tabs = {}                  # rel -> table obj for reusable tables
        self.h = d[pos:pos + hdr]

    # --- primitives
    def u(self, o): return U32.unpack_from(self.h, o)[0]
    def i(self, o): return struct.unpack_from('<i', self.h, o)[0]
    def valloc(self, n, al):
        o = (self.v + al - 1) & ~(al - 1); self.v = o + n; return o
    def take(self, n):
        b = self.d[self.pos:self.pos + n]; assert len(b) == n; self.pos += n; return b
    def cstr(self):
        e = self.d.index(b'\0', self.pos); s = self.d[self.pos:e].decode('latin1')
        o = self.valloc(e - self.pos + 1, 1); self.pos = e + 1; STRS[o] = s; return s
    def string(self, p):
        if p == FOLLOW: return self.cstr()
        if p == 0: return None
        a = p - 1
        if (a >> 28) == 4 and (a & 0x0FFFFFFF) in STRS: return STRS[a & 0x0FFFFFFF]
        raise ValueError('unresolvable string ref %#x' % p)
    def array(self, p, size, count, al):
        """returns (rel, bytes) for inline arrays, None for null/ref. INSERT (-2) additionally reserves a 4 B
        VIRTUAL alias slot first; its rel offset is left in self.last_slot."""
        self.last_slot = None
        if p in (FOLLOW, INSERT) and count:
            if p == INSERT: self.last_slot = self.valloc(4, 4)
            rel = self.valloc(size * count, al); return rel, self.take(size * count)
        if p not in (0, FOLLOW, INSERT) and self.strict: raise ValueError('unexpected offset ptr for array %x' % p)
        return None
    def runtime(self, p, size, count):
        assert p in (0, FOLLOW), hex(p)

    # --- asset pointers
    def asset(self, p, kind, loader, field=None):
        """field: VIRTUAL rel offset of the pointer field itself (a -1 pointer is later referenced by that address)"""
        if p == 0: return None
        if p in (FOLLOW, INSERT):
            slot = self.valloc(4, 4) if p == INSERT else None
            st = self.pos
            obj = loader()
            if os.environ.get('WX_TRACE2'): print('   asset', kind, p == INSERT, st, '->', self.pos, obj.get('name') if isinstance(obj, dict) else obj)
            if slot is not None: self.slots.append((slot, kind, obj))
            if field is not None: self.slots.append((field, kind, obj))
            return obj
        r = {'ref': p - 1, 'kind': kind}
        self.refs.append((p - 1, kind, r))
        return r

    def image(self):
        h = self.take(36)
        mapType, tex = struct.unpack_from('<II', h, 0)
        im = {'mapType': mapType, 'semantic': h[11], 'category': h[30],
              'w': struct.unpack_from('<H', h, 24)[0], 'h': struct.unpack_from('<H', h, 26)[0],
              'depth': struct.unpack_from('<H', h, 28)[0]}
        im['name'] = self.string(U32.unpack_from(h, 32)[0])
        if tex in (FOLLOW, INSERT):             # GfxImageLoadDef in TEMP (reusable: INSERT reserves a VIRTUAL alias slot)
            if tex == INSERT: self.valloc(4, 4)
            ld = self.take(16)
            lc, fl, d0, d1, d2, fmt, rsz = struct.unpack('<BBHHHiI', ld)
            self.take(rsz)
            im['loadDef'] = {'levels': lc, 'flags': fl, 'dims': [d0, d1, d2], 'format': fmt, 'resourceSize': rsz}
        elif tex != 0: im['loadDef'] = {'ref': tex - 1}
        return im

    # --- inline techset (consumed only to keep the stream/VIRTUAL accounting exact; mirrors ffparse.load_techset)
    def ptr_slot(self, p):
        if p == INSERT: return self.valloc(4, 4)

    def techset(self):
        h = self.take(148)
        namep = U32.unpack_from(h, 0)[0]
        ts = {'name': self.string(namep), 'techniques': 0}
        for p in struct.unpack_from('<34I', h, 12):
            if p in (FOLLOW, INSERT):
                self.ptr_slot(p); self.technique(); ts['techniques'] += 1
        return ts

    def technique(self):
        npass = struct.unpack_from('<H', self.d, self.pos + 6)[0]
        self.valloc(8 + 20 * npass, 4); h = self.take(8 + 20 * npass)
        namep = U32.unpack_from(h, 0)[0]
        for k in range(npass):
            decl_p, vs_p, ps_p, n1, n2, n3, csf, args_p = struct.unpack_from('<IIIBBBBI', h, 8 + 20 * k)
            if decl_p in (FOLLOW, INSERT):
                self.ptr_slot(decl_p); self.valloc(100, 4); self.take(100)
            for ptr in (vs_p, ps_p):
                if ptr in (FOLLOW, INSERT):
                    self.ptr_slot(ptr)
                    self.valloc(16, 4); sh = self.take(16)
                    nm_p, _rt, prog_p, psz, _lf = struct.unpack('<IIIHH', sh)
                    self.string(nm_p)
                    assert prog_p == FOLLOW
                    self.valloc(psz * 4, 4); self.take(psz * 4)
            n = n1 + n2 + n3
            if n:
                assert args_p == FOLLOW
                self.valloc(8 * n, 4); ab = self.take(8 * n)
                for j in range(n):
                    typ, dest, uu = struct.unpack_from('<HHI', ab, 8 * j)
                    if typ in (1, 7) and uu in (FOLLOW, INSERT):
                        self.ptr_slot(uu); self.valloc(16, 4); self.take(16)
        if namep in (FOLLOW,): self.string(namep)

    # --- earlier top-level assets (needed only to advance the absolute VIRTUAL offset exactly)
    def physpreset(self):
        h = self.take(44)
        pp = {'name': self.string(U32.unpack_from(h, 0)[0])}
        self.string(U32.unpack_from(h, 28)[0])
        return pp

    def xmodel(self):
        h = self.take(220)
        namep = U32.unpack_from(h, 0)[0]
        nb, nrb, ns = h[4], h[5], h[6]
        bp, pl, qp, tp, pc, bm, sp, mh = struct.unpack_from('<8I', h, 8)
        cs, ncs, bi = U32.unpack_from(h, 152)[0], struct.unpack_from('<i', h, 156)[0], U32.unpack_from(h, 164)[0]
        phyp, phyg = U32.unpack_from(h, 212)[0], U32.unpack_from(h, 216)[0]
        xm = {'name': self.string(namep), 'numBones': nb, 'numsurfs': ns}
        self.array(bp, 2, nb, 2)
        self.array(pl, 1, nb - nrb, 1)
        self.array(qp, 8, nb - nrb, 2)
        self.array(tp, 4, (nb - nrb) * 4, 4)
        self.array(pc, 1, nb, 1)
        self.array(bm, 32, nb, 4)
        sr = self.array(sp, 56, ns, 4)
        if sr:
            for k in range(ns):
                tileMode, deformed, vcount, tcount, zh, btri, bvert = struct.unpack_from('<BBHHBxHH', sr[1], 56 * k)
                tip = U32.unpack_from(sr[1], 56 * k + 12)[0]
                vc4 = struct.unpack_from('<4h', sr[1], 56 * k + 16)
                vbp = U32.unpack_from(sr[1], 56 * k + 24)[0]
                v0p = U32.unpack_from(sr[1], 56 * k + 28)[0]
                vlc = U32.unpack_from(sr[1], 56 * k + 32)[0]
                vlp = U32.unpack_from(sr[1], 56 * k + 36)[0]
                self.array(vbp, 2, vc4[0] + 3 * vc4[1] + 5 * vc4[2] + 7 * vc4[3], 2)
                # verts0 -> VERTEX block: stream bytes, VIRTUAL only gets an INSERT alias slot
                self.vblock(v0p, 32, vcount)
                vl = self.array(vlp, 12, vlc, 4)
                if vl:
                    for q in range(vlc):
                        ctp = U32.unpack_from(vl[1], 12 * q + 8)[0]
                        ct = self.array(ctp, 40, 1, 4)
                        if ct:
                            nc, np_, lc, lp = struct.unpack_from('<IIII', ct[1], 24)
                            self.array(np_, 16, nc, 16)
                            self.array(lp, 2, lc, 2)
                self.vblock(tip, 6, tcount)
        mr = self.array(mh, 4, ns, 4)
        xm['materials'] = []
        if mr:
            for k in range(ns):
                mp = U32.unpack_from(mr[1], 4 * k)[0]
                xm['materials'].append(self.asset(mp, 'M', self.material, mr[0] + 4 * k))
        csr = self.array(cs, 44, ncs, 4)
        if csr:
            for k in range(ncs):
                ctp = U32.unpack_from(csr[1], 44 * k)[0]; nct = struct.unpack_from('<i', csr[1], 44 * k + 4)[0]
                self.array(ctp, 48, nct, 4)
        self.array(bi, 40, nb, 4)
        xm['physPreset'] = self.asset(phyp, 'PP', self.physpreset)
        pg = self.array(phyg, 44, 1, 4)
        if pg:
            gc = U32.unpack_from(pg[1], 0)[0]; gp = U32.unpack_from(pg[1], 4)[0]
            gr = self.array(gp, 68, gc, 4)
            if gr:
                for k in range(gc):
                    bp_ = U32.unpack_from(gr[1], 68 * k)[0]
                    self.brush(bp_)
        return xm

    def vblock(self, p, size, count):
        """pointer into the VERTEX/INDEX block: INSERT reserves a VIRTUAL alias slot; data comes from the stream"""
        if p in (FOLLOW, INSERT) and count:
            if p == INSERT: self.valloc(4, 4)
            self.take(size * count)
        elif p not in (0, FOLLOW, INSERT): pass

    def brush(self, p):
        if p not in (FOLLOW,): 
            assert p == 0, hex(p); return
        r = self.array(p, 80, 1, 4)
        b = r[1]
        numsides = U32.unpack_from(b, 28)[0]; sidesp = U32.unpack_from(b, 32)[0]
        baseadj = U32.unpack_from(b, 48)[0]; totedge = struct.unpack_from('<i', b, 72)[0]; planesp = U32.unpack_from(b, 76)[0]
        sd = self.array(sidesp, 12, numsides, 4)
        if sd:
            for k in range(numsides):
                pp = U32.unpack_from(sd[1], 12 * k)[0]
                self.array(pp, 20, 1, 4)
        self.array(baseadj, 1, totedge, 1)
        self.array(planesp, 20, numsides, 4)

    def comworld(self):
        h = self.take(16)
        cw = {'name': self.string(U32.unpack_from(h, 0)[0]), 'lights': []}
        n = U32.unpack_from(h, 8)[0]
        r = self.array(U32.unpack_from(h, 12)[0], 68, n, 4)
        if r:
            for k in range(n):
                b = r[1][68 * k:68 * k + 68]
                f = struct.unpack_from('<3f3f3f6f', b, 4)
                cw['lights'].append({'type': b[0], 'canUseShadowMap': b[1], 'exponent': b[2], 'color': list(f[0:3]), 'dir': list(f[3:6]),
                                     'origin': list(f[6:9]), 'radius': f[9], 'cosHalfFovOuter': f[10], 'cosHalfFovInner': f[11],
                                     'cosHalfFovExpanded': f[12], 'rotationLimit': f[13], 'translationLimit': f[14]})
            for k in range(n): cw['lights'][k]['defName'] = self.string(U32.unpack_from(r[1], 68 * k + 64)[0])
        return cw

    def lightdef(self):
        h = self.take(16)
        np_, imgp = struct.unpack_from('<II', h, 0)
        ld = {'name': self.string(np_)}
        ld['image'] = self.asset(imgp, 'I', self.image)
        return ld

    def material(self):
        h = self.take(80)
        namep = U32.unpack_from(h, 0)[0]
        m = {'gameFlags': h[4], 'sortKey': h[5], 'atlasRows': h[6], 'atlasCols': h[7],
             'drawSurf': struct.unpack_from('<Q', h, 8)[0], 'surfaceTypeBits': struct.unpack_from('<I', h, 16)[0],
             'stateBitsEntry': list(h[24:58]), 'textureCount': h[58], 'constantCount': h[59],
             'stateBitsCount': h[60], 'stateFlags': h[61], 'cameraRegion': h[62]}
        tsp, ttp, ctp, sbp = struct.unpack_from('<IIII', h, 64)
        m['name'] = self.string(namep)
        m['techset'] = self.asset(tsp, 'T', self.techset)
        # textureTable
        m['textures'] = None
        if ttp in (FOLLOW, INSERT):
            rel, b = self.array(ttp, 12, m['textureCount'], 4); slot = self.last_slot
            tex = []
            for k in range(m['textureCount']):
                nh, ns, ne, ss, sem, u = struct.unpack_from('<IBBBBI', b, 12 * k)
                t = {'nameHash': nh, 'samplerState': ss, 'semantic': sem}
                assert sem != 0xB, 'water texture not supported'
                tex.append((t, u))
            for k, (t, u) in enumerate(tex):
                t['image'] = self.asset(u, 'I', self.image, rel + 12 * k + 8)
            m['textures'] = [t for t, _ in tex]
            if slot is not None: self.tabs[slot] = m['textures']
        elif ttp:
            m['textures'] = {'ref': ttp - 1}; self.refs.append((ttp - 1, 'TT', m['textures']))
        m['constants'] = None
        if ctp in (FOLLOW, INSERT):
            rel, b = self.array(ctp, 32, m['constantCount'], 16); slot = self.last_slot
            m['constants'] = []
            for k in range(m['constantCount']):
                nh = struct.unpack_from('<I', b, 32 * k)[0]
                nm = b[32 * k + 4:32 * k + 16].split(b'\0')[0].decode('latin1')
                m['constants'].append({'nameHash': nh, 'name': nm, 'value': list(struct.unpack_from('<4f', b, 32 * k + 16))})
            if slot is not None: self.tabs[slot] = m['constants']
        elif ctp:
            m['constants'] = {'ref': ctp - 1}; self.refs.append((ctp - 1, 'CT', m['constants']))
        m['stateBits'] = None
        if sbp in (FOLLOW, INSERT):
            rel, b = self.array(sbp, 8, m['stateBitsCount'], 4); slot = self.last_slot
            m['stateBits'] = [list(struct.unpack_from('<II', b, 8 * k)) for k in range(m['stateBitsCount'])]
            if slot is not None: self.tabs[slot] = m['stateBits']
        elif sbp:
            m['stateBits'] = {'ref': sbp - 1}; self.refs.append((sbp - 1, 'SB', m['stateBits']))
        return m

    def loaddef_array(self, p, count):
        """RUNTIME array of GfxTexture: zeroed runtime space, no stream bytes, loadDef pointers stay null"""
        self.runtime(p, 4, count)


def decode(d, p0, v0=0):
    w = World(d, p0, v0)
    u, i = w.u, w.i
    out = {}
    out['name'] = w.string(u(H['name'])); out['baseName'] = w.string(u(H['baseName']))
    planeCount, nodeCount = i(H['planeCount']), i(H['nodeCount'])
    indexCount, surfaceCount = i(H['indexCount']), i(H['surfaceCount'])
    vertexCount, vldSize = u(H['vertexCount']), u(H['vertexLayerDataSize'])
    out['counts'] = dict(planeCount=planeCount, nodeCount=nodeCount, indexCount=indexCount,
                         surfaceCount=surfaceCount, vertexCount=vertexCount, vertexLayerDataSize=vldSize,
                         primaryLightCount=u(H['primaryLightCount']), cellCount=i(H['cellCount']),
                         lightmapCount=i(H['lightmapCount']), reflectionProbeCount=u(H['reflectionProbeCount']),
                         modelCount=i(H['modelCount']), materialMemoryCount=i(H['materialMemoryCount']),
                         cullGroupCount=i(H['cullGroupCount']))
    T = lambda tag: os.environ.get('WX_TRACE') and print('  [%s] pos=%d v=%d' % (tag, w.pos, w.v))
    # indices
    r = w.array(u(H['indices']), 2, indexCount, 2); out['indices_rel'] = r[0]; out['indices'] = r[1]
    T('indices')
    # skyStartSurfs
    w.array(u(H['skyStartSurfs']) if i(H['skySurfCount']) else 0, 4, i(H['skySurfCount']), 4)
    out['skyImage'] = w.asset(u(H['skyImage']), 'I', w.image)
    T('sky')
    # sunLight (reusable single GfxLight, 64 B)
    slp = u(H['sunLight'])
    if slp in (FOLLOW, INSERT):
        r = w.array(slp, 64, 1, 4)
        dp = struct.unpack_from('<I', r[1], 60)[0]
        out['sunLight'] = {'type': r[1][0], 'color': struct.unpack_from('<3f', r[1], 4), 'dir': struct.unpack_from('<3f', r[1], 16),
                           'origin': struct.unpack_from('<3f', r[1], 28), 'radius': struct.unpack_from('<f', r[1], 40)[0]}
        out['sunLight']['def'] = w.asset(dp, 'LD', w.lightdef)
    T('sun')
    # reflection probes
    rpc = u(H['reflectionProbeCount'])
    rp = w.array(u(H['reflectionProbes']), 16, rpc, 4)
    out['reflectionProbes'] = []
    if rp:
        for k in range(rpc):
            o = struct.unpack_from('<3f', rp[1], 16 * k); ip = struct.unpack_from('<I', rp[1], 16 * k + 12)[0]
            out['reflectionProbes'].append({'origin': o, 'image': w.asset(ip, 'I', w.image, rp[0] + 16 * k + 12)})
    T('rprobes')
    w.loaddef_array(u(H['reflectionProbeTextures']), rpc)
    T('rptex')
    # dpvsPlanes
    cellCount = i(H['cellCount'])
    w.array(u(H['planes']), 20, planeCount, 4)
    w.array(u(H['nodes']), 2, nodeCount, 2)
    w.runtime(u(H['sceneEntCellBits']), 4, cellCount * 0x100)
    T('planes')
    # cells
    cells = w.array(u(H['cells']), 56, cellCount, 4)
    ncull = nrefp = ntrees = nport = 0
    if cells:
        for c in range(cellCount):
            (mn0, mn1, mn2, mx0, mx1, mx2, atc, atp, pc, pp, cgc, cgp, rpcnt, rppp) = struct.unpack_from('<6fIIIIIIB3xI', cells[1], 56 * c)
            tr = w.array(atp, 44, atc, 4)
            if tr:
                ntrees += atc
                for t in range(atc):
                    sic = struct.unpack_from('<H', tr[1], 44 * t + 34)[0]
                    sip = struct.unpack_from('<I', tr[1], 44 * t + 36)[0]
                    if sip and sic:
                        if sip in (FOLLOW, INSERT): w.array(sip, 2, sic, 2)
                        # offset -> reuse
            pr = w.array(pp, 68, pc, 4)
            if pr:
                nport += pc
                for q in range(pc):
                    vc = pr[1][68 * q + 40]; vp = struct.unpack_from('<I', pr[1], 68 * q + 36)[0]
                    cp = struct.unpack_from('<I', pr[1], 68 * q + 32)[0]
                    assert cp != FOLLOW
                    if vp: w.array(vp, 12, vc, 4)
            w.array(cgp, 4, cgc, 4)
            w.array(rppp, 1, rpcnt, 1)
    T('cells')
    # lightmaps
    lmc = i(H['lightmapCount'])
    lm = w.array(u(H['lightmaps']), 8, lmc, 4)
    out['lightmaps'] = []
    if lm:
        for k in range(lmc):
            a, b = struct.unpack_from('<II', lm[1], 8 * k)
            pi = w.asset(a, 'I', w.image, lm[0] + 8 * k); si = w.asset(b, 'I', w.image, lm[0] + 8 * k + 4)
            out['lightmaps'].append({'primary': pi, 'secondary': si})
    # lightGrid (inline header struct at 272)
    g = H['lightGrid']
    mins = struct.unpack_from('<3H', w.h, g + 8); maxs = struct.unpack_from('<3H', w.h, g + 14)
    rowAxis = u(g + 20)
    rdsp, rrsz, rrp, ec, ep, cc, cp = (u(g + 28), u(g + 32), u(g + 36), u(g + 40), u(g + 44), u(g + 48), u(g + 52))
    w.array(rdsp, 2, maxs[rowAxis] - mins[rowAxis] + 1, 2)
    w.array(rrp, 1, rrsz, 1)
    w.array(ep, 4, ec, 4)
    w.array(cp, 168, cc, 4)
    w.loaddef_array(u(H['lightmapPrimaryTextures']), lmc)
    w.loaddef_array(u(H['lightmapSecondaryTextures']), lmc)
    T('lightgrid')
    # models
    w.array(u(H['models']), 56, i(H['modelCount']), 4)
    T('models')
    # materialMemory (loaded right after models, i.e. in declaration order; only vd/vld are deferred)
    mm = w.array(u(H['materialMemory']), 8, i(H['materialMemoryCount']), 4)
    out['materialMemory'] = []
    if mm:
        for k in range(i(H['materialMemoryCount'])):
            mp, mem = struct.unpack_from('<Ii', mm[1], 8 * k)
            out['materialMemory'].append(w.asset(mp, 'M', w.material, mm[0] + 8 * k))
    # vd, vld (stream order: right after materialMemory)
    vr = w.array(u(H['vdVertices']), 44, vertexCount, 4)
    out['verts'] = vr[1]
    vl = w.array(u(H['vldData']), 1, vldSize, 1)
    out['vld'] = vl[1] if vl else b''
    # sun flare materials
    s = H['sun']
    out['sun_sprite'] = w.asset(u(s + 4), 'M', w.material)
    out['sun_flare'] = w.asset(u(s + 8), 'M', w.material)
    out['outdoorImage'] = w.asset(u(H['outdoorImage']), 'I', w.image)
    # RUNTIME members: no stream bytes (cellCasterBits, sceneDynModel, sceneDynBrush, shadow vis...)
    T('outdoor')
    pc = u(H['primaryLightCount'])
    sg = w.array(u(572), 12, pc, 4)
    if sg:
        for k in range(pc):
            sc, mc, sp, mp = struct.unpack_from('<HHII', sg[1], 12 * k)
            w.array(sp, 2, sc, 2); w.array(mp, 2, mc, 2)
    lr = w.array(u(576), 8, pc, 4)
    if lr:
        for k in range(pc):
            hc, hp = struct.unpack_from('<II', lr[1], 8 * k)
            hl = w.array(hp, 80, hc, 4)
            if hl:
                for q in range(hc):
                    ac, ap = struct.unpack_from('<II', hl[1], 80 * q + 72)
                    w.array(ap, 20, ac, 4)
    # dpvs static
    D = H['dpvs']
    smodelCount, ssc, sscnd = u(D), u(D + 4), u(D + 8)
    w.array(u(D + 72), 2, ssc + sscnd, 2)                  # sortedSurfIndex
    w.array(u(D + 76), 28, smodelCount, 4)                  # smodelInsts
    surfs = w.array(u(D + 80), 48, surfaceCount, 4)
    assert surfs
    out['surfaces_raw'] = surfs[1]
    out['surfaceMaterials'] = []
    for k in range(surfaceCount):
        mp = struct.unpack_from('<I', surfs[1], 48 * k + 16)[0]
        out['surfaceMaterials'].append(w.asset(mp, 'M', w.material, surfs[0] + 48 * k + 16))
    w.array(u(D + 84), 32, i(H['cullGroupCount']), 4)
    sdi = w.array(u(D + 88), 76, smodelCount, 4)
    out['smodelModelPtrs'] = collections.Counter()
    out['inlineXModels'] = []
    if sdi:
        for k in range(smodelCount):
            mp = struct.unpack_from('<I', sdi[1], 76 * k + 56)[0]
            out['smodelModelPtrs']['inline' if mp in (FOLLOW, INSERT) else 'ref' if mp else 'null'] += 1
            xm = w.asset(mp, 'X', w.xmodel, sdi[0] + 76 * k + 56)
            if mp in (FOLLOW, INSERT): out['inlineXModels'].append(xm['name'])
    out['end_pos'] = w.pos
    out['w'] = w
    out['hdr'] = w.h
    out['stats'] = dict(cells=cellCount, aabbTrees=ntrees, portals=nport)
    return out


# ------------------------------------------------------------------ prefix pass / VIRTUAL base
def asset_array_offset(z):
    """VIRTUAL offset of the XAsset array (strings pointers + strings, then aligned 4)."""
    off = 4 * z.nstr
    for p, s in zip(z.strptrs, z.strings):
        if p == 0xFFFFFFFF: off += len(s) + 1
    return (off + 3) & ~3


def decode_prefix(z):
    """Decode every asset before the GfxWorld (techsets, xmodels, comworld, lightdefs) only to advance the exact
    absolute VIRTUAL offset, so offset pointers into earlier nested data (images/materials of xmodels) resolve.
    Returns (World, {asset-array slot address -> techset name})"""
    d = z.d
    AA = asset_array_offset(z)
    off = 4 * z.nstr
    for p, st in zip(z.strptrs, z.strings):
        if p == 0xFFFFFFFF: STRS[off] = st; off += len(st) + 1
    w = World(d, z.pos, AA + 8 * z.nasset, 0)
    w.strict = False
    loaders = {5: w.techset, 3: w.xmodel, 12: w.comworld, 17: w.lightdef}
    slot_name = {}
    gi = z.types.index(16)
    for k in range(gi):
        o = loaders[z.types[k]]()
        if z.types[k] == 5: slot_name[AA + 8 * k + 4] = o['name']
        if z.types[k] == 12: w.comworld_obj = o
    return w, slot_name


def main():
    ff, outdir = sys.argv[1], sys.argv[2]
    os.makedirs(outdir, exist_ok=True)
    z = ffparse.Zone(ff); z.header(); d = z.d
    w0, slot_name = decode_prefix(z)
    p0, B = w0.pos, w0.v
    assert struct.unpack_from('<i', d, p0 + 24)[0] == 5558 and d[p0 + 4:p0 + 8] == b'\xff\xff\xff\xff', 'prefix decode did not land on the GfxWorld header'
    print('prefix decoded: %d assets, GfxWorld header at stream %d, absolute VIRTUAL base %d' % (z.types.index(16), p0, B))
    out = decode(d, p0, B)
    w = out['w']
    nxt = U32.unpack_from(d, out['end_pos'])[0]
    print('counts', out['counts'], 'end_pos', out['end_pos'], 'next word %#x (GameWorldMp name ref expected)' % nxt, 'stats', out['stats'])
    print('inline xmodels in world:', out['inlineXModels'], 'smodel ptr kinds', dict(out['smodelModelPtrs']))
    print('techset slots', len(slot_name))
    rel_by_abs = {}
    for ww in (w0, w):
        for a_, k, o in ww.slots: rel_by_abs[a_] = (k, o)
    tabs = {}
    tabs.update(w0.tabs); tabs.update(w.tabs)

    def resolve(ref, kind):
        a = ref & 0x0FFFFFFF
        if (ref >> 28) != 4: return None
        if kind == 'T' and a in slot_name: return slot_name[a]
        if a in rel_by_abs and rel_by_abs[a][0] == kind:
            o = rel_by_abs[a][1]
            return o['name'] if kind == 'T' else o
        return None

    mats = {}                       # material name -> material obj
    surf_mat = []
    for m in out['surfaceMaterials']:
        if m is None: surf_mat.append(None); continue
        if 'ref' in m and 'name' not in m:
            r = resolve(m['ref'], 'M')
            if r is None: surf_mat.append(('unresolved', m['ref'])); continue
            m = r
        surf_mat.append(m['name']); mats.setdefault(m['name'], m)
    for m in out['materialMemory'] + [out['sun_sprite'], out['sun_flare']]:
        if m and 'name' in m: mats.setdefault(m['name'], m)
    print('surface material unresolved:', sum(1 for n in surf_mat if not isinstance(n, str)), 'distinct materials', len(mats))

    # ---- table / image / techset resolution of materials
    tab_by_abs = tabs
    stats = collections.Counter()

    def rsolve(obj, kind):
        """obj is a decoded object or {'ref':..}; returns decoded object or None"""
        if obj is None: return None
        if isinstance(obj, dict) and 'ref' in obj and 'name' not in obj:
            return resolve(obj['ref'], kind)
        return obj

    def tab(o, kind):
        if isinstance(o, dict) and 'ref' in o:
            a = o['ref'] & 0x0FFFFFFF
            return tab_by_abs.get(a) if (o['ref'] >> 28) == 4 else None
        return o

    def mat_json(name, m):
        for fld, cnt in (('textures', 'textureCount'), ('constants', 'constantCount'), ('stateBits', 'stateBitsCount')):
            if m[fld] is None and m[cnt] == 0: m[fld] = []
        ts = m['techset']
        tsn = None
        if isinstance(ts, dict) and 'ref' in ts: tsn = resolve(ts['ref'], 'T')
        elif isinstance(ts, dict): tsn = ts.get('name')
        stats['techset_resolved' if tsn else 'techset_unresolved'] += 1
        J = {'name': name, 'techset': tsn, 'sortKey': m['sortKey'], 'gameFlags': m['gameFlags'],
             'stateFlags': m['stateFlags'], 'cameraRegion': m['cameraRegion'], 'surfaceTypeBits': m['surfaceTypeBits'],
             'atlasRows': m['atlasRows'], 'atlasCols': m['atlasCols'], 'textureCount': m['textureCount'],
             'constantCount': m['constantCount'], 'stateBitsCount': m['stateBitsCount'],
             'stateBitsEntry': m['stateBitsEntry']}
        tx = tab(m['textures'], 'TT'); J['textures'] = None
        if tx is not None:
            J['textures'] = []
            for t in tx:
                im = rsolve(t['image'], 'I')
                stats['image_resolved' if im else 'image_unresolved'] += 1
                ss = t['samplerState']
                J['textures'].append({'semantic': t['semantic'], 'semanticName': SEM.get(t['semantic'], str(t['semantic'])),
                                      'nameHash': t['nameHash'], 'samplerState': ss,
                                      'sampler': {'filter': ss & 7, 'mipMap': (ss >> 3) & 3, 'clampU': (ss >> 5) & 1, 'clampV': (ss >> 6) & 1, 'clampW': (ss >> 7) & 1},
                                      'image': im['name'] if im else None, 'imageFile': im['name'].lstrip(',') if im else None, 'mapType': im['mapType'] if im else None,
                                      'imageSemantic': im['semantic'] if im else None, 'imageSize': [im['w'], im['h']] if im else None,
                                      'imageFormatCode': (im.get('loadDef') or {}).get('format') if im else None})
        else: stats['textures_unresolved'] += 1
        cs = tab(m['constants'], 'CT')
        J['constants'] = cs
        if cs is None: stats['constants_unresolved'] += 1
        sb = tab(m['stateBits'], 'SB')
        J['stateBits'] = None
        if sb is not None: J['stateBits'] = [decode_state(x) for x in sb]
        else: stats['stateBits_unresolved'] += 1
        return J

    # ---- materials.json (distinct materials referenced by surfaces, then the rest of materialMemory)
    names_in_surfaces = collections.OrderedDict()
    for n in surf_mat:
        if isinstance(n, str): names_in_surfaces.setdefault(n, 0); names_in_surfaces[n] += 1
    matj = []
    for n in names_in_surfaces: matj.append(mat_json(n, mats[n]))
    for n, m in mats.items():
        if n not in names_in_surfaces:
            j = mat_json(n, m); j['notInSurfaces'] = True; matj.append(j)
    # ---- surfaces.json
    sraw = out['surfaces_raw']; surfs = []
    idx = out['indices']; nidx = len(idx) // 2
    vmin = vmax = None
    conv = collections.Counter()
    for k in range(len(surf_mat)):
        vl, fv, vc, tc, bi, mp, lm, rpi, pli, fl = struct.unpack_from('<iiHHiIbbbb', sraw, 48 * k)
        bounds = struct.unpack_from('<6f', sraw, 48 * k + 24)
        seg = struct.unpack_from('<%dH' % (tc * 3), idx, bi * 2)
        lo, hi = min(seg), max(seg)
        if fv == 0: conv['firstVertex==0 (ambiguous)' if hi < vc else 'neither'] += 1
        elif hi < vc: conv['relative (indices in 0..vertexCount-1, firstVertex>0)'] += 1
        elif lo >= fv and hi < fv + vc: conv['absolute (indices in firstVertex..)'] += 1
        else: conv['neither'] += 1
        mn = surf_mat[k]
        surfs.append({'material': mn if isinstance(mn, str) else None, 'firstVertex': fv, 'vertexCount': vc, 'firstIndex': bi,
                      'triCount': tc, 'lightmapIndex': lm, 'reflectionProbeIndex': rpi, 'primaryLightIndex': pli, 'flags': fl & 0xFF,
                      'vertexLayerData': vl, 'bounds': [list(bounds[:3]), list(bounds[3:])],
                      'unresolvedMaterialRef': None if isinstance(mn, str) else (list(mn) if mn else None)})
    print('index convention counts', dict(conv), 'sum tri*3', sum(x['triCount'] * 3 for x in surfs), 'indexCount', nidx)
    spans = sorted((x['firstIndex'], x['triCount'] * 3) for x in surfs)
    print('index spans contiguous & disjoint:', all(spans[i][0] + spans[i][1] <= spans[i + 1][0] for i in range(len(spans) - 1)), 'last end', spans[-1][0] + spans[-1][1])
    # ---- sanity: vertices
    vb = out['verts']; nv = len(vb) // 44
    hdr = out['hdr']
    wmin = struct.unpack_from('<3f', hdr, H['mins']); wmax = struct.unpack_from('<3f', hdr, H['maxs'])
    outside = 0; lens = []; tlens = []; dots = []
    for k in range(nv):
        x, y, z_ = struct.unpack_from('<3f', vb, 44 * k)
        if not all(wmin[a] - 1 <= v <= wmax[a] + 1 for a, v in enumerate((x, y, z_))): outside += 1
        if k % 50 == 0:
            nrm = unpack_vec(struct.unpack_from('<I', vb, 44 * k + 36)[0]); lens.append(sum(c * c for c in nrm) ** 0.5)
            tg = unpack_vec(struct.unpack_from('<I', vb, 44 * k + 40)[0]); tlens.append(sum(c * c for c in tg) ** 0.5)
            dots.append(sum(a * b for a, b in zip(nrm, tg)))
    print('verts', nv, 'outside bounds', outside, 'normal len min/max', min(lens), max(lens), 'tangent len', min(tlens), max(tlens), 'max |n.t|', max(abs(x) for x in dots))
    json.dump(surfs, open(outdir + '/surfaces.json', 'w'))
    sunp = hdr[H['sunParse']:H['sunParse'] + 128]
    sa = struct.unpack_from('<f3ff3f3f', sunp, 64)
    sunl = out.get('sunLight')
    meta = {'world_bounds': {'mins': list(wmin), 'maxs': list(wmax)},
            'sunParse': {'name': sunp[:64].split(b'\0')[0].decode('latin1'), 'ambientScale': sa[0], 'ambientColor': list(sa[1:4]),
                         'diffuseFraction': sa[4], 'sunLight': sa[5], 'sunColor': list(sa[6:9]), 'diffuseColor': list(struct.unpack_from('<3f', sunp, 100)),
                         'diffuseColorHasBeenSet': sunp[112], 'angles': list(struct.unpack_from('<3f', sunp, 116))},
            'sunColorFromBsp': list(struct.unpack_from('<3f', hdr, H['sunColorFromBsp'])),
            'sunPrimaryLightIndex': struct.unpack_from('<I', hdr, H['sunPrimaryLightIndex'])[0],
            'primaryLights': w0.comworld_obj['lights'],
            'primaryLightCount': struct.unpack_from('<I', hdr, H['primaryLightCount'])[0],
            'sunLight': {k: (list(v) if isinstance(v, tuple) else v) for k, v in sunl.items() if k != 'def'} if sunl else None,
            'sunFlare': {'hasValidData': hdr[H['sun']], 'sunFxPosition': list(struct.unpack_from('<3f', hdr, H['sun'] + 84))},
            'outdoorLookupMatrix': list(struct.unpack_from('<16f', hdr, H['outdoorLookupMatrix'])),
            'counts': out['counts'],
            'reflectionProbes': [{'origin': list(p['origin']), 'image': (rsolve(p['image'], 'I') or {}).get('name')} for p in out['reflectionProbes']],
            'lightmaps': [{'primary': (rsolve(l['primary'], 'I') or {}).get('name'), 'secondary': (rsolve(l['secondary'], 'I') or {}).get('name')} for l in out['lightmaps']],
            'indexConvention': dict(conv)}
    json.dump({'world': meta, 'materials': matj}, open(outdir + '/materials.json', 'w'), indent=1)
    open(outdir + '/verts.bin', 'wb').write(vb)
    open(outdir + '/indices.bin', 'wb').write(idx)
    print('stats', dict(stats), 'surfaces', len(surfs), 'materials', len(matj), 'verts bytes', len(vb), 'idx', nidx)
    return out, B


SEM = {0: '2D', 1: 'function', 2: 'colorMap', 5: 'normalMap', 8: 'specularMap', 11: 'water'}


def unpack_vec(p):
    """PackedUnitVec: bytes x,y,z biased by 127, w = scale selector (decodeScale = (w+192)/32385, w=63 -> 1/127)"""
    sc = ((p >> 24) + 192) / 32385.0
    return [((p & 255) - 127) * sc, (((p >> 8) & 255) - 127) * sc, (((p >> 16) & 255) - 127) * sc]


def decode_state(sb):
    a, b = sb
    BL = {0: 'disabled', 1: 'zero', 2: 'one', 3: 'srcColor', 4: 'invSrcColor', 5: 'srcAlpha', 6: 'invSrcAlpha', 7: 'dstAlpha',
          8: 'invDstAlpha', 9: 'dstColor', 10: 'invDstColor'}
    return {'raw': [a, b], 'srcBlendRgb': BL.get(a & 15, a & 15), 'dstBlendRgb': BL.get((a >> 4) & 15, (a >> 4) & 15), 'blendOpRgb': (a >> 8) & 7,
            'alphaTest': 'disabled' if (a >> 11) & 1 else ['gt0', 'lt128', 'ge128'][((a >> 12) & 3) - 1] if ((a >> 12) & 3) else 'none?',
            'cull': ['?', 'none', 'back', 'front'][(a >> 14) & 3], 'srcBlendAlpha': BL.get((a >> 16) & 15, (a >> 16) & 15),
            'dstBlendAlpha': BL.get((a >> 20) & 15, (a >> 20) & 15), 'blendOpAlpha': (a >> 24) & 7, 'colorWriteRgb': (a >> 27) & 1,
            'colorWriteAlpha': (a >> 28) & 1, 'polyLine': (a >> 31) & 1, 'depthWrite': b & 1, 'depthTestDisabled': (b >> 1) & 1,
            'depthTest': ['always', 'less', 'equal', 'lessEqual'][(b >> 2) & 3], 'polygonOffset': (b >> 4) & 3,
            'stencilFront': (b >> 6) & 1, 'stencilBack': (b >> 7) & 1}


if __name__ == '__main__':
    main()
