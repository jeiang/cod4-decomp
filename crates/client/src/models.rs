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

    fn hands(self) -> &'static [&'static str] {
        match self {
            Team::Allies => &["usmc", "sas"],
            Team::Axis => &["opfor", "militia", "arab", "russian"],
        }
    }
}

/// Which models make up a player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerModelSet {
    pub body: String,
    pub head: Option<String>,
    pub viewhands: Option<String>,
}

/// The loaded content and the animation sets built from it.
pub struct Library {
    pub content: Content,
    anims: HashMap<(String, Option<String>), Arc<PlayerAnims>>,
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
        })
    }

    /// The first stock body, head and view hands of `team` the loaded zones have, in name order.
    pub fn team_models(&self, team: Team) -> Option<PlayerModelSet> {
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
            viewhands: pick("viewhands_", team.hands()),
            body,
        })
    }

    /// A posable player for `set`.
    pub fn player(&mut self, set: &PlayerModelSet) -> Result<Player, String> {
        let key = (set.body.clone(), set.head.clone());
        let anims = match self.anims.get(&key) {
            Some(a) => a.clone(),
            None => {
                let a = Arc::new(PlayerAnims::new(
                    &self.content,
                    &set.body,
                    set.head.as_deref(),
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
    yaw: f32,
}

impl Player {
    /// Advances the animation by `dt` seconds for `input` and poses the skeleton.
    pub fn update(&mut self, dt: f32, input: &PlayerPoseInput) {
        self.state.update(dt, input);
        self.state.pose(&self.anims, &mut self.pose);
        self.yaw = input.yaw;
    }

    /// The animation currently playing.
    pub fn animation(&self) -> Option<&'static str> {
        self.state.current()
    }

    /// The body and head as model instances for a player standing at `origin`.
    pub fn instances(&self, origin: [f32; 3]) -> Vec<ModelInstance> {
        let bones = self.pose.bones();
        let nb = usize::from(self.body.num_bones);
        let mut out = Vec::with_capacity(2);
        let mut push = |model: &Arc<XModel>, bones: &[sim::skel::BoneMat]| {
            let mut m = ModelInstance::new(model.clone(), ModelKind::World);
            m.origin = origin;
            m.angles = [0.0, self.yaw, 0.0];
            m.bones = bones.to_vec();
            m.light_origin = [origin[0], origin[1], origin[2] + 36.0];
            out.push(m);
        };
        push(&self.body, &bones[..nb.min(bones.len())]);
        if let Some(h) = &self.head {
            push(h, bones.get(nb..).unwrap_or(&[]));
        }
        out
    }
}
