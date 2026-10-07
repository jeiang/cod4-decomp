use assets::zone::{Asset, DecodeFilter, XAssetType, Zone};
struct K;
impl DecodeFilter for K { fn keep(&self, t: XAssetType) -> bool { matches!(t, XAssetType::RawFile | XAssetType::Material | XAssetType::TechniqueSet) } }
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = std::path::Path::new(&a[1]).join("zone/english").join(format!("{}.ff", a[2]));
    let z = Zone::open(std::io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
    z.decode(&K, |x| match x {
        Asset::Material(m) => {
            let ts = m.technique_set.as_ref().and_then(|t| t.name.as_deref().map(str::to_owned));
            let tx: Vec<String> = m.textures.iter().map(|t| format!("{:08x}/{:02x}", t.name_hash, t.sampler_state)).collect();
            println!("MAT {:?} ts={:?} sort={} tex={:?} consts={}", m.name, ts, m.sort_key, tx, m.constants.len());
        }
        Asset::TechniqueSet(t) => { let n: Vec<usize> = (0..34).filter(|&i| t.techniques[i].is_some()).collect(); println!("TS {:?} {:?}", t.name, n); }
        Asset::RawFile(r) => { if let Some(w) = std::env::var_os("RAWFILE") { if r.name.as_deref() == w.to_str() { println!("{}", String::from_utf8_lossy(&r.data)); } } else { println!("RAW {:?} {}", r.name, r.data.len()) } }
        _ => {}
    }).unwrap();
}
