// SPDX-License-Identifier: GPL-3.0-or-later
//! `GfxStateBits` (two words per material state) to wgpu pipeline state.
//!
//! Word 0: blend (src/dst RGB 4 bits each, op RGB 3 bits at 8; alpha at 16/20/24; a zero alpha op copies RGB), alpha
//! test (bit 11 disable, 12..13 mode), cull (14..15: 1 none, 2 back, 3 front), colour write (27 RGB, 28 alpha), line
//! polygon mode (31). Word 1: depth write (0), depth test disable (1), depth function (2..3), polygon offset (4..5),
//! stencil enables (6, 7) and front/back stencil ops and functions.

use sm3::{AlphaTest, Compare};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StateBits(pub [u32; 2]);

impl StateBits {
    pub fn blend(self) -> Option<wgpu::BlendState> {
        let a = self.0[0];
        if a & 0x700 == 0 {
            return None;
        }
        let rgb = (a & 0xF, (a >> 4) & 0xF, (a >> 8) & 7);
        let alpha = if a & 0x700_0000 == 0 {
            rgb
        } else {
            ((a >> 16) & 0xF, (a >> 20) & 0xF, (a >> 24) & 7)
        };
        let comp = |(s, d, op): (u32, u32, u32)| wgpu::BlendComponent {
            src_factor: factor(s),
            dst_factor: factor(d),
            operation: match op {
                2 => wgpu::BlendOperation::Subtract,
                3 => wgpu::BlendOperation::ReverseSubtract,
                4 => wgpu::BlendOperation::Min,
                5 => wgpu::BlendOperation::Max,
                _ => wgpu::BlendOperation::Add,
            },
        };
        Some(wgpu::BlendState {
            color: comp(rgb),
            alpha: comp(alpha),
        })
    }

    pub fn alpha_test(self) -> Option<AlphaTest> {
        let a = self.0[0];
        if a & 0x800 != 0 {
            return None;
        }
        match a & 0x3000 {
            0x1000 => Some(AlphaTest {
                func: Compare::Greater,
                reference: 0.0,
            }),
            0x2000 => Some(AlphaTest {
                func: Compare::Less,
                reference: 128.0 / 255.0,
            }),
            0x3000 => Some(AlphaTest {
                func: Compare::GreaterEqual,
                reference: 128.0 / 255.0,
            }),
            _ => None,
        }
    }

    /// D3D clockwise is the front face (the view matrices keep the original handedness).
    pub fn cull(self) -> Option<wgpu::Face> {
        match (self.0[0] >> 14) & 3 {
            2 => Some(wgpu::Face::Back),
            3 => Some(wgpu::Face::Front),
            _ => None,
        }
    }

    pub fn color_write(self) -> wgpu::ColorWrites {
        let a = self.0[0];
        let mut w = wgpu::ColorWrites::empty();
        if a & 0x800_0000 != 0 {
            w |= wgpu::ColorWrites::RED | wgpu::ColorWrites::GREEN | wgpu::ColorWrites::BLUE;
        }
        if a & 0x1000_0000 != 0 {
            w |= wgpu::ColorWrites::ALPHA;
        }
        w
    }

    pub fn wireframe(self) -> bool {
        self.0[0] & 0x8000_0000 != 0
    }

    pub fn depth_write(self) -> bool {
        self.0[1] & 1 != 0
    }

    pub fn depth_compare(self) -> wgpu::CompareFunction {
        let b = self.0[1];
        if b & 2 != 0 {
            return wgpu::CompareFunction::Always;
        }
        match (b >> 2) & 3 {
            0 => wgpu::CompareFunction::Always,
            1 => wgpu::CompareFunction::Less,
            2 => wgpu::CompareFunction::Equal,
            _ => wgpu::CompareFunction::LessEqual,
        }
    }

    pub fn depth_bias(self) -> wgpu::DepthBiasState {
        match (self.0[1] >> 4) & 3 {
            0 => wgpu::DepthBiasState::default(),
            3 => wgpu::DepthBiasState {
                constant: 2,
                slope_scale: 2.0,
                clamp: 0.0,
            },
            k => wgpu::DepthBiasState {
                constant: -(k as i32),
                slope_scale: -(k as f32),
                clamp: 0.0,
            },
        }
    }

    pub fn stencil(self) -> wgpu::StencilState {
        let b = self.0[1];
        if b & 0x40 == 0 {
            return wgpu::StencilState::default();
        }
        let op = |v: u32| match v & 7 {
            1 => wgpu::StencilOperation::Zero,
            2 => wgpu::StencilOperation::Replace,
            3 => wgpu::StencilOperation::IncrementClamp,
            4 => wgpu::StencilOperation::DecrementClamp,
            5 => wgpu::StencilOperation::Invert,
            6 => wgpu::StencilOperation::IncrementWrap,
            7 => wgpu::StencilOperation::DecrementWrap,
            _ => wgpu::StencilOperation::Keep,
        };
        let func = |v: u32| match v & 7 {
            0 => wgpu::CompareFunction::Never,
            1 => wgpu::CompareFunction::Less,
            2 => wgpu::CompareFunction::Equal,
            3 => wgpu::CompareFunction::LessEqual,
            4 => wgpu::CompareFunction::Greater,
            5 => wgpu::CompareFunction::NotEqual,
            6 => wgpu::CompareFunction::GreaterEqual,
            _ => wgpu::CompareFunction::Always,
        };
        let front = wgpu::StencilFaceState {
            compare: func(b >> 17),
            pass_op: op(b >> 8),
            fail_op: op(b >> 11),
            depth_fail_op: op(b >> 14),
        };
        let back = if b & 0x80 != 0 {
            wgpu::StencilFaceState {
                compare: func(b >> 29),
                pass_op: op(b >> 20),
                fail_op: op(b >> 23),
                depth_fail_op: op(b >> 26),
            }
        } else {
            front
        };
        wgpu::StencilState {
            front,
            back,
            read_mask: 0xFF,
            write_mask: 0xFF,
        }
    }
}

fn factor(v: u32) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as B;
    match v {
        1 => B::Zero,
        2 => B::One,
        3 => B::Src,
        4 => B::OneMinusSrc,
        5 => B::SrcAlpha,
        6 => B::OneMinusSrcAlpha,
        7 => B::DstAlpha,
        8 => B::OneMinusDstAlpha,
        9 => B::Dst,
        10 => B::OneMinusDst,
        _ => B::One,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_has_no_blend_and_alpha_op_copies_rgb() {
        assert!(StateBits([0x800 | 0x8000 | 0x1800_0000, 0xD]).blend().is_none());
        // src alpha / inv src alpha, op add, no separate alpha op.
        let b = StateBits([5 | 6 << 4 | 1 << 8, 0]).blend().unwrap();
        assert_eq!(b.alpha.src_factor, wgpu::BlendFactor::SrcAlpha);
        assert_eq!(b.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
    }

    #[test]
    fn depth_and_cull_decode() {
        let s = StateBits([0x8000, 0x1 | 0xC]);
        assert_eq!(s.cull(), Some(wgpu::Face::Back));
        assert!(s.depth_write());
        assert_eq!(s.depth_compare(), wgpu::CompareFunction::LessEqual);
        assert_eq!(StateBits([0, 0x2]).depth_compare(), wgpu::CompareFunction::Always);
    }
}
