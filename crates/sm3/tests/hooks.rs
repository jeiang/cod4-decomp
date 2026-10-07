// SPDX-License-Identifier: GPL-3.0-or-later
//! Hand-assembled ps_3_0: `texld r0, v0, s0; mov oC0, r0`. No original content involved.
mod common;
use sm3::{AlphaTest, Compare, Error, Options, SamplerDim, translate};

fn tokens(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|t| t.to_le_bytes()).collect()
}

fn sample_ps() -> Vec<u8> {
    tokens(&[
        0xFFFF_0300,
        0x0200_001F,
        0x8000_0005,
        0x901F_0000, // dcl_texcoord0 v0
        0x0200_001F,
        0x9000_0000,
        0xA00F_0800, // dcl_2d s0
        0x0300_0042,
        0x800F_0000,
        0x90E4_0000,
        0xA0E4_0800, // texld r0, v0, s0
        0x0200_0001,
        0x800F_0800,
        0x80E4_0000, // mov oC0, r0
        0x0000_FFFF,
    ])
}

#[test]
fn plain_translation_has_separate_texture_and_sampler_and_no_discard() {
    let t = translate(&sample_ps(), &Options::default()).unwrap();
    common::validate(&t.wgsl).unwrap();
    assert!(t.wgsl.contains("var tex0: texture_2d<f32>") && t.wgsl.contains("var smp0: sampler;"));
    assert!(!t.wgsl.contains("discard"));
    assert_eq!(t.reflection.samplers[&0].dim, SamplerDim::D2);
}

#[test]
fn alpha_test_discards_on_final_alpha() {
    let opts = Options {
        alpha_test: Some(AlphaTest {
            func: Compare::GreaterEqual,
            reference: 0.5,
        }),
        ..Options::default()
    };
    let t = translate(&sample_ps(), &opts).unwrap();
    common::validate(&t.wgsl).unwrap();
    assert!(t.wgsl.contains("if (!(oc0.w >= 0.5)) { discard; }"));
}

#[test]
fn comparison_sampler_uses_depth_texture() {
    let opts = Options {
        comparison_samplers: [0].into(),
        ..Options::default()
    };
    let t = translate(&sample_ps(), &opts).unwrap();
    common::validate(&t.wgsl).unwrap();
    assert!(t.wgsl.contains("texture_depth_2d") && t.wgsl.contains("sampler_comparison"));
    assert!(t.wgsl.contains("textureSampleCompare("));
    assert!(t.reflection.samplers[&0].comparison);
}

#[test]
fn malformed_input_is_an_error_not_a_panic() {
    let good = sample_ps();
    for n in 0..good.len() {
        let _ = translate(&good[..n], &Options::default());
    }
    assert!(matches!(
        translate(&[1, 2, 3, 4], &Options::default()),
        Err(Error::Malformed(_))
    ));
}
