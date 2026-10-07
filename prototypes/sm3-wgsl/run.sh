#!/bin/sh
# THROWAWAY: end-to-end reproduction. usage: ./run.sh [COD4_DIR]   (default ../../COD4 relative to the main checkout)
# Everything derived from the original install goes to $WORK (outside the repo).
set -e
COD4=${1:-/Users/aidanp/Projects/cod4-decomp/COD4}; WORK=${WORK:-/tmp/sm3wgsl-work}; HERE=$(cd "$(dirname "$0")" && pwd)
CARGO="nix shell nixpkgs#cargo nixpkgs#rustc -c cargo"
python3 $HERE/tools/extract.py $COD4/zone/english $WORK            # 1. shader blobs + techset context -> $WORK/{shaders,index.json}
python3 $HERE/tools/world_extract.py $COD4/zone/english/mp_crash.ff $WORK/world   # 1b. mp_crash world geometry/materials
python3 - <<PY                                                      # lists: SM3 shaders and unique (vs,ps) pairs
import json
idx=json.load(open('$WORK/index.json')); sh={s['hash']:s for s in idx['shaders']}; pairs=set()
for ts in idx['techsets'].values():
    for t in ts['data']['techniques']:
        for P in (t or {}).get('passes',[]):
            v,p=P.get('vs'),P.get('ps')
            if isinstance(v,dict) and isinstance(p,dict) and 'hash' in v and 'hash' in p and sh[v['hash']]['version']=='fffe0300' and sh[p['hash']]['version']=='ffff0300': pairs.add((v['hash'],p['hash']))
open('$WORK/pairs_sm3.txt','w').write(''.join('%s %s\n'%x for x in sorted(pairs)))
open('$WORK/list_sm3.txt','w').write(''.join('%s %s\n'%(s['kind'],s['hash']) for s in idx['shaders'] if s['version'] in ('fffe0300','ffff0300')))
PY
[ -x $WORK/bin/mojo_drv ] || sh $HERE/tools/build_c_tools.sh                                   # 2. build translators
mkdir -p $WORK/mojo_out $WORK/vkd3d21_out $WORK/dxbcspv_out
$WORK/bin/mojo_drv $WORK/pairs_sm3.txt $WORK/shaders $WORK/mojo_out > $WORK/mojo_log.txt
$WORK/bin/vkd3d_drv $WORK/list_sm3.txt $WORK/shaders $WORK/vkd3d21_out > $WORK/vkd3d21_log.txt
while read k h; do $WORK/dxbcspv/build/tools/dxbc_compiler --no-debug --no-colors --spv $WORK/dxbcspv_out/${k}_$h.spv $WORK/shaders/${k}_$h.bin >/dev/null 2>&1 && echo "$k $h OK" || echo "$k $h ERR"; done < $WORK/list_sm3.txt > $WORK/dxbcspv_log.txt
cd $HERE && $CARGO build --release                                                              # 3. rust: report + viewer
./target/release/report $WORK; ./target/release/report $WORK c; ./target/release/report $WORK wgpu; ./target/release/report $WORK hist
./target/release/viewer --work $WORK --cod4 $COD4                                                # 4. window
