// SPDX-License-Identifier: GPL-3.0-or-later
//! Engine-supplied ("code") shader constants and samplers: their ids, their CTAB names, and the per-frame values.
//!
//! Technique passes bind a register to a code constant by id; the shader's CTAB names the same constant (for example
//! `sunPosition` for [`SUN_POSITION`]). [`from_ctab_name`] maps the name back to the id, which is what lets a bare
//! shader blob (no technique arguments) be bound, as the render-diff cross-check does.

use glam::{Mat4, Vec3};

pub const NAMES: [&str; 0x5A] = [
    "LIGHT_POSITION",
    "LIGHT_DIFFUSE",
    "LIGHT_SPECULAR",
    "LIGHT_SPOTDIR",
    "LIGHT_SPOTFACTORS",
    "NEARPLANE_ORG",
    "NEARPLANE_DX",
    "NEARPLANE_DY",
    "SHADOW_PARMS",
    "SHADOWMAP_POLYGON_OFFSET",
    "RENDER_TARGET_SIZE",
    "LIGHT_FALLOFF_PLACEMENT",
    "DOF_EQUATION_VIEWMODEL_AND_FAR_BLUR",
    "DOF_EQUATION_SCENE",
    "DOF_LERP_SCALE",
    "DOF_LERP_BIAS",
    "DOF_ROW_DELTA",
    "PARTICLE_CLOUD_COLOR",
    "GAMETIME",
    "PIXEL_COST_FRACS",
    "PIXEL_COST_DECODE",
    "FILTER_TAP_0",
    "FILTER_TAP_1",
    "FILTER_TAP_2",
    "FILTER_TAP_3",
    "FILTER_TAP_4",
    "FILTER_TAP_5",
    "FILTER_TAP_6",
    "FILTER_TAP_7",
    "COLOR_MATRIX_R",
    "COLOR_MATRIX_G",
    "COLOR_MATRIX_B",
    "SHADOWMAP_SWITCH_PARTITION",
    "SHADOWMAP_SCALE",
    "ZNEAR",
    "SUN_POSITION",
    "SUN_DIFFUSE",
    "SUN_SPECULAR",
    "LIGHTING_LOOKUP_SCALE",
    "DEBUG_BUMPMAP",
    "MATERIAL_COLOR",
    "FOG",
    "FOG_COLOR",
    "GLOW_SETUP",
    "GLOW_APPLY",
    "COLOR_BIAS",
    "COLOR_TINT_BASE",
    "COLOR_TINT_DELTA",
    "OUTDOOR_FEATHER_PARMS",
    "ENVMAP_PARMS",
    "SPOT_SHADOWMAP_PIXEL_ADJUST",
    "CLIP_SPACE_LOOKUP_SCALE",
    "CLIP_SPACE_LOOKUP_OFFSET",
    "PARTICLE_CLOUD_MATRIX",
    "DEPTH_FROM_CLIP",
    "CODE_MESH_ARG_0",
    "CODE_MESH_ARG_1",
    "BASE_LIGHTING_COORDS",
    "WORLD_MATRIX",
    "INVERSE_WORLD_MATRIX",
    "TRANSPOSE_WORLD_MATRIX",
    "INVERSE_TRANSPOSE_WORLD_MATRIX",
    "VIEW_MATRIX",
    "INVERSE_VIEW_MATRIX",
    "TRANSPOSE_VIEW_MATRIX",
    "INVERSE_TRANSPOSE_VIEW_MATRIX",
    "PROJECTION_MATRIX",
    "INVERSE_PROJECTION_MATRIX",
    "TRANSPOSE_PROJECTION_MATRIX",
    "INVERSE_TRANSPOSE_PROJECTION_MATRIX",
    "WORLD_VIEW_MATRIX",
    "INVERSE_WORLD_VIEW_MATRIX",
    "TRANSPOSE_WORLD_VIEW_MATRIX",
    "INVERSE_TRANSPOSE_WORLD_VIEW_MATRIX",
    "VIEW_PROJECTION_MATRIX",
    "INVERSE_VIEW_PROJECTION_MATRIX",
    "TRANSPOSE_VIEW_PROJECTION_MATRIX",
    "INVERSE_TRANSPOSE_VIEW_PROJECTION_MATRIX",
    "WORLD_VIEW_PROJECTION_MATRIX",
    "INVERSE_WORLD_VIEW_PROJECTION_MATRIX",
    "TRANSPOSE_WORLD_VIEW_PROJECTION_MATRIX",
    "INVERSE_TRANSPOSE_WORLD_VIEW_PROJECTION_MATRIX",
    "SHADOW_LOOKUP_MATRIX",
    "INVERSE_SHADOW_LOOKUP_MATRIX",
    "TRANSPOSE_SHADOW_LOOKUP_MATRIX",
    "INVERSE_TRANSPOSE_SHADOW_LOOKUP_MATRIX",
    "WORLD_OUTDOOR_LOOKUP_MATRIX",
    "INVERSE_WORLD_OUTDOOR_LOOKUP_MATRIX",
    "TRANSPOSE_WORLD_OUTDOOR_LOOKUP_MATRIX",
    "INVERSE_TRANSPOSE_WORLD_OUTDOOR_LOOKUP_MATRIX",
];

/// Ids of the vector constants the renderer sets.
pub const RENDER_TARGET_SIZE: u32 = 0x0A;
pub const GAMETIME: u32 = 0x12;
pub const ZNEAR: u32 = 0x22;
pub const SUN_POSITION: u32 = 0x23;
pub const SUN_DIFFUSE: u32 = 0x24;
pub const SUN_SPECULAR: u32 = 0x25;
pub const LIGHTING_LOOKUP_SCALE: u32 = 0x26;
pub const MATERIAL_COLOR: u32 = 0x28;
pub const FOG: u32 = 0x29;
pub const FOG_COLOR: u32 = 0x2A;
pub const COLOR_BIAS: u32 = 0x2D;
pub const COLOR_TINT_BASE: u32 = 0x2E;
pub const OUTDOOR_FEATHER_PARMS: u32 = 0x30;
pub const ENVMAP_PARMS: u32 = 0x31;
pub const BASE_LIGHTING_COORDS: u32 = 0x39;
pub const SHADOWMAP_SWITCH_PARTITION: u32 = 0x20;
pub const SHADOWMAP_SCALE: u32 = 0x21;
/// Number of vector constants; ids from here are matrices (`kind * 4 + variant`).
pub const FIRST_MATRIX: u32 = 0x3A;
pub const COUNT: u32 = 0x5A;

/// Code textures, by the id carried in a code-sampler argument.
pub const TEXTURE_NAMES: [&str; 27] = [
    "BLACK",
    "WHITE",
    "IDENTITY_NORMAL_MAP",
    "MODEL_LIGHTING",
    "LIGHTMAP_PRIMARY",
    "LIGHTMAP_SECONDARY",
    "SHADOWCOOKIE",
    "SHADOWMAP_SUN",
    "SHADOWMAP_SPOT",
    "FEEDBACK",
    "RESOLVED_POST_SUN",
    "RESOLVED_SCENE",
    "POST_EFFECT_0",
    "POST_EFFECT_1",
    "SKY",
    "LIGHT_ATTENUATION",
    "DYNAMIC_SHADOWS",
    "OUTDOOR",
    "FLOATZ",
    "PROCESSED_FLOATZ",
    "RAW_FLOATZ",
    "CASE_TEXTURE",
    "CINEMATIC_Y",
    "CINEMATIC_CR",
    "CINEMATIC_CB",
    "CINEMATIC_A",
    "REFLECTION_PROBE",
];

pub mod tex {
    pub const BLACK: u32 = 0;
    pub const WHITE: u32 = 1;
    pub const IDENTITY_NORMAL_MAP: u32 = 2;
    pub const MODEL_LIGHTING: u32 = 3;
    pub const LIGHTMAP_PRIMARY: u32 = 4;
    pub const LIGHTMAP_SECONDARY: u32 = 5;
    pub const SHADOWMAP_SUN: u32 = 7;
    pub const SHADOWMAP_SPOT: u32 = 8;
    pub const SKY: u32 = 14;
    pub const OUTDOOR: u32 = 17;
    pub const FLOATZ: u32 = 18;
    pub const REFLECTION_PROBE: u32 = 26;
}

fn norm(s: &str, drop: &str) -> String {
    s.to_ascii_lowercase().replace('_', "").replace(drop, "")
}

/// CTAB names that differ from the id name beyond case and underscores.
const CONST_ALIASES: &[(&str, &str)] = &[
    ("featherparms", "outdoorfeatherparms"),
    ("time", "gametime"),
    ("nearplaneorg", "nearplaneorg"),
];
const TEXTURE_ALIASES: &[(&str, &str)] = &[
    ("skymap", "sky"),
    ("outdoormap", "outdoor"),
    ("attenuation", "lightattenuation"),
    ("colormappostsun", "resolvedpostsun"),
    ("dynamicshadow", "dynamicshadows"),
];

fn lookup(names: &[&str], key: &str, drop: &str, aliases: &[(&str, &str)]) -> Option<u32> {
    let mut k = norm(key, drop);
    if let Some((_, to)) = aliases.iter().find(|(from, _)| *from == k) {
        k = (*to).to_owned();
    }
    names.iter().position(|n| norm(n, "") == k).map(|i| i as u32)
}

/// Code-constant id for a CTAB constant name (`worldViewProjectionMatrix`, `sunPosition`, ...). `filterTap` and other
/// arrays resolve to the first element.
pub fn from_ctab_name(name: &str) -> Option<u32> {
    let n = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '[' || c == ']');
    lookup(&NAMES, n, "", CONST_ALIASES).or_else(|| lookup(&NAMES, &format!("{n}0"), "", &[]))
}

/// Code-texture id for a CTAB sampler name (`lightmapSamplerPrimary`, `skyMapSampler`, ...).
pub fn texture_from_ctab_name(name: &str) -> Option<u32> {
    lookup(&TEXTURE_NAMES, name, "sampler", TEXTURE_ALIASES)
}

/// Per-object values.
#[derive(Clone, Copy)]
pub struct Object {
    pub world: Mat4,
    pub base_lighting: [f32; 4],
}

impl Default for Object {
    fn default() -> Self {
        Object {
            world: Mat4::IDENTITY,
            base_lighting: [0.0, 0.0, 0.5, 1.0],
        }
    }
}

/// Everything a frame fixes: camera matrices and the vector constants.
pub struct FrameConsts {
    pub view: Mat4,
    pub proj: Mat4,
    pub vec: [[f32; 4]; FIRST_MATRIX as usize],
    pub shadow_lookup: Mat4,
    pub outdoor_lookup: Mat4,
}

impl FrameConsts {
    pub fn new(view: Mat4, proj: Mat4) -> Self {
        let mut vec = [[0.0; 4]; FIRST_MATRIX as usize];
        vec[MATERIAL_COLOR as usize] = [1.0; 4];
        vec[0x26] = [0.005_859_375, 1.0 / 256.0, 0.375, 0.0];
        vec[SHADOWMAP_SWITCH_PARTITION as usize] = [1.0e9, 0.0, 0.0, 0.0];
        vec[SHADOWMAP_SCALE as usize] = [0.0, 0.0, 1.0, 1.0];
        // 30/ 0 colour matrix passthrough rows used by post shaders
        vec[0x1D] = [1.0, 0.0, 0.0, 0.0];
        vec[0x1E] = [0.0, 1.0, 0.0, 0.0];
        vec[0x1F] = [0.0, 0.0, 1.0, 0.0];
        FrameConsts {
            view,
            proj,
            vec,
            shadow_lookup: Mat4::IDENTITY,
            outdoor_lookup: Mat4::IDENTITY,
        }
    }

    pub fn set_sun(&mut self, dir: Vec3, color: Vec3, specular_scale: f32) {
        self.vec[SUN_POSITION as usize] = [dir.x, dir.y, dir.z, 0.0];
        self.vec[SUN_DIFFUSE as usize] = [color.x, color.y, color.z, 1.0];
        let s = color * specular_scale;
        self.vec[SUN_SPECULAR as usize] = [s.x, s.y, s.z, 1.0];
    }

    fn matrix(&self, kind: u32, obj: &Object) -> Mat4 {
        match kind {
            0 => obj.world,
            1 => self.view,
            2 => self.proj,
            3 => self.view * obj.world,
            4 => self.proj * self.view,
            5 => self.proj * self.view * obj.world,
            6 => self.shadow_lookup,
            _ => self.outdoor_lookup * obj.world,
        }
    }

    /// Row `row` of code constant `id` for `obj`. Matrix variants (`id - FIRST_MATRIX = kind * 4 + variant`, variant 0
    /// plain, 1 inverse, 2 transpose, 3 inverse transpose): the transpose variants hold the rows of the matrix
    /// (column-vector convention), the others its columns, which is what `dp4(pos, c[i])` in the stock vertex shaders
    /// expects.
    pub fn value(&self, id: u32, row: u32, obj: &Object) -> [f32; 4] {
        if id < FIRST_MATRIX {
            return if id == BASE_LIGHTING_COORDS {
                obj.base_lighting
            } else {
                self.vec[id as usize]
            };
        }
        let rel = id - FIRST_MATRIX;
        let (kind, variant) = (rel / 4, rel % 4);
        if kind > 7 {
            return [0.0; 4];
        }
        let m = self.matrix(kind, obj);
        let m = if variant & 1 == 1 { m.inverse() } else { m };
        let m = if variant >= 2 { m.transpose() } else { m };
        m.col(row as usize & 3).to_array()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctab_names_resolve_to_ids() {
        assert_eq!(from_ctab_name("sunPosition"), Some(SUN_POSITION));
        assert_eq!(from_ctab_name("fogColor"), Some(FOG_COLOR));
        assert_eq!(from_ctab_name("worldViewProjectionMatrix"), Some(0x4E));
        assert_eq!(from_ctab_name("inverseTransposeViewMatrix"), Some(0x41));
        assert_eq!(from_ctab_name("filterTap"), Some(0x15));
        assert_eq!(from_ctab_name("featherParms"), Some(OUTDOOR_FEATHER_PARMS));
        assert_eq!(from_ctab_name("noSuchConstant"), None);
        assert_eq!(texture_from_ctab_name("lightmapSamplerPrimary"), Some(tex::LIGHTMAP_PRIMARY));
        assert_eq!(texture_from_ctab_name("skyMapSampler"), Some(tex::SKY));
        assert_eq!(texture_from_ctab_name("modelLightingSampler"), Some(tex::MODEL_LIGHTING));
    }

    #[test]
    fn transpose_rows_are_matrix_rows() {
        let proj = Mat4::from_cols_array(&[1., 2., 3., 4., 5., 6., 7., 8., 9., 10., 11., 12., 13., 14., 15., 16.]);
        let f = FrameConsts::new(Mat4::IDENTITY, proj);
        let o = Object::default();
        let plain = f.value(0x42, 0, &o);
        let tr = f.value(0x44, 0, &o);
        assert_eq!(plain, [1., 2., 3., 4.]);
        assert_eq!(tr, [1., 5., 9., 13.]);
    }
}
