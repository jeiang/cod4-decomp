// SPDX-License-Identifier: GPL-3.0-only
// Translated in part from KisakCOD (cgame_mp/cg_players_mp.cpp CG_UpdateWeaponVisibility and CG_IsWeaponVisible, cgame_mp/cg_main_mp.cpp CG_AttachWeapon and CG_GetWeaponAttachBone, bgame/bg_animation_mp.cpp BG_UpdatePlayerDObj; GPL-3.0, copyright the KisakCOD contributors and Activision).
//! What hangs on the models of a snapshot: the attachments the scripts gave a player or script model, and the weapon a
//! player holds (the variant of the world model, the hand it is in, the knife of a swing, whether the gun clips a wall).

use super::{NetPlay, team_of};
use crate::models::{Attachment, PlayerModelSet};
use assets::zone::weapon::WeaponDef;
use net::entity::EntityState;
use net::ui::ClientUiState;
use server::content::Content;
use server::link::tag_name;
use sim::cm::Collide;
use sim::contents;

/// What the scripts attached to entity `e`, whose own model is `body`: model names and the tag of each, as the server
/// numbered them (`server::link::tag_wire`).
pub(super) fn attachments_of(
    ui: &ClientUiState,
    content: &Content,
    e: &EntityState,
    body: &str,
) -> Vec<Attachment> {
    let mut models = vec![body];
    let mut out = Vec::new();
    for (model, tag) in e.attachments() {
        let name = ui.model(model);
        let tag = tag_name(content, &models, tag).map_or_else(String::new, |t| t.to_string());
        out.push((name.to_owned(), tag));
        models.push(name);
    }
    out
}

/// The tag a held weapon hangs from (`CG_GetWeaponAttachBone`): a grenade in the hand, a gun in the left or right.
pub(super) fn weapon_tag(grenade: bool, left_hand: bool) -> &'static str {
    if grenade {
        "tag_inhand"
    } else if left_hand {
        "tag_weapon_left"
    } else {
        "tag_weapon_right"
    }
}

/// The world model of `def` a player holds (`weaponModel`: the variant a camouflage or attachment picks), the first when
/// the variant has none.
pub(super) fn held_model(def: &WeaponDef, variant: u8) -> Option<String> {
    def.world_models
        .get(usize::from(variant))
        .cloned()
        .flatten()
        .or_else(|| def.world_models.first().cloned().flatten())
        .and_then(|m| m.name.as_deref().map(str::to_owned))
}

/// How far from its tag a world model's stock is and how long it is, along the tag's x axis (`CG_CalcWeaponVisTrace`).
pub(super) fn barrel_of(model: &assets::zone::xmodel::XModel) -> (f32, f32) {
    let base = model.base_mat.first().map_or(0.0, |b| b.trans[0]);
    (model.mins[0] - base, model.maxs[0] - model.mins[0])
}

/// `CG_IsWeaponVisible`: a gun whose barrel runs into a wall is hidden unless the viewer can see where it starts.
/// `frame` is where the weapon's tag is and the way it points, `barrel` the stock distance and length of the model.
pub(super) fn weapon_visible(
    world: &dyn Collide,
    eye: Option<[f32; 3]>,
    owner: u16,
    frame: Option<([f32; 3], [f32; 3])>,
    barrel: (f32, f32),
) -> bool {
    let Some((origin, forward)) = frame else {
        return true;
    };
    let mask = contents::SOLID | contents::AI_NOSIGHT;
    let (to_stock, length) = barrel;
    let along = |d: f32, from: [f32; 3]| std::array::from_fn(|i| from[i] + forward[i] * d);
    let stock = along(to_stock, origin);
    let end = along(length, stock);
    let zero = [0.0; 3];
    let shot = world.trace(stock, end, zero, zero, owner, mask);
    if length - length * shot.fraction <= 3.0 {
        return true;
    }
    eye.is_none_or(|eye| world.trace(eye, stock, zero, zero, owner, mask).fraction == 1.0)
}

impl NetPlay {
    /// The models of the player or corpse `e`: the body the scripts gave it with what they attached, failing that the
    /// stock body of its team.
    pub(super) fn body_set(&self, e: &EntityState) -> Option<PlayerModelSet> {
        let ui = self.net.ui_ref()?;
        let body = ui.model(e.model);
        let attached = attachments_of(ui, &self.lib.content, e, body);
        self.lib
            .scripted_models(body, &attached)
            .or_else(|| team_of(e.eflags).and_then(|t| self.lib.team_models(t)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::cm::Trace;

    /// Thin walls across x at each of `at`.
    struct Walls(Vec<f32>);

    impl Collide for Walls {
        fn trace(
            &self,
            a: [f32; 3],
            b: [f32; 3],
            _: [f32; 3],
            _: [f32; 3],
            _: u16,
            _: i32,
        ) -> Trace {
            let mut t = Trace::MISS;
            for w in &self.0 {
                if (a[0] < *w) != (b[0] < *w) {
                    t.fraction = t.fraction.min((w - a[0]) / (b[0] - a[0]));
                }
            }
            t
        }
        fn point_contents(&self, _: [f32; 3], _: u16, _: i32) -> i32 {
            0
        }
    }

    /// A gun that starts 2 units behind its tag and is 32 long.
    const GUN: (f32, f32) = (-2.0, 32.0);

    #[test]
    fn a_gun_is_hidden_only_when_its_barrel_is_in_a_wall_and_its_stock_is_out_of_the_viewers_sight()
    {
        let frame = Some(([0.0; 3], [1.0, 0.0, 0.0]));
        let eye = Some([-50.0, 0.0, 0.0]);
        let seen = |walls: &[f32], eye| weapon_visible(&Walls(walls.to_vec()), eye, 3, frame, GUN);
        assert!(seen(&[], eye), "in the open");
        assert!(
            seen(&[10.0], eye),
            "the barrel is in a wall but the stock is in plain view"
        );
        assert!(
            !seen(&[-20.0, 10.0], eye),
            "a wall on each side of the stock"
        );
        assert!(seen(&[-20.0, 10.0], None), "no viewer to hide it from");
        assert!(seen(&[28.5], eye), "a graze of the muzzle is not enough");
    }
}
