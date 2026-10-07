use assets::vfs::Vfs;
use render::material::VertexKind;
use render::{Gpu, MapData, Renderer, Scene, TextureCache};
use std::sync::Arc;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let install = std::path::Path::new(&a[1]);
    let gpu = Arc::new(Gpu::new(None).unwrap());
    let data = MapData::load(install, &a[2]).unwrap();
    let scene = Scene::new(&gpu, &data);
    let vfs = Vfs::open_stock(install, 0).unwrap();
    let mut r = Renderer::new(gpu.clone(), scene, &data, TextureCache::new(Some(vfs), 0));
    let pat = a.get(3).cloned().unwrap_or_default();
    for m in &data.post_materials {
        let name = m.name.clone().unwrap_or_default();
        let ts = m.technique_set.as_ref();
        let have: Vec<usize> = ts
            .map(|t| {
                (0..t.techniques.len())
                    .filter(|&i| t.techniques[i].is_some())
                    .collect()
            })
            .unwrap_or_default();
        eprintln!(
            "== {name} techset {:?} techs {have:?} tex {:?} consts {:?}",
            ts.and_then(|t| t.name.clone()),
            m.textures.iter().map(|t| t.name_hash).collect::<Vec<_>>(),
            m.constants
                .iter()
                .map(|c| (c.name_hash, c.literal))
                .collect::<Vec<_>>()
        );
        if let Some(d) = ts
            .and_then(|t| t.techniques.get(4))
            .and_then(|t| t.as_ref())
            .and_then(|t| t.passes.first())
            .and_then(|p| p.vertex_decl.as_ref())
        {
            eprintln!(
                "   decl streams {} routing {:?}",
                d.stream_count,
                &d.routing[..usize::from(d.stream_count).min(16)]
            );
        }
        if !pat.is_empty() && name.contains(&pat) {
            for &t in &have {
                if let Some(p) = r.inspect(m, &[t], VertexKind::World) {
                    eprintln!("{}", p.describe());
                    if std::env::var_os("WGSL").is_some() {
                        eprintln!("// ---- vs\n{}\n// ---- ps\n{}", p.vs_wgsl(), p.ps_wgsl());
                    }
                }
            }
        }
    }
    eprintln!("failures {:?}", r.materials.failures);
}
