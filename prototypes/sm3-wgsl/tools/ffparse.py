"""THROWAWAY: minimal IW3 v5 fastfile reader that only decodes MaterialTechniqueSet assets.
Block/pointer rules per research/fastfiles.md. Nothing is written into the repo."""
import struct, zlib, sys
BLOCKS = ['TEMP','RUNTIME','LARGE_RUNTIME','PHYSICAL_RUNTIME','VIRTUAL','LARGE','PHYSICAL','VERTEX','INDEX']
VIRTUAL = 4

class Zone:
    def __init__(self, path):
        raw = open(path,'rb').read()
        assert raw[:8] == b'IWffu100' and struct.unpack('<I',raw[8:12])[0]==5, path
        self.d = zlib.decompress(raw[12:])
        d = self.d
        self.size, self.ext = struct.unpack_from('<II', d, 0)
        self.blocksz = struct.unpack_from('<9I', d, 8)
        self.nstr, _p, self.nasset, _p2 = struct.unpack_from('<IIII', d, 44)
        self.pos = 60
        self.off = [0]*9      # per-block allocation offset
        self.stack = []
    # ---- block allocator ----
    def push(self, b): self.stack.append((b, self.off[b]))
    def pop(self):
        b, saved = self.stack.pop()
        if b == 0: self.off[0] = saved
    def alloc(self, n, align=1):
        b = self.stack[-1][0]
        o = (self.off[b] + align - 1) & ~(align-1)
        self.off[b] = o + n
        return b, o
    def read(self, n, align=1):
        b, o = self.alloc(n, align)
        data = self.d[self.pos:self.pos+n]; assert len(data)==n
        self.pos += n
        return b, o, data
    def u32(self): 
        v = struct.unpack_from('<I', self.d, self.pos)[0]; self.pos += 4; return v
    def cstr(self):
        e = self.d.index(b'\0', self.pos); s = self.d[self.pos:e].decode('latin1')
        self.alloc(e-self.pos+1, 1); self.pos = e+1; return s
    def header(self):
        self.pos = 60
        self.strptrs = struct.unpack_from('<%dI'%self.nstr, self.d, self.pos); self.pos += 4*self.nstr
        self.strings = []
        for p in self.strptrs:
            if p == 0xFFFFFFFF:
                e = self.d.index(b'\0', self.pos); self.strings.append(self.d[self.pos:e].decode('latin1')); self.pos = e+1
            else: self.strings.append(None)
        self.assets = struct.unpack_from('<%dI'%(2*self.nasset), self.d, self.pos); self.pos += 8*self.nasset
        self.types = self.assets[0::2]; self.hptrs = self.assets[1::2]

def ref(p):  # offset pointer -> (block, offset)
    v = p - 1
    return (v >> 28) & 0xF, v & 0x0FFFFFFF

def load_techset(z, reg):
    """reg: dict (block,off)-> object for reusable pointers"""
    z.push(0)
    _, _, h = z.read(148, 4)
    name_p, wvf = struct.unpack_from('<IB', h, 0)
    tp = struct.unpack_from('<34I', h, 12)
    z.push(VIRTUAL)
    ts = {'name': None, 'techniques': [None]*34, 'worldVertFormat': wvf}
    assert name_p == 0xFFFFFFFF
    ts['name'] = z.cstr()
    for i, p in enumerate(tp):
        if p == 0: continue
        if p == 0xFFFFFFFF:
            ts['techniques'][i] = load_technique(z, reg)
        else:
            ts['techniques'][i] = reg[ref(p)]
    z.pop(); z.pop()
    return ts

def load_technique(z, reg):
    # header: name ptr, flags u16, passCount u16 ; then passCount*20 passes
    npass = struct.unpack_from('<H', z.d, z.pos+6)[0]
    b, o, h = z.read(8+20*npass, 4)
    tech = {'flags': struct.unpack_from('<H', h, 4)[0], 'passes': []}
    reg[(b,o)] = tech
    namep = struct.unpack_from('<I', h, 0)[0]
    for k in range(npass):
        decl_p, vs_p, ps_p, nprim, nobj, nstable, csf, args_p = struct.unpack_from('<IIIBBBBI', h, 8+20*k)
        P = {'prim': nprim, 'obj': nobj, 'stable': nstable, 'customSamplerFlags': csf, 'args': []}
        # decl
        if decl_p == 0xFFFFFFFF:
            b2, o2, dd = z.read(100, 4)
            P['decl'] = {'streamCount': dd[0], 'hasOptionalSource': dd[1], 'routing': [(dd[4+2*i], dd[5+2*i]) for i in range(16)]}
            reg[(b2,o2)] = P['decl']
        else: P['decl'] = reg[ref(decl_p)]
        for key, ptr in (('vs', vs_p), ('ps', ps_p)):
            if ptr == 0xFFFFFFFF:
                b2, o2, sh = z.read(16, 4)
                nm_p, _rt, prog_p, psz, lfr = struct.unpack('<IIIHH', sh)
                S = {'name': None}
                assert nm_p == 0xFFFFFFFF; S['name'] = z.cstr()
                assert prog_p == 0xFFFFFFFF
                _,_,S['code'] = z.read(psz*4, 4)
                reg[(b2,o2)] = S
                P[key] = S
            else: P[key] = reg[ref(ptr)]
        n = nprim+nobj+nstable
        if n:
            assert args_p == 0xFFFFFFFF
            b2, o2, ab = z.read(8*n, 4)
            for j in range(n):
                typ, dest, u = struct.unpack_from('<HHI', ab, 8*j)
                A = {'type': typ, 'dest': dest}
                if typ in (1, 7):
                    if u == 0xFFFFFFFF:
                        bb, oo, lit = z.read(16, 4)
                        A['lit'] = struct.unpack('<4f', lit); reg[(bb,oo)] = A['lit']
                    else: A['lit'] = reg[ref(u)]
                elif typ in (3, 5): A['code'] = (u & 0xFFFF, (u>>16)&0xFF, (u>>24)&0xFF)  # index, firstRow, rowCount
                elif typ == 4: A['sampler'] = u
                else: A['hash'] = u
                P['args'].append(A)
        tech['passes'].append(P)
    if namep == 0xFFFFFFFF: tech['name'] = z.cstr()
    return tech

def parse_techsets(path):
    z = Zone(path); z.header()
    reg = {}; out = []
    i = 0
    while i < z.nasset and z.types[i] == 5:
        assert z.hptrs[i] == 0xFFFFFFFF
        out.append(load_techset(z, reg)); i += 1
    return z, out, i

if __name__ == '__main__':
    z, ts, n = parse_techsets(sys.argv[1])
    print(sys.argv[1], 'assets', z.nasset, 'leading techsets', n, 'pos', z.pos, 'virt', z.off[VIRTUAL], 'declared', z.blocksz[VIRTUAL])
    print(sorted(set(z.types[n:]))[:20], z.types[:3], z.types[n:n+5])
