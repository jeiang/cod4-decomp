// SPDX-License-Identifier: GPL-3.0-or-later
//! Throwaway: lights, shadow geometry and art of the stock maps.
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let install = std::path::Path::new(&a[1]);
    for map in &a[2..] {
        let d = render::MapData::load(install, map).unwrap();
        let w = &d.world;
        println!("== {map}: sun_idx {} light_count {} com_lights {} art {:?}", w.sun_primary_light_index, w.primary_light_count, d.com_lights.len(), d.art);
        for (i, l) in d.com_lights.iter().enumerate() {
            println!("  light {i}: kind {} shadow {} exp {} color {:?} radius {} origin {:?} dir {:?} def {:?}", l.kind, l.can_use_shadow_map, l.exponent, l.color, l.radius, l.origin, l.dir, l.def_name);
        }
        for d in &d.light_defs { println!("  def {:?} start {} image {:?}", d.name, d.lmap_lookup_start, d.attenuation_image.as_ref().map(|i| (i.name.clone(), i.width, i.height, i.map_type))); }
        for (i, l) in w.lightmaps.iter().enumerate() {
            for (n, im) in [("primary", &l.primary), ("secondary", &l.secondary)] {
                if let Some(im) = im { if let Some(d) = &im.load_def { println!("  lightmap {i} {n} {:?} fmt {} bytes {}", d.dimensions, d.format, d.data.len());
                    if n == "secondary" { let w = d.dimensions[0] as usize; let row: Vec<u8> = (30..70).map(|x| d.data[x*4+2]).collect(); println!("   row0 R[30..70] {row:?} (w={w})"); } } }
            }
        }
        let mut hist = std::collections::BTreeMap::new();
        for s in &w.dpvs.surfaces { *hist.entry(s.primary_light_index).or_insert(0u32) += 1; }
        let mut mh = std::collections::BTreeMap::new();
        for s in &w.dpvs.smodel_draw_insts { *mh.entry(s.primary_light_index).or_insert(0u32) += 1; }
        println!("  surfaces per light {hist:?} models per light {mh:?}");
        for (i, g) in w.shadow_geometry.iter().enumerate() {
            println!("  shadow geom {i}: {} surfaces {} models", g.sorted_surf_index.len(), g.smodel_index.len());
        }
        for li in 2..5u8 {
            if let Some(sf) = w.dpvs.surfaces.iter().find(|s| s.primary_light_index == li) {
                println!("  light {li} surface bounds {:?} material {:?}", sf.bounds, sf.material.as_ref().map(|m| (m.name.clone(), m.technique_set.as_ref().and_then(|t| t.name.clone()))));
            }
        }
        println!("  total surfaces {} models {} bounds {:?} {:?}", w.dpvs.surfaces.len(), w.dpvs.smodel_draw_insts.len(), w.mins, w.maxs);
        println!("  sun_light {:?}", w.sun_light.as_ref().map(|l| (l.kind, l.color, l.dir, l.can_use_shadow_map)));
    }
}
