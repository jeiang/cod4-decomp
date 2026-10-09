// SPDX-License-Identifier: GPL-3.0-only
//! Player models: loading the body and attachments the scripts gave a player (or the stock ones of a team), and
//! posing a player each frame.
//!
//! The server's [`PlayerAnims`] (rig, `pb_*` animation selection, cross-fades) is the pose source for remote
//! players, so what a client draws is the skeleton the server shoots at. The content is the server's [`Content`] kept
//! with its render payload ([`Content::for_client`]).

use assets::zone::xmodel::XModel;
use server::content::{Content, Install};
use server::playeranim::{PlayerAnims, PlayerPoseInput, PlayerPoseState};
use sim::skel::{Controllers, Pose, Rig, RigModel};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::ragdoll::{Def, Ragdoll, Skel};
use render::{ModelInstance, ModelKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Team {
    Allies,
    Axis,
}

impl Team {
    /// Name fragments of the stock models of the team's factions.
    fn factions(self) -> &'static [&'static str] {
        match self {
            Team::Allies => &["usmc", "sas"],
            Team::Axis => &["arab", "russian"],
        }
    }
}

/// A model hanging on another: its name and the tag of the models before it that it hangs from (empty for the origin of
/// the body, which melds a head onto it by bone name).
pub type Attachment = (String, String);

/// Which models make up a player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerModelSet {
    pub body: String,
    /// What the scripts hung on the body, in rig order.
    pub attach: Vec<Attachment>,
    /// What is in the hands: the weapon and the knife, each with the body tag it follows. These are not part of the rig
    /// (a hand flip or a knife swing changes them every few frames), see [`Player::set_hand`].
    pub hand: Vec<Attachment>,
}

impl PlayerModelSet {
    /// This set with `weapon` (a model and the tag it hangs from) and the knife (`tag_inhand`) in the hands.
    pub fn armed(mut self, weapon: Option<Attachment>, knife: Option<String>) -> Self {
        self.hand = weapon
            .into_iter()
            .chain(knife.map(|k| (k, "tag_inhand".to_owned())))
            .collect();
        self
    }
}

/// A model in the hands: it follows the body bone `tag`, with its own bones at rest.
#[derive(Clone)]
pub struct Held {
    model: Arc<XModel>,
    rest: Arc<RestPose>,
    /// The bone names of `model`.
    names: server::content::BoneNames,
    tag: String,
}

impl Held {
    /// The bones of the model when its origin sits on the body bone `hand`.
    fn bones(&self, hand: &sim::skel::BoneMat) -> Vec<sim::skel::BoneMat> {
        self.rest.bones[0]
            .iter()
            .map(|b| {
                let r = sim::skel::quat::rotate(&hand.quat, &b.trans);
                sim::skel::BoneMat {
                    quat: sim::skel::quat::mul(&hand.quat, &b.quat),
                    trans: [
                        hand.trans[0] + r[0],
                        hand.trans[1] + r[1],
                        hand.trans[2] + r[2],
                    ],
                }
            })
            .collect()
    }

    fn bone_index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(name))
    }
}

/// A model with models attached, in the rest pose of their bones: what a script model that carries attachments is
/// drawn from.
pub struct RestPose {
    models: Vec<Arc<XModel>>,
    bones: Vec<Vec<sim::skel::BoneMat>>,
    rig: Arc<Rig>,
}

/// The bits of `bits` (bone `n` is the `n`-th most significant over four words) that belong to the `count` bones from
/// bone `base` on, renumbered from 0: what one model of an entity hides when the entity hides `bits` of its bones.
pub fn part_bits_of(bits: [u32; 4], base: usize, count: usize) -> [u32; 4] {
    let mut out = [0; 4];
    for j in 0..count.min(128) {
        let g = base + j;
        if g < 128 && bits[g >> 5] & (0x8000_0000 >> (g & 31)) != 0 {
            out[j >> 5] |= 0x8000_0000 >> (j & 31);
        }
    }
    out
}

impl RestPose {
    /// The rig of the models, for placing a turret's gunner.
    pub fn rig(&self) -> &Arc<Rig> {
        &self.rig
    }

    /// The models as instances for an entity at `origin` facing `angles`, hiding the bones of `hidden`.
    pub fn instances(
        &self,
        origin: [f32; 3],
        angles: [f32; 3],
        hidden: [u32; 4],
    ) -> Vec<ModelInstance> {
        self.instances_of(&self.bones, origin, angles, hidden)
    }

    /// [`RestPose::instances`] with a turret's gun swung by `gun` (`gunAngles`): its `tag_aim`, `tag_aim_animated` and
    /// `tag_flash` bones turn and what hangs from them follows.
    pub fn swung(
        &self,
        gun: [f32; 3],
        origin: [f32; 3],
        angles: [f32; 3],
        hidden: [u32; 4],
    ) -> Vec<ModelInstance> {
        let mut pose = Pose::default();
        let ctl = Controllers {
            turret: Some(gun),
            ..Controllers::NONE
        };
        self.rig.pose(&[], &ctl, &mut pose);
        let mut at = 0;
        let bones: Vec<Vec<sim::skel::BoneMat>> = self
            .bones
            .iter()
            .map(|rest| {
                let b = pose.bones().get(at..at + rest.len()).unwrap_or(rest);
                at += rest.len();
                b.to_vec()
            })
            .collect();
        self.instances_of(&bones, origin, angles, hidden)
    }

    fn instances_of(
        &self,
        bones: &[Vec<sim::skel::BoneMat>],
        origin: [f32; 3],
        angles: [f32; 3],
        hidden: [u32; 4],
    ) -> Vec<ModelInstance> {
        let mut base = 0;
        self.models
            .iter()
            .zip(bones)
            .map(|(model, bones)| {
                let mut m = ModelInstance::new(model.clone(), ModelKind::World);
                m.origin = origin;
                m.angles = angles;
                m.light_origin = origin;
                m.bones.clone_from(bones);
                m.hidden_parts = part_bits_of(hidden, base, bones.len());
                base += bones.len();
                m
            })
            .collect()
    }
}

/// The loaded content and the animation sets built from it.
pub struct Library {
    pub content: Content,
    /// The install's `ragdoll.cfg`: empty when it has none, which leaves bodies in the pose they died in.
    pub ragdoll: Def,
    anims: HashMap<(String, Vec<Attachment>), Arc<PlayerAnims>>,
    rests: HashMap<(String, Vec<Attachment>), Arc<RestPose>>,
    /// [`Library::team_models`] by team: the loaded models never change, and the net frame asks for every player.
    teams: [std::sync::OnceLock<Option<PlayerModelSet>>; 2],
}

impl Library {
    /// Decodes `common_mp` and `map` of the install at `root`.
    pub fn load(root: &Path, map: &str) -> Result<Library, String> {
        let install = Install::open(root).map_err(|e| format!("cannot open the install: {e}"))?;
        let mut content = Content::for_client();
        content.load_zone(&install, "common_mp", 4)?;
        content.load_map(&install, map)?;
        let ragdoll = match install.vfs.read("ragdoll.cfg") {
            Ok(Some(bytes)) => Def::parse(&String::from_utf8_lossy(&bytes)),
            _ => Def::default(),
        };
        Ok(Library {
            content,
            ragdoll,
            anims: HashMap::new(),
            rests: HashMap::new(),
            teams: Default::default(),
        })
    }

    /// The first stock body, head and view hands of `team` the loaded zones have, in name order.
    pub fn team_models(&self, team: Team) -> Option<PlayerModelSet> {
        self.teams[team as usize]
            .get_or_init(|| self.find_team_models(team))
            .clone()
    }

    /// The models of a player the scripts dressed: the body they chose (`setmodel`) with what they attached, the head
    /// among it. Attachments the loaded zones lack are left out. `None` when the body is not in the loaded zones.
    pub fn scripted_models(&self, body: &str, attached: &[Attachment]) -> Option<PlayerModelSet> {
        self.content.model(body)?;
        Some(PlayerModelSet {
            body: body.to_owned(),
            attach: attached
                .iter()
                .filter(|(m, _)| self.content.model(m).is_some())
                .cloned()
                .collect(),
            hand: Vec::new(),
        })
    }

    fn find_team_models(&self, team: Team) -> Option<PlayerModelSet> {
        let pick = |prefix: &str, words: &[&str]| -> Option<String> {
            let names = self.content.model_names(prefix);
            words
                .iter()
                .find_map(|w| names.iter().find(|n| n[prefix.len()..].contains(w)))
                .map(|n| (*n).to_owned())
        };
        let body = pick("body_mp_", team.factions())?;
        let faction = team
            .factions()
            .iter()
            .find(|f| body.contains(*f))
            .copied()?;
        Some(PlayerModelSet {
            attach: pick("head_mp_", &[faction])
                .map(|h| (h, String::new()))
                .into_iter()
                .collect(),
            hand: Vec::new(),
            body,
        })
    }

    /// A posable player for `set`.
    pub fn player(&mut self, set: &PlayerModelSet) -> Result<Player, String> {
        let key = (set.body.clone(), set.attach.clone());
        let anims = match self.anims.get(&key) {
            Some(a) => a.clone(),
            None => {
                let attach: Vec<(&str, &str)> = set
                    .attach
                    .iter()
                    .map(|(m, t)| (m.as_str(), t.as_str()))
                    .collect();
                let a = Arc::new(PlayerAnims::with_attachments(
                    &self.content,
                    &set.body,
                    &attach,
                )?);
                self.anims.insert(key, a.clone());
                a
            }
        };
        let model = |n: &str| -> Result<Arc<XModel>, String> {
            self.content
                .model(n)
                .cloned()
                .ok_or_else(|| format!("model {n} not loaded"))
        };
        let models = std::iter::once(set.body.as_str())
            .chain(set.attach.iter().map(|(m, _)| m.as_str()))
            .map(model)
            .collect::<Result<_, _>>()?;
        let mut player = Player {
            models,
            hand: Vec::new(),
            hidden: [0; 4],
            anims,
            state: PlayerPoseState::default(),
            pose: Pose::default(),
            yaw: 0.0,
        };
        player.set_hand(self.hand(&set.hand)?);
        Ok(player)
    }

    /// What a player holds, from `(model, tag)` pairs.
    pub fn hand(&mut self, hand: &[Attachment]) -> Result<Vec<Held>, String> {
        hand.iter()
            .map(|(m, tag)| {
                let model = self
                    .content
                    .model(m)
                    .cloned()
                    .ok_or_else(|| format!("model {m} not loaded"))?;
                Ok(Held {
                    model,
                    rest: self.rest_pose(m, &[])?,
                    names: self
                        .content
                        .model_bone_names(m)
                        .cloned()
                        .ok_or_else(|| format!("model {m} has no bone names"))?,
                    tag: tag.clone(),
                })
            })
            .collect()
    }

    /// `body` with `attach` hanging on it, at rest: for an entity the scripts gave attachments that does not animate.
    pub fn rest_pose(
        &mut self,
        body: &str,
        attach: &[Attachment],
    ) -> Result<Arc<RestPose>, String> {
        let key = (body.to_owned(), attach.to_vec());
        if let Some(r) = self.rests.get(&key) {
            return Ok(r.clone());
        }
        let names = |m: &str| {
            self.content
                .model_bone_names(m)
                .ok_or_else(|| format!("model {m} has no bone names"))
        };
        let models: Vec<Arc<XModel>> = std::iter::once(body)
            .chain(attach.iter().map(|(m, _)| m.as_str()))
            .map(|m| {
                self.content
                    .model(m)
                    .cloned()
                    .ok_or_else(|| format!("model {m} not loaded"))
            })
            .collect::<Result<_, _>>()?;
        let texts: Vec<Vec<&str>> = std::iter::once(body)
            .chain(attach.iter().map(|(m, _)| m.as_str()))
            .map(|m| names(m).map(|n| n.iter().map(|s| &**s).collect()))
            .collect::<Result<_, _>>()?;
        let specs: Vec<RigModel> = models
            .iter()
            .zip(&texts)
            .enumerate()
            .map(|(i, (m, t))| RigModel {
                model: m.clone(),
                bone_names: t,
                attach: i
                    .checked_sub(1)
                    .map(|a| attach[a].1.as_str())
                    .filter(|tag| !tag.is_empty()),
            })
            .collect();
        let rig = Rig::new(&specs)?;
        let mut pose = Pose::default();
        rig.pose(&[], &Controllers::NONE, &mut pose);
        let mut at = 0;
        let bones = models
            .iter()
            .map(|m| {
                let n = usize::from(m.num_bones);
                let b = pose.bones().get(at..at + n).unwrap_or(&[]).to_vec();
                at += n;
                b
            })
            .collect();
        let r = Arc::new(RestPose {
            models,
            bones,
            rig: Arc::new(rig),
        });
        self.rests.insert(key, r.clone());
        Ok(r)
    }
}

/// One player's animation state and models.
pub struct Player {
    anims: Arc<PlayerAnims>,
    state: PlayerPoseState,
    pose: Pose,
    /// The body, then what hangs on it.
    models: Vec<Arc<XModel>>,
    /// The weapon and the knife in the hands, drawn after `models`.
    hand: Vec<Held>,
    /// The hidden bones (`hidepart`), over the bones of all the models.
    hidden: [u32; 4],
    yaw: f32,
}

impl Player {
    /// Advances the animation by `dt` seconds for `input` and poses the skeleton.
    pub fn update(&mut self, dt: f32, input: &PlayerPoseInput) {
        self.state.update(&*self.anims, dt, input);
        self.state.pose(&self.anims, &mut self.pose);
        self.yaw = input.yaw;
    }

    /// Changes what is in the hands in place: the rig and the animation state stay as they are.
    pub fn set_hand(&mut self, hand: Vec<Held>) {
        self.hand = hand;
    }

    /// Hides the bones of `bits` (`hidepart`).
    pub fn hide_parts(&mut self, bits: [u32; 4]) {
        self.hidden = bits;
    }

    /// The animation currently playing.
    pub fn animation(&self) -> Option<&'static str> {
        self.state.current()
    }

    /// The upper-body clip (fire, reload, melee, throw, pullout, flinch) playing over the legs, if any.
    pub fn torso_animation(&self) -> Option<&'static str> {
        self.state.torso(&self.anims)
    }

    /// The knife swings (`BG_IsKnifeMeleeAnim`): the upper-body clip is a melee one.
    pub fn is_knifing(&self) -> bool {
        self.torso_animation()
            .is_some_and(|c| c.starts_with("pt_melee_"))
    }

    /// The gun is in the left hand: the last `anim_gunhand` notetrack the torso clip has passed says `left`
    /// (`CG_ProcessClientNoteTracks`; with no clip playing the gun is back in the right hand).
    pub fn gun_in_left_hand(&self, content: &Content) -> bool {
        self.state.torso_seconds().is_some_and(|(clip, seconds)| {
            content
                .anim(clip)
                .is_some_and(|a| a.length > 0.0 && a.gun_hand_left(seconds / a.length))
        })
    }

    /// Where the bone `tag` is for a player standing at `origin` in the last [`Player::update`]'s pose, and the way
    /// its x axis points.
    pub fn tag_frame(&self, tag: &str, origin: [f32; 3]) -> Option<([f32; 3], [f32; 3])> {
        let i = self.anims.rig().bone_index(tag)?;
        let bone = self.pose.bones().get(i)?;
        let forward = sim::skel::quat::axes(&bone.quat)[0];
        Some((
            Pose::to_world(&bone.trans, &origin, self.yaw),
            Pose::to_world(&forward, &[0.0; 3], self.yaw),
        ))
    }

    /// A ragdoll of the body as the last [`Player::update`] posed it, thrown with velocity `push`. `None` when the
    /// definition names a bone the skeleton lacks.
    pub fn ragdoll(&self, def: &Def, origin: [f32; 3], push: [f32; 3]) -> Option<Ragdoll> {
        let rig = self.anims.rig();
        let skel = Skel {
            names: (0..rig.len())
                .map(|i| rig.bone_name(i).to_owned())
                .collect(),
            parent: (0..rig.len()).map(|i| rig.parent(i)).collect(),
            alias: (0..rig.len()).map(|i| rig.duplicate_of(i)).collect(),
        };
        let mut bind = Pose::default();
        rig.pose(&[], &Controllers::NONE, &mut bind);
        Ragdoll::new(
            def,
            &skel,
            self.pose.bones(),
            bind.bones(),
            origin,
            self.yaw,
            push,
        )
    }

    /// Where the `j_head` bone is for a player standing at `origin` in the last [`Player::update`]'s pose.
    pub fn head_pos(&self, origin: [f32; 3]) -> Option<[f32; 3]> {
        let i = self.anims.rig().bone_index("j_head")?;
        let head = self.pose.bones().get(i)?.trans;
        Some(sim::skel::Pose::to_world(&head, &origin, self.yaw))
    }

    /// Where `tag` of the weapon in the player's hands is in the world, for a player standing at `origin` in the pose
    /// of the last [`Player::update`]: the muzzle (`tag_flash`), the ejection port (`tag_brass`).
    pub fn weapon_tag(&self, origin: [f32; 3], tag: &str) -> Option<fx::Frame> {
        let held = self.hand.first()?;
        let hand = self
            .pose
            .bones()
            .get(self.anims.rig().bone_index(&held.tag)?)?;
        let b = held.bones(hand).into_iter().nth(held.bone_index(tag)?)?;
        let b = &b;
        let body = glam::Mat4::from_rotation_translation(
            glam::Quat::from_array(sim::skel::quat::from_angles(&[0.0, self.yaw, 0.0])),
            glam::Vec3::from(origin),
        );
        let m = body
            * glam::Mat4::from_rotation_translation(
                glam::Quat::from_xyzw(b.quat[0], b.quat[1], b.quat[2], b.quat[3]).normalize(),
                glam::Vec3::from(b.trans),
            );
        Some(fx::Frame {
            origin: m.w_axis.truncate(),
            axis: [
                m.x_axis.truncate(),
                m.y_axis.truncate(),
                m.z_axis.truncate(),
            ],
        })
    }

    /// The models as instances for a player standing at `origin`.
    pub fn instances(&self, origin: [f32; 3]) -> Vec<ModelInstance> {
        self.instances_posed(origin, self.yaw, self.pose.bones())
    }

    /// The models in the pose `bones`, for an entity at `origin` facing `yaw` degrees.
    pub fn instances_posed(
        &self,
        origin: [f32; 3],
        yaw: f32,
        bones: &[sim::skel::BoneMat],
    ) -> Vec<ModelInstance> {
        let mut at = 0;
        let mut out: Vec<ModelInstance> = self
            .models
            .iter()
            .map(|model| {
                let n = usize::from(model.num_bones);
                let mut m = ModelInstance::new(model.clone(), ModelKind::World);
                m.casts_cookie = true;
                m.origin = origin;
                m.angles = [0.0, yaw, 0.0];
                m.bones = bones.get(at..at + n).unwrap_or(&[]).to_vec();
                m.hidden_parts = part_bits_of(self.hidden, at, n);
                m.light_origin = [origin[0], origin[1], origin[2] + 36.0];
                at += n;
                m
            })
            .collect();
        // What is held follows its tag bone: its own rest bones carried by that bone's matrix.
        let rig = self.anims.rig();
        for h in &self.hand {
            let Some(tag) = rig.bone_index(&h.tag).and_then(|i| bones.get(i)) else {
                continue;
            };
            let mut m = ModelInstance::new(h.model.clone(), ModelKind::World);
            m.casts_cookie = true;
            m.origin = origin;
            m.angles = [0.0, yaw, 0.0];
            m.bones = h.bones(tag);
            m.light_origin = [origin[0], origin[1], origin[2] + 36.0];
            out.push(m);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A player holding a weapon draws its world model, in the hands: the gun is the third instance and its first
    /// bone sits at hand height beside the body, not at the feet.
    #[test]
    fn an_armed_player_draws_the_weapon_at_the_hand() {
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut lib = Library::load(std::path::Path::new(&root), "mp_backlot").expect("content");
        let held = lib
            .content
            .weapon("m4_mp")
            .and_then(|w| w.world_models.first().cloned().flatten())
            .and_then(|m| m.name.as_deref().map(str::to_owned))
            .expect("m4 world model");
        let set = lib
            .team_models(Team::Allies)
            .expect("allied models")
            .armed(Some((held.clone(), "tag_weapon_right".to_owned())), None);
        let mut p = lib.player(&set).expect("player");
        p.update(0.0, &PlayerPoseInput::default());
        let models = p.instances([0.0; 3]);
        assert_eq!(models.len(), 3);
        let gun = &models[2];
        assert_eq!(gun.model.name.as_deref(), Some(held.as_str()));
        let at = gun.bones[0].trans;
        assert!(
            (20.0..70.0).contains(&at[2]) && at[0].hypot(at[1]) < 40.0,
            "the gun is at {at:?}"
        );
    }

    /// A stock body, its head and a many-bone weapon (all three together pass the original's 128 bones) make a rig, and
    /// changing what is held leaves that rig and its animation clock alone.
    #[test]
    fn a_heavy_weapon_fits_and_changing_hands_keeps_the_rig() {
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; untested");
            return;
        };
        let mut lib = Library::load(std::path::Path::new(&root), "mp_backlot").expect("content");
        let set = lib
            .scripted_models(
                "body_mp_arab_regular_assault",
                &[("head_mp_arab_regular_asad".to_owned(), String::new())],
            )
            .expect("arab body");
        let mut p = lib
            .player(&set.clone().armed(
                Some((
                    "weapon_saw_mg_setup".to_owned(),
                    "tag_weapon_right".to_owned(),
                )),
                None,
            ))
            .expect("rig with the gun");
        p.update(0.1, &PlayerPoseInput::default());
        assert_eq!(p.instances([0.0; 3]).len(), 3);
        let before = Arc::as_ptr(&p.anims);
        let hand = lib
            .hand(&[(
                "weapon_saw_mg_setup".to_owned(),
                "tag_weapon_left".to_owned(),
            )])
            .expect("left hand");
        p.set_hand(hand);
        assert_eq!(Arc::as_ptr(&p.anims), before, "the rig is not rebuilt");
        assert_eq!(p.instances([0.0; 3]).len(), 3);
    }

    /// The stock skeleton and `ragdoll.cfg` make a body that falls on a floor and settles with the bones the
    /// definition drives still the length they were, the head above the floor and every skeleton bone accounted for.
    #[test]
    fn a_stock_body_falls_as_a_ragdoll_and_keeps_its_bones_together() {
        use sim::cm::{Collide, Trace};
        struct Floor;
        impl Collide for Floor {
            fn trace(
                &self,
                a: [f32; 3],
                b: [f32; 3],
                mins: [f32; 3],
                _: [f32; 3],
                _: u16,
                _: i32,
            ) -> Trace {
                let (a, b) = (a[2] + mins[2], b[2] + mins[2]);
                let mut t = Trace::MISS;
                if b < 0.0 && a >= 0.0 {
                    t.fraction = a / (a - b);
                    t.normal = [0.0, 0.0, 1.0];
                }
                t
            }
            fn point_contents(&self, _: [f32; 3], _: u16, _: i32) -> i32 {
                0
            }
        }
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; untested");
            return;
        };
        let mut lib = Library::load(std::path::Path::new(&root), "mp_backlot").expect("content");
        assert!(!lib.ragdoll.is_empty(), "the install has a ragdoll.cfg");
        let set = lib.team_models(Team::Allies).expect("allied models");
        let mut p = lib.player(&set).expect("player");
        p.update(0.0, &PlayerPoseInput::default());
        let rig = p.anims.rig();
        let (root_bone, neck, head) = (
            rig.bone_index("j_mainroot").expect("root"),
            rig.bone_index("j_neck").expect("neck"),
            rig.bone_index("j_head").expect("head"),
        );
        let mut r = p
            .ragdoll(&lib.ragdoll, [0.0; 3], [120.0, 0.0, 0.0])
            .expect("a ragdoll");
        let len = |b: &[sim::skel::BoneMat]| {
            glam::Vec3::from(b[root_bone].trans).distance(glam::Vec3::from(b[neck].trans))
        };
        let before = len(&r.bones());
        for _ in 0..900 {
            r.update(1.0 / 60.0, &Floor);
        }
        assert!(r.at_rest());
        let after = r.bones();
        assert!(
            (len(&after) - before).abs() < 0.5,
            "{before} -> {}",
            len(&after)
        );
        assert!(after.iter().all(|b| b.trans.iter().all(|v| v.is_finite())));
        let h = after[head].trans;
        assert!((0.0..60.0).contains(&h[2]), "the head lies at {h:?}");
    }
}
