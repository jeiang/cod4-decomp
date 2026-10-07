//! Engine-supplied ("code") shader constants for the prototype: fixed values + camera matrices.
//! Matrix convention (verified against the lit VS: dp4(pos, c[i]) per row): `TRANSPOSE_*` registers hold the ROWS of
//! the column-vector matrix, plain variants hold its columns.
use crate::codeconst_names::*;
use glam::{Mat4, Vec3, Vec4};

pub struct FrameState {
    pub world: Mat4, pub view: Mat4, pub proj: Mat4,
    pub eye: Vec3,
    pub sun_dir: Vec3, pub sun_color: Vec3,
    pub time: f32,
}

impl FrameState {
    pub fn matrix(&self, kind: u32) -> Mat4 {
        match kind { 0 => self.world, 1 => self.view, 2 => self.proj, 3 => self.view * self.world, 4 => self.proj * self.view, 5 => self.proj * self.view * self.world, _ => Mat4::IDENTITY }
    }
}

fn rows(m: Mat4) -> [[f32; 4]; 4] { let t = m.transpose(); [t.x_axis.to_array(), t.y_axis.to_array(), t.z_axis.to_array(), t.w_axis.to_array()] }
fn cols(m: Mat4) -> [[f32; 4]; 4] { [m.x_axis.to_array(), m.y_axis.to_array(), m.z_axis.to_array(), m.w_axis.to_array()] }

/// value of code constant `idx` row `row`
pub fn code_const(idx: u32, row: u32, st: &FrameState) -> [f32; 4] {
    if idx >= FIRST_CODE_MATRIX {
        let rel = idx - FIRST_CODE_MATRIX; let (kind, var) = (rel / 4, rel % 4);
        let m = st.matrix(kind);
        let m = if var == 1 || var == 3 { m.inverse() } else { m };
        let r = if var >= 2 { rows(m) } else { cols(m) };
        return r[(row & 3) as usize];
    }
    let n = CODE_CONST_NAMES[idx as usize];
    let sd = st.sun_dir;
    match n {
        "SUN_POSITION" => [sd.x, sd.y, sd.z, 0.0],
        "SUN_DIFFUSE" => [st.sun_color.x, st.sun_color.y, st.sun_color.z, 1.0],
        "SUN_SPECULAR" => [st.sun_color.x * 0.5, st.sun_color.y * 0.5, st.sun_color.z * 0.5, 1.0],
        "FOG" => [0.0, 0.0, -0.00006, 0.0],
        "FOG_COLOR" => [0.55, 0.62, 0.7, 1.0],
        "MATERIAL_COLOR" => [1.0; 4],
        "LIGHTING_LOOKUP_SCALE" => [1.0, 1.0, 1.0, 1.0],
        "GAMETIME" => [st.time, st.time, st.time, st.time],
        "RENDER_TARGET_SIZE" => [1280.0, 720.0, 1.0 / 1280.0, 1.0 / 720.0],
        "SHADOWMAP_SWITCH_PARTITION" => [1.0e9, 0.0, 0.0, 0.0],
        "SHADOWMAP_SCALE" => [0.0, 0.0, 1.0, 1.0],
        "ZNEAR" => [4.0, 0.0, 0.0, 0.0],
        "ENVMAP_PARMS" => [0.0, 0.0, 0.0, 0.0],
        "LIGHT_DIFFUSE" | "LIGHT_SPECULAR" => [0.0, 0.0, 0.0, 0.0],
        _ => [0.0; 4],
    }
}

pub fn eye_light_dir(eye: Vec3) -> Vec3 { let _ = eye; Vec3::new(0.3, 0.2, 0.9).normalize() }
pub fn _unused(_: Vec4) {}
