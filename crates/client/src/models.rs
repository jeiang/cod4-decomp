// SPDX-License-Identifier: GPL-3.0-or-later
//! Player models: loading the stock body, head and view hands for a team, and posing a player each frame.
//!
//! The server's [`PlayerAnims`] (rig, `pb_*` animation selection, cross-fades) is the pose source for remote
//! players, so what a client draws is the skeleton the server shoots at. The content is the server's [`Content`] kept
//! with its render payload ([`Content::for_client`]).

use assets::zone::xmodel::XModel;
use server::content::{Content, Install};
use server::playeranim::{PlayerAnims, PlayerPoseInput, PlayerPoseState};
use sim::skel::Pose;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::ragdoll::Ragdoll;
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

/// Which models make up a player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerModelSet {
    pub body: String,
    pub head: Option<String>,
    /// The world model of the held weapon, attached at `tag_weapon_right`.
    pub weapon: Option<String>,
}

/// The loaded content and the animation sets built from it.
pub struct Library {
    pub content: Content,
    anims: HashMap<(String, Option<String>, Option<String>), Arc<PlayerAnims>>,
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
        Ok(Library {
            content,
            anims: HashMap::new(),
            teams: Default::default(),
        })
    }

    /// The first stock body, head and view hands of `team` the loaded zones have, in name order.
    pub fn team_models(&self, team: Team) -> Option<PlayerModelSet> {
        self.teams[team as usize]
            .get_or_init(|| self.find_team_models(team))
            .clone()
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
            head: pick("head_mp_", &[faction]),
            weapon: None,
            body,
        })
    }

    /// A posable player for `set`.
    pub fn player(&mut self, set: &PlayerModelSet) -> Result<Player, String> {
        let key = (set.body.clone(), set.head.clone(), set.weapon.clone());
        let anims = match self.anims.get(&key) {
            Some(a) => a.clone(),
            None => {
                let a = Arc::new(PlayerAnims::with_weapon(
                    &self.content,
                    &set.body,
                    set.head.as_deref(),
                    set.weapon.as_deref(),
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
        Ok(Player {
            body: model(&set.body)?,
            head: set.head.as_deref().map(model).transpose()?,
            weapon: set.weapon.as_deref().map(model).transpose()?,
            anims,
            state: PlayerPoseState::default(),
            pose: Pose::default(),
            yaw: 0.0,
        })
    }
}

/// One player's animation state and models.
pub struct Player {
    anims: Arc<PlayerAnims>,
    state: PlayerPoseState,
    pose: Pose,
    body: Arc<XModel>,
    head: Option<Arc<XModel>>,
    weapon: Option<Arc<XModel>>,
    yaw: f32,
}

impl Player {
    /// Advances the animation by `dt` seconds for `input` and poses the skeleton.
    pub fn update(&mut self, dt: f32, input: &PlayerPoseInput) {
        self.state.update(dt, input);
        self.state.pose(&self.anims, &mut self.pose);
        self.yaw = input.yaw;
    }

    /// Swaps in the models and skeleton of `other` (the same body holding another weapon), keeping the animation state.
    pub fn rearm(&mut self, other: Player) {
        self.anims = other.anims;
        self.weapon = other.weapon;
    }

    /// The yaw of the last update, degrees.
    pub fn yaw(&self) -> f32 {
        self.yaw
    }

    /// The animation currently playing.
    pub fn animation(&self) -> Option<&'static str> {
        self.state.current()
    }

    /// A ragdoll of the body as the last [`Player::update`] posed it, thrown with velocity `push`.
    pub fn ragdoll(&self, origin: [f32; 3], push: [f32; 3]) -> Ragdoll {
        let rig = self.anims.rig();
        Ragdoll::new(
            self.pose.bones(),
            |i| rig.parent(i),
            |i| rig.duplicate_of(i),
            origin,
            self.yaw,
            push,
        )
    }

    /// The body and head as model instances for a player standing at `origin`.
    pub fn instances(&self, origin: [f32; 3]) -> Vec<ModelInstance> {
        self.instances_posed(origin, self.yaw, self.pose.bones())
    }

    /// The body and head in the pose `bones`, for an entity at `origin` facing `yaw` degrees.
    pub fn instances_posed(
        &self,
        origin: [f32; 3],
        yaw: f32,
        bones: &[sim::skel::BoneMat],
    ) -> Vec<ModelInstance> {
        let nb = usize::from(self.body.num_bones);
        let mut out = Vec::with_capacity(2);
        let mut push = |model: &Arc<XModel>, bones: &[sim::skel::BoneMat]| {
            let mut m = ModelInstance::new(model.clone(), ModelKind::World);
            m.origin = origin;
            m.angles = [0.0, yaw, 0.0];
            m.bones = bones.to_vec();
            m.light_origin = [origin[0], origin[1], origin[2] + 36.0];
            out.push(m);
        };
        push(&self.body, &bones[..nb.min(bones.len())]);
        if let Some(h) = &self.head {
            let nh = usize::from(h.num_bones);
            push(h, bones.get(nb..nb + nh).unwrap_or(&[]));
            if let Some(w) = &self.weapon {
                push(w, bones.get(nb + nh..).unwrap_or(&[]));
            }
        } else if let Some(w) = &self.weapon {
            push(w, bones.get(nb..).unwrap_or(&[]));
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
        let set = PlayerModelSet {
            weapon: Some(held.clone()),
            ..lib.team_models(Team::Allies).expect("allied models")
        };
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
}
