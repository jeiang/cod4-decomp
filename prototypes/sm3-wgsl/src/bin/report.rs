//! THROWAWAY: run each translation path on every extracted SM3 shader and print pass rates.
use sm3_wgsl_proto::{check::*, spvsplit};
use std::{collections::BTreeMap, fs, path::Path};

fn main() {
    let work = std::env::args().nth(1).unwrap_or("/tmp/sm3wgsl-work".into());
    let w = Path::new(&work);
    let pairs: Vec<(String, String)> = fs::read_to_string(w.join("pairs_sm3.txt")).unwrap().lines()
        .map(|l| { let mut it = l.split(' '); (it.next().unwrap().into(), it.next().unwrap().into()) }).collect();
    let arg = std::env::args().nth(2);
    match arg.as_deref() {
        Some("hist") => { path_c_hist(w); return; }
        Some("c") => { path_c(w); return; }
        Some("wgpu") => { wgpu_check(w); return; }
        Some(h) if h.starts_with("spv:") => { let p: Vec<&str> = h[4..].split(",").collect(); dump_spv(w, p[0], p[1], p[2]); return; }
        Some(h) if h.starts_with("emit:") => { dump_emit(w, &h[5..]); return; }
        Some(h) => { let mut it = h.split(","); dump_a(w, it.next().unwrap(), it.next().unwrap()); return; }
        None => {}
    }
    path_a(w, &pairs);
    path_b(w, "vkd3d18");
    path_b(w, "vkd3d21");
    path_b(w, "dxbcspv");
}

struct Row { kind: String, hash: String, res: Result<(), String> }

fn summarize(name: &str, rows: &[Row]) {
    let n = |k: &str| rows.iter().filter(|r| r.kind == k).count();
    let ok = |k: &str| rows.iter().filter(|r| r.kind == k && r.res.is_ok()).count();
    println!("{name:<44} VS {:>3}/{:<3} ({:5.1}%)   PS {:>3}/{:<3} ({:5.1}%)", ok("vs"), n("vs"), 100.0 * ok("vs") as f64 / n("vs") as f64, ok("ps"), n("ps"), 100.0 * ok("ps") as f64 / n("ps") as f64);
    let mut causes: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for r in rows { if let Err(e) = &r.res { let c = causes.entry(e.clone()).or_default(); if r.kind == "vs" { c.0 += 1 } else { c.1 += 1 } } }
    let mut cv: Vec<_> = causes.into_iter().collect(); cv.sort_by_key(|x| std::cmp::Reverse(x.1 .0 + x.1 .1));
    for (e, (v, p)) in cv.iter().take(6) { println!("      VS {v:>3} PS {p:>3}  {e}"); }
}

fn shader_list(w: &Path) -> Vec<(String, String)> {
    fs::read_to_string(w.join("list_sm3.txt")).unwrap().lines().map(|l| { let mut it = l.split(' '); (it.next().unwrap().into(), it.next().unwrap().into()) }).collect()
}

fn path_a(w: &Path, pairs: &[(String, String)]) {
    for split in [false, true] {
        let mut vs_res: BTreeMap<String, Result<(), String>> = BTreeMap::new();
        let mut ps_res: BTreeMap<String, Result<(), String>> = BTreeMap::new();
        let mut pair_ok = 0;
        for (v, p) in pairs {
            let run = |file: String| -> Result<String, Fail> {
                let b = fs::read(w.join("mojo_out").join(file)).map_err(|e| Fail::new("io", e.to_string()))?;
                let mut words = words_of(&b);
                if split { words = spvsplit::split_samplers(&words).map_err(|e| Fail::new("split", e))?.0; }
                spv_to_wgsl(&words)
            };
            let vr = vs_res.entry(v.clone()).or_insert_with(|| run(format!("vs_{v}.spv")).map(|_| ()).map_err(|f| bucket(&f)));
            let vok = vr.is_ok();
            let pr = run(format!("ps_{p}__{v}.spv")).map(|_| ()).map_err(|f| bucket(&f));
            let pok = pr.is_ok();
            ps_res.entry(p.clone()).and_modify(|e| if e.is_ok() && pr.is_err() { *e = pr.clone(); }).or_insert(pr);
            if vok && pok { pair_ok += 1; }
        }
        let mut rows = vec![];
        for (h, r) in vs_res { rows.push(Row { kind: "vs".into(), hash: h, res: r }); }
        for (h, r) in ps_res { rows.push(Row { kind: "ps".into(), hash: h, res: r }); }
        summarize(&format!("(a) MojoShader->SPIR-V{}", if split { " + sampler split" } else { "" }), &rows);
        println!("      pairs (VS+PS both ok): {pair_ok}/{}", pairs.len());
    }
}

fn path_b(w: &Path, ver: &str) {
    for split in [false, true] {
        let mut rows = vec![];
        for (k, h) in shader_list(w) {
            let res = (|| -> Result<(), Fail> {
                let b = fs::read(w.join(format!("{ver}_out")).join(format!("{k}_{h}.spv"))).map_err(|_| Fail::new("vkd3d", "tool rejected shader (see vkd3d_log)"))?;
                let mut words = words_of(&b);
                if ver == "dxbcspv" { words = spvsplit::downgrade_vulkan_features(&words).map_err(|e| Fail::new("split", e))?; }
                if split { words = spvsplit::split_samplers(&words).map_err(|e| Fail::new("split", e))?.0; words = spvsplit::pointsize_to_location(&words).map_err(|e| Fail::new("split", e))?.0; }
                spv_to_wgsl(&words).map(|_| ())
            })().map_err(|f| bucket(&f));
            rows.push(Row { kind: k, hash: h, res });
        }
        summarize(&format!("(b) {ver} ->SPIR-V{}", if split { " + sampler split + PointSize fixup" } else { "" }), &rows);
    }
    // tool-level causes (before naga)
    let log = fs::read_to_string(w.join(format!("{ver}_log.txt"))).unwrap();
    let mut c: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for l in log.lines().filter(|l| l.contains(" ERR ")) {
        let kind = l.split(' ').next().unwrap();
        let msg = l.splitn(4, ' ').nth(3).unwrap_or("");
        let msg: String = msg.split(": ").skip(1).collect::<Vec<_>>().join(": ");
        let e = c.entry(msg.chars().take(80).collect()).or_default(); if kind == "vs" { e.0 += 1 } else { e.1 += 1 }
    }
    let mut cv: Vec<_> = c.into_iter().collect(); cv.sort_by_key(|x| std::cmp::Reverse(x.1 .0 + x.1 .1));
    println!("   vkd3d-shader own rejection reasons:"); for (e, (v, p)) in cv.iter().take(12) { println!("      VS {v:>3} PS {p:>3}  {e}"); }
}

#[allow(dead_code)]
pub fn dump_a(w: &Path, v: &str, p: &str) {
    for (f, n) in [(format!("vs_{v}.spv"), "vs"), (format!("ps_{p}__{v}.spv"), "ps")] {
        let words = spvsplit::split_samplers(&words_of(&fs::read(w.join("mojo_out").join(f)).unwrap())).unwrap().0;
        println!("// ---- {n}\n{}", spv_to_wgsl(&words).unwrap());
    }
}

fn path_c_hist(w: &Path) {
    use sm3_wgsl_proto::sm3;
    for st in [sm3::Stage::Vertex, sm3::Stage::Pixel] {
        let mut h = sm3::Hist::default(); let mut n = 0;
        for (k, hash) in shader_list(w) {
            if (k == "vs") != (st == sm3::Stage::Vertex) { continue; }
            let b = fs::read(w.join("shaders").join(format!("{k}_{hash}.bin"))).unwrap();
            let s = sm3::parse(&b).unwrap(); h.add(&s); n += 1;
        }
        println!("== {:?} shaders: {n}", st);
        println!("-- opcodes (instances / shaders using)");
        let mut v: Vec<_> = h.ops.iter().collect(); v.sort_by_key(|x| std::cmp::Reverse(*x.1));
        for (k, c) in v { println!("  {k:<14} {c:>7} {:>5}", h.shaders_with[k]); }
        println!("-- features (instances / shaders using)");
        for (k, c) in &h.feats { println!("  {k:<40} {c:>7} {:>5}", h.shaders_with[k]); }
    }
}

pub fn dump_spv(w: &Path, dir: &str, k: &str, h: &str) {
    let mut words = words_of(&fs::read(w.join(dir).join(format!("{k}_{h}.spv"))).unwrap());
    words = spvsplit::split_samplers(&words).unwrap().0; words = spvsplit::pointsize_to_location(&words).unwrap().0;
    println!("{}", spv_to_wgsl(&words).unwrap());
}

fn path_c(w: &Path) {
    use sm3_wgsl_proto::{emit, sm3};
    let mut rows = vec![]; let mut fails: BTreeMap<String, usize> = BTreeMap::new();
    for (k, hash) in shader_list(w) {
        let b = fs::read(w.join("shaders").join(format!("{k}_{hash}.bin"))).unwrap();
        let res = (|| -> Result<(), Fail> {
            let s = sm3::parse(&b).map_err(|e| Fail::new("sm3-parse", e))?;
            let e = emit::emit(&s).map_err(|e| Fail::new("emit-unsupported", e))?;
            check_wgsl(&e.wgsl)
        })().map_err(|f| bucket(&f));
        if let Err(e) = &res { *fails.entry(e.clone()).or_default() += 1; }
        rows.push(Row { kind: k, hash, res });
    }
    summarize("(c) custom SM3 token stream -> WGSL", &rows);
}

fn dump_emit(w: &Path, kh: &str) {
    let (k, h) = kh.split_once(',').unwrap();
    let b = fs::read(w.join("shaders").join(format!("{k}_{h}.bin"))).unwrap();
    let e = sm3_wgsl_proto::emit::emit(&sm3_wgsl_proto::sm3::parse(&b).unwrap()).unwrap();
    println!("{}", e.wgsl);
    println!("{:?}", check_wgsl(&e.wgsl).err());
}

/// Final arbiter: hand each path's WGSL to a real wgpu device (Metal here) and see if create_shader_module is clean.
fn wgpu_check(w: &Path) {
    use sm3_wgsl_proto::{emit, sm3};
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).expect("adapter");
    println!("adapter: {:?}", adapter.get_info());
    let (device, _q) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let try_module = |wgsl: &str| -> Result<(), String> {
        let g = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let _m = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(wgsl.into()) });
        match pollster::block_on(g.pop()) { None => Ok(()), Some(e) => Err(e.to_string().lines().next().unwrap_or("").chars().take(100).collect()) }
    };
    let pairs: Vec<(String, String)> = fs::read_to_string(w.join("pairs_sm3.txt")).unwrap().lines().map(|l| { let mut it = l.split(' '); (it.next().unwrap().into(), it.next().unwrap().into()) }).collect();
    let _ = pairs;
    let mut rows: BTreeMap<&str, Vec<Row>> = BTreeMap::new();
    for (k, h) in shader_list(w) {
        let code = fs::read(w.join("shaders").join(format!("{k}_{h}.bin"))).unwrap();
        // (a) MojoShader: first pairing's SPIR-V
        let a = (|| -> Result<String, String> {
            let f = if k == "vs" { format!("vs_{h}.spv") } else {
                let e = fs::read_dir(w.join("mojo_out")).unwrap().filter_map(|e| e.ok()).find(|e| e.file_name().to_string_lossy().starts_with(&format!("ps_{h}__"))).ok_or("no spv")?; e.file_name().to_string_lossy().into() };
            let words = spvsplit::split_samplers(&words_of(&fs::read(w.join("mojo_out").join(f)).map_err(|e| e.to_string())?))?.0;
            spv_to_wgsl(&words).map_err(|f| bucket(&f))
        })();
        let b = (|| -> Result<String, String> {
            let words = words_of(&fs::read(w.join("vkd3d21_out").join(format!("{k}_{h}.spv"))).map_err(|e| e.to_string())?);
            let words = spvsplit::pointsize_to_location(&spvsplit::split_samplers(&words)?.0)?.0;
            spv_to_wgsl(&words).map_err(|f| bucket(&f))
        })();
        let c = (|| -> Result<String, String> { let e = emit::emit(&sm3::parse(&code)?)?; Ok(e.wgsl) })();
        for (name, r) in [("a mojoshader+split", a), ("b vkd3d-shader 2.1", b), ("c custom", c)] {
            let res = r.and_then(|wg| try_module(&wg));
            rows.entry(name).or_default().push(Row { kind: k.clone(), hash: h.clone(), res });
        }
    }
    for (n, r) in &rows { summarize(&format!("wgpu create_shader_module: {n}"), r); }
}
