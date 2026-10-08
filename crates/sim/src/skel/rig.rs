// SPDX-License-Identifier: GPL-3.0-or-later
//! The server-side skeleton of one entity: bones of one or more models, posed from weighted
//! animations plus the player controllers, in entity space.
//!
//! This follows the original's `DObj`: the bones of every model sit in one array (body first,
//! then attached models), at most [`MAX_BONES`]. A model added without an attach tag is melded:
//! each of its bones whose name already exists in an earlier model becomes a *duplicate* that
//! simply copies that bone's matrix (the stock head model repeats `j_spine4`, `j_neck`,
//! `j_head`, ... this way); its other bones hang off the duplicates through its own
//! `parent_list`. A model added with an attach tag has its root bones parented to that tag's
//! bone instead.
//!
//! Script strings are indices into the zone that loaded the asset, so a model's bone names and
//! an animation's part names are only comparable once resolved to text. The rig therefore
//! takes names as `&str` and animations are bound to it by name ([`Rig::bind`]); both happen
//! at load time, never per pose.
//!
//! Composition per bone, in global bone order (`DObjCalcSkel`):
//!
//! * local rotation: the blended animation's rotation, else the model's rest rotation
//!   (`quats / 32767`; root bones rest at identity);
//! * local translation: the blended animation's translation (zero when the animation has none)
//!   plus, for non-root bones, the model's rest translation (three floats per non-root bone);
//! * a controller bone ([`CONTROLLER_BONES`]) takes its rotation from the controller angles and
//!   composes in the root's frame: `world = root * C * root^-1 * parent`;
//! * `world = parent * local` for rotation, `parent.trans + parent * trans` for translation.
//!
//! The root (bone 0, `tag_origin`) is posed by the controllers' `tag_origin` angles and offset,
//! which is how the original leans, turns the legs and shifts a prone body. The entity's own
//! origin and yaw are not part of the pose; see [`Pose::to_world`].

use std::sync::Arc;

use assets::zone::xmodel::XModel;

use super::anim::{self, Accum, NO_BONE};
use super::quat::{self, Quat};
use crate::Vec3;

/// `DOBJ_MAX_PARTS`.
pub const MAX_BONES: usize = 128;

/// The six bones the engine drives from view angles, in `controller_names` order.
pub const CONTROLLER_BONES: [&str; 6] =
    ["back_low", "back_mid", "back_up", "neck", "head", "pelvis"];

/// One model of a rig.
pub struct RigModel<'a> {
    pub model: Arc<XModel>,
    /// One resolved name per bone of `model`.
    pub bone_names: &'a [&'a str],
    /// Bone of an earlier model to attach the model's roots to; `None` melds by name.
    pub attach: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    /// Root bone; `parent` is the attach bone or [`NO_BONE`].
    Root,
    Child,
    /// Copies another bone's matrix.
    Duplicate(u8),
}

#[derive(Debug, Clone, Copy)]
struct Bone {
    model: u8,
    local: u8,
    parent: u8,
    kind: Kind,
}

#[derive(Debug)]
pub struct Rig {
    models: Vec<Arc<XModel>>,
    bones: Vec<Bone>,
    names: Vec<Box<str>>,
    control: [u8; 6],
}

/// Anim part index to rig bone index; build with [`Rig::bind`].
#[derive(Debug, Clone)]
pub struct AnimBinding(Box<[u8]>);

impl AnimBinding {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Number of anim parts that found a bone.
    pub fn matched(&self) -> usize {
        self.0.iter().filter(|b| **b != NO_BONE).count()
    }
}

/// An animation sampled at `time` (normalised) and weighted into the pose.
#[derive(Clone, Copy)]
pub struct AnimLayer<'a> {
    pub anim: &'a assets::zone::xanim::XAnimParts,
    pub bind: &'a AnimBinding,
    pub time: f32,
    pub weight: f32,
}

/// What the engine's `controller_info_t` holds: Euler angles (pitch, yaw, roll degrees) for
/// each of [`CONTROLLER_BONES`] and the root's angles and offset.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Controllers {
    pub angles: [Vec3; 6],
    pub tag_origin_angles: Vec3,
    pub tag_origin_offset: Vec3,
}

impl Controllers {
    pub const NONE: Controllers = Controllers {
        angles: [[0.0; 3]; 6],
        tag_origin_angles: [0.0; 3],
        tag_origin_offset: [0.0; 3],
    };
}

/// A bone's matrix: unit rotation and translation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoneMat {
    pub quat: Quat,
    pub trans: Vec3,
}

impl BoneMat {
    pub const IDENTITY: BoneMat = BoneMat {
        quat: quat::IDENTITY,
        trans: [0.0; 3],
    };
}

/// Entity-space bone matrices, filled by [`Rig::pose`].
#[derive(Clone)]
pub struct Pose {
    pub len: usize,
    pub bones: [BoneMat; MAX_BONES],
}

impl Default for Pose {
    fn default() -> Self {
        Self {
            len: 0,
            bones: [BoneMat::IDENTITY; MAX_BONES],
        }
    }
}

impl std::fmt::Debug for Pose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pose").field("len", &self.len).finish()
    }
}

impl Pose {
    pub fn bones(&self) -> &[BoneMat] {
        &self.bones[..self.len]
    }

    /// Entity-space point `p` to world space for an entity at `origin` facing `yaw` degrees.
    pub fn to_world(p: &Vec3, origin: &Vec3, yaw: f32) -> Vec3 {
        let (s, c) = crate::pm::math::sincos_deg(yaw);
        [
            origin[0] + p[0] * c - p[1] * s,
            origin[1] + p[0] * s + p[1] * c,
            origin[2] + p[2],
        ]
    }
}

impl Rig {
    /// Builds the rig. Fails when a model has the wrong number of names or the bones exceed
    /// [`MAX_BONES`].
    pub fn new(models: &[RigModel]) -> Result<Rig, String> {
        let mut bones: Vec<Bone> = Vec::new();
        let mut names: Vec<Box<str>> = Vec::new();
        let mut starts: Vec<usize> = Vec::new();
        for (mi, m) in models.iter().enumerate() {
            let n = usize::from(m.model.num_bones);
            if m.bone_names.len() != n {
                return Err(format!(
                    "model {} has {n} bones but {} names",
                    m.model.name.as_deref().unwrap_or("?"),
                    m.bone_names.len()
                ));
            }
            let base = bones.len();
            if base + n > MAX_BONES {
                return Err(format!("rig exceeds {MAX_BONES} bones"));
            }
            let find_before = |name: &str, bones_len: usize, names: &[Box<str>]| {
                (0..bones_len)
                    .rev()
                    .find(|i| names[*i].eq_ignore_ascii_case(name))
            };
            let tag_parent = match m.attach {
                Some(tag) if mi > 0 && !tag.is_empty() => {
                    find_before(tag, base, &names).map_or(NO_BONE, |i| i as u8)
                }
                _ => NO_BONE,
            };
            let meld = mi > 0 && m.attach.is_none_or(str::is_empty);
            let roots = usize::from(m.model.num_root_bones);
            for (local, name) in m.bone_names.iter().enumerate() {
                let global = base + local;
                let dup = meld
                    .then(|| find_before(name, base, &names))
                    .flatten()
                    .map(|i| i as u8);
                let (kind, parent) = if let Some(src) = dup {
                    (Kind::Duplicate(src), NO_BONE)
                } else if local < roots {
                    (Kind::Root, tag_parent)
                } else {
                    let off = usize::from(m.model.parent_list[local - roots]);
                    (Kind::Child, (global - off) as u8)
                };
                bones.push(Bone {
                    model: mi as u8,
                    local: local as u8,
                    parent,
                    kind,
                });
                names.push((*name).into());
            }
            starts.push(base);
        }
        let control = CONTROLLER_BONES.map(|c| {
            names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(c))
                .map_or(NO_BONE, |i| i as u8)
        });
        Ok(Rig {
            models: models.iter().map(|m| m.model.clone()).collect(),
            bones,
            names,
            control,
        })
    }

    pub fn len(&self) -> usize {
        self.bones.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bones.is_empty()
    }

    pub fn bone_name(&self, i: usize) -> &str {
        &self.names[i]
    }

    /// The bone `i` hangs from, if it has one.
    pub fn parent(&self, i: usize) -> Option<usize> {
        let b = self.bones.get(i)?;
        (!matches!(b.kind, Kind::Duplicate(_)) && b.parent != NO_BONE)
            .then_some(usize::from(b.parent))
    }

    /// The bone whose matrix bone `i` copies (a model melded onto another), if it does.
    pub fn duplicate_of(&self, i: usize) -> Option<usize> {
        match self.bones.get(i)?.kind {
            Kind::Duplicate(src) => Some(usize::from(src)),
            _ => None,
        }
    }

    /// Index of the first bone with this name (case-insensitive).
    pub fn bone_index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }

    /// Index of the controller bone `which` of [`CONTROLLER_BONES`], if the rig has it.
    pub fn controller_bone(&self, which: usize) -> Option<usize> {
        let b = self.control[which];
        (b != NO_BONE).then_some(usize::from(b))
    }

    pub fn model(&self, i: usize) -> &Arc<XModel> {
        &self.models[i]
    }

    /// Model index, model-local bone index of a rig bone.
    pub(super) fn bone_model(&self, bone: usize) -> (&XModel, usize) {
        let b = &self.bones[bone];
        (&self.models[usize::from(b.model)], usize::from(b.local))
    }

    pub(super) fn bone_parent(&self, bone: usize) -> Option<usize> {
        let b = &self.bones[bone];
        match b.kind {
            Kind::Duplicate(_) => None,
            _ => (b.parent != NO_BONE).then_some(usize::from(b.parent)),
        }
    }

    pub(super) fn bone_duplicate_of(&self, bone: usize) -> Option<usize> {
        match self.bones[bone].kind {
            Kind::Duplicate(s) => Some(usize::from(s)),
            _ => None,
        }
    }

    /// Binds an animation's part names to this rig's bones (names compare case-insensitively;
    /// parts the rig does not have map to [`NO_BONE`]).
    pub fn bind<S: AsRef<str>>(&self, part_names: &[S]) -> AnimBinding {
        AnimBinding(
            part_names
                .iter()
                .map(|p| self.bone_index(p.as_ref()).map_or(NO_BONE, |i| i as u8))
                .collect(),
        )
    }

    /// Poses the rig into `out`: blends `layers`, applies `ctl`, composes down the hierarchy.
    /// Bones no layer drives stay at the model's rest pose. Allocation-free.
    pub fn pose(&self, layers: &[AnimLayer], ctl: &Controllers, out: &mut Pose) {
        let n = self.bones.len();
        let mut acc = [Accum::ZERO; MAX_BONES];
        for l in layers {
            if l.weight > 0.0 {
                anim::accumulate(l.anim, l.bind.as_slice(), l.time, l.weight, &mut acc[..n]);
            }
        }
        out.len = n;
        let root_q = quat::normalize(&quat::from_angles(&ctl.tag_origin_angles));
        for (i, (&b, acc_i)) in self.bones.iter().zip(&acc).enumerate() {
            let mat = match b.kind {
                Kind::Duplicate(src) => out.bones[usize::from(src)],
                Kind::Root => {
                    let (q, t) = acc_i.finish();
                    let mut lq = q.unwrap_or(quat::IDENTITY);
                    let mut lt = t.unwrap_or([0.0; 3]);
                    if i == 0 {
                        lq = root_q;
                        lt = ctl.tag_origin_offset;
                    }
                    if b.parent == NO_BONE {
                        BoneMat {
                            quat: lq,
                            trans: lt,
                        }
                    } else {
                        let p = out.bones[usize::from(b.parent)];
                        compose(&p, &lq, &lt)
                    }
                }
                Kind::Child => {
                    let model = &self.models[usize::from(b.model)];
                    let local = usize::from(b.local) - usize::from(model.num_root_bones);
                    let (aq, at) = acc_i.finish();
                    let control = self.control.iter().position(|c| usize::from(*c) == i);
                    let lq = if let Some(c) = control {
                        quat::normalize(&quat::from_angles(&ctl.angles[c]))
                    } else {
                        aq.unwrap_or_else(|| {
                            quat::normalize(&model.quats[local].map(|c| f32::from(c) / 32767.0))
                        })
                    };
                    let rest = [
                        model.trans[3 * local],
                        model.trans[3 * local + 1],
                        model.trans[3 * local + 2],
                    ];
                    let at = if control.is_some() {
                        [0.0; 3]
                    } else {
                        at.unwrap_or([0.0; 3])
                    };
                    let lt = [at[0] + rest[0], at[1] + rest[1], at[2] + rest[2]];
                    let p = out.bones[usize::from(b.parent)];
                    if control.is_some() {
                        let root = out.bones[0].quat;
                        let q = quat::mul(
                            &quat::mul(&root, &lq),
                            &quat::mul(&quat::conj(&root), &p.quat),
                        );
                        let t = quat::rotate(&p.quat, &lt);
                        BoneMat {
                            quat: q,
                            trans: [p.trans[0] + t[0], p.trans[1] + t[1], p.trans[2] + t[2]],
                        }
                    } else {
                        compose(&p, &lq, &lt)
                    }
                }
            };
            out.bones[i] = mat;
        }
    }
}

#[inline]
fn compose(parent: &BoneMat, lq: &Quat, lt: &Vec3) -> BoneMat {
    let t = quat::rotate(&parent.quat, lt);
    BoneMat {
        quat: quat::normalize(&quat::mul(&parent.quat, lq)),
        trans: [
            parent.trans[0] + t[0],
            parent.trans[1] + t[1],
            parent.trans[2] + t[2],
        ],
    }
}
