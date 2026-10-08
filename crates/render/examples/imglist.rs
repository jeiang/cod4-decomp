use assets::vfs::Vfs;
use assets::zone::gfx::TextureSource;
use render::{Gpu, MapData, Scene};
use std::sync::Arc;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let install = std::path::Path::new(&a[1]);
    let gpu = Arc::new(Gpu::new(None).unwrap());
    let data = MapData::load(install, &a[2]).unwrap();
    let scene = Scene::new(&gpu, &data);
    let vfs = Vfs::open_stock(install, 0).unwrap();
    let mut seen = std::collections::HashSet::new();
    for m in scene.materials() {
        for t in m.textures.iter() {
            if let TextureSource::Image(Some(img)) = &t.source {
                let n = img.name.clone().unwrap_or_default();
                let n = n.trim_start_matches(',').to_owned();
                if !seen.insert(n.clone()) { continue; }
                let st = t.sampler_state; let inl = img.load_def.as_ref().is_some_and(|d| !d.data.is_empty());
                match assets::iwi::load_picmip(&vfs, &n, 0) {
                    Ok(i) => println!("st={st:02x} {n} iwi={}x{} mips={} flags={:02x} sem={} mat={:?}", i.header.width, i.header.height, i.mip_count(), i.header.flags.0, t.semantic, m.name),
                    Err(e) => println!("ERR {n} {e:?}"),
                }
            }
        }
    }
}
