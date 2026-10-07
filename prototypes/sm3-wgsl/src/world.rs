//! Loads the extracted mp_crash world dump + technique-set context (all produced outside the repo by tools/*.py).
use serde_json::Value;
use std::{collections::HashMap, fs, path::Path};

#[derive(Clone)]
pub struct Tex { pub semantic: String, pub hash: u32, pub image: String, pub clamp_u: bool, pub clamp_v: bool, pub filter: u32 }
#[derive(Clone)]
pub struct Mat {
    pub name: String, pub techset: String, pub sort_key: u32,
    pub textures: Vec<Tex>, pub constants: Vec<(u32, String, [f32; 4])>,
    pub state_entry: Vec<u8>, pub state_bits: Vec<Value>,
}
pub struct Surf { pub mat: usize, pub first_vertex: i32, pub first_index: u32, pub tri_count: u32 }
pub struct World {
    pub verts: Vec<u8>, pub indices: Vec<u8>, pub surfs: Vec<Surf>, pub mats: Vec<Mat>,
    pub mins: [f32; 3], pub maxs: [f32; 3], pub sun_dir: [f32; 3], pub sun_color: [f32; 3],
    pub techsets: HashMap<String, Value>, pub shaders: HashMap<String, Value>,
    pub surf_count_by_mat: Vec<usize>,
}

fn f3(v: &Value) -> [f32; 3] { [v[0].as_f64().unwrap_or(0.0) as f32, v[1].as_f64().unwrap_or(0.0) as f32, v[2].as_f64().unwrap_or(0.0) as f32] }

impl World {
    pub fn load(work: &Path) -> World {
        let w = work.join("world");
        let mj: Value = serde_json::from_slice(&fs::read(w.join("materials.json")).unwrap()).unwrap();
        let sj: Value = serde_json::from_slice(&fs::read(w.join("surfaces.json")).unwrap()).unwrap();
        let mut mats = vec![]; let mut by_name = HashMap::new();
        for m in mj["materials"].as_array().unwrap() {
            let textures = m["textures"].as_array().unwrap().iter().map(|t| Tex {
                semantic: t["semanticName"].as_str().unwrap_or("?").into(), hash: t["nameHash"].as_u64().unwrap_or(0) as u32,
                image: t["imageFile"].as_str().or(t["image"].as_str()).unwrap_or("").trim_start_matches(',').into(),
                clamp_u: t["sampler"]["clampU"].as_u64().unwrap_or(0) != 0, clamp_v: t["sampler"]["clampV"].as_u64().unwrap_or(0) != 0, filter: t["sampler"]["filter"].as_u64().unwrap_or(3) as u32,
            }).collect();
            let constants = m["constants"].as_array().unwrap().iter().map(|c| (c["nameHash"].as_u64().unwrap() as u32, c["name"].as_str().unwrap_or("").to_string(), { let v = &c["value"]; [v[0].as_f64().unwrap() as f32, v[1].as_f64().unwrap() as f32, v[2].as_f64().unwrap() as f32, v[3].as_f64().unwrap() as f32] })).collect();
            by_name.insert(m["name"].as_str().unwrap().to_string(), mats.len());
            mats.push(Mat { name: m["name"].as_str().unwrap().into(), techset: m["techset"].as_str().unwrap().trim_start_matches(',').into(), sort_key: m["sortKey"].as_u64().unwrap_or(0) as u32,
                textures, constants, state_entry: m["stateBitsEntry"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u8).collect(), state_bits: m["stateBits"].as_array().cloned().unwrap_or_default() });
        }
        let mut surfs = vec![]; let mut cnt = vec![0usize; mats.len()];
        for s in sj.as_array().unwrap() {
            let mi = by_name[s["material"].as_str().unwrap()]; cnt[mi] += 1;
            surfs.push(Surf { mat: mi, first_vertex: s["firstVertex"].as_i64().unwrap() as i32, first_index: s["firstIndex"].as_u64().unwrap() as u32, tri_count: s["triCount"].as_u64().unwrap() as u32 });
        }
        let idx: Value = serde_json::from_slice(&fs::read(work.join("index.json")).unwrap()).unwrap();
        let mut techsets: HashMap<String, Value> = HashMap::new();
        for (_, ts) in idx["techsets"].as_object().unwrap() {
            let Some(name) = ts["name"].as_str().map(|s| s.to_string()) else { continue };
            let has = ts["data"]["techniques"].as_array().map_or(false, |a| a.iter().any(|t| t.get("passes").is_some()));
            if has { techsets.entry(name).or_insert_with(|| ts["data"].clone()); }
        }
        let mut shaders = HashMap::new();
        for s in idx["shaders"].as_array().unwrap() { shaders.insert(format!("{}_{}", s["kind"].as_str().unwrap(), s["hash"].as_str().unwrap()), s.clone()); }
        let wb = &mj["world"]["world_bounds"];
        let sun = &mj["world"]["sunLight"];
        World {
            verts: fs::read(w.join("verts.bin")).unwrap(), indices: fs::read(w.join("indices.bin")).unwrap(), surfs, mats,
            mins: f3(&wb["mins"]), maxs: f3(&wb["maxs"]), sun_dir: f3(&sun["dir"]), sun_color: f3(&sun["color"]),
            techsets, shaders, surf_count_by_mat: cnt,
        }
    }
}
