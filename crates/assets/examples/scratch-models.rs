use assets::zone::{Asset, KeepAll, Zone};
fn main() {
    let root = std::env::var("COD4_PATH").unwrap();
    for z in std::env::args().skip(1) {
        let f = std::fs::File::open(format!("{root}/zone/english/{z}.ff")).unwrap();
        let zone = Zone::open(std::io::BufReader::new(f)).unwrap();
        zone.decode(&KeepAll, |a| match a {
            Asset::XModel(m) => println!(
                "{z} xmodel {} bones={} lods={} surfs={}",
                m.name.as_deref().unwrap_or("?"),
                m.num_bones,
                m.num_lods,
                m.surfs.len()
            ),
            Asset::Weapon(w) => println!(
                "{z} weapon {} gun={:?} hand={:?} world={:?} anims={:?} stand={:?}",
                w.internal_name.as_deref().unwrap_or("?"),
                w.gun_models
                    .iter()
                    .map(|g| g.as_ref().and_then(|m| m.name.clone()))
                    .collect::<Vec<_>>(),
                w.hand_model.as_ref().and_then(|m| m.name.clone()),
                w.world_models
                    .iter()
                    .map(|g| g.as_ref().and_then(|m| m.name.clone()))
                    .collect::<Vec<_>>(),
                w.anims,
                w.v_stand_move
            ),
            Asset::RawFile(r)
                if r.name.as_deref().is_some_and(|n| {
                    n.starts_with("mptype/mptype_ally_cqb")
                        || n.starts_with("maps/mp/gametypes/_teams")
                }) =>
            {
                println!(
                    "{z} RAW {}\n{}",
                    r.name.as_deref().unwrap(),
                    String::from_utf8_lossy(&r.data)
                )
            }
            Asset::XAnimParts(x) => println!(
                "{z} anim {} frames={} rate={} loop={}",
                x.name.as_deref().unwrap_or("?"),
                x.num_frames,
                x.frame_rate,
                x.looping
            ),
            _ => {}
        })
        .unwrap();
    }
}
