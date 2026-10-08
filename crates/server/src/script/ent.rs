// SPDX-License-Identifier: GPL-3.0-or-later
//! Entity and world builtins: linking, attachments, tags, touch tests, corpses, use triggers
//! and the vector helpers scripts build on them.

use gsc::{EntClass, EntRef, Value, Vm};
use sim::cm::{ClipModel, Trace};
use sim::contents;

use super::Impl::{self, Real};
use super::methods::live;
use super::{Args, FuncFn, MethFn};
use crate::client::Team;
use crate::delta;
use crate::game::{Ent, EntKind, Game};
use crate::link::MAX_ATTACH;
use crate::tags;

type R = Result<Value, String>;

const fn f(f: FuncFn) -> Impl<FuncFn> {
    Real(f)
}

const fn m(f: MethFn) -> Impl<MethFn> {
    Real(f)
}

pub const FUNCS: &[(&str, Impl<FuncFn>)] = &[
    ("getentbynum", f(get_ent_by_num)),
    ("getbrushmodelcenter", f(get_brush_model_center)),
    ("combineangles", f(combine_angles)),
    ("getnumparts", f(get_num_parts)),
    ("getpartname", f(get_part_name)),
    ("getmovedelta", f(|g, _, a| anim_delta(g, a, false))),
    ("getangledelta", f(|g, _, a| anim_delta(g, a, true))),
];

pub const METHODS: &[(&str, Impl<MethFn>)] = &[
    ("attach", m(attach)),
    ("detach", m(detach)),
    ("detachall", m(detach_all)),
    ("getattachsize", m(get_attach_size)),
    ("getattachmodelname", m(get_attach_model_name)),
    ("getattachtagname", m(get_attach_tag_name)),
    ("getattachignorecollision", m(get_attach_ignore_collision)),
    ("hidepart", m(|g, _, e, a| set_part(g, e, a, true))),
    ("showpart", m(|g, _, e, a| set_part(g, e, a, false))),
    ("showallparts", m(show_all_parts)),
    ("linkto", m(link_to)),
    ("unlink", m(unlink)),
    ("enablelinkto", m(enable_link_to)),
    ("istouching", m(is_touching)),
    ("gettagorigin", m(get_tag_origin)),
    ("gettagangles", m(get_tag_angles)),
    ("localtoworldcoords", m(local_to_world)),
    ("placespawnpoint", m(place_spawn_point)),
    ("physicslaunch", m(physics_launch)),
    ("startragdoll", m(start_ragdoll)),
    ("isragdoll", m(is_ragdoll)),
    ("getcorpseanim", m(get_corpse_anim)),
    ("cloneplayer", m(clone_player)),
    ("getnormalhealth", m(get_normal_health)),
    ("setnormalhealth", m(set_normal_health)),
    ("useby", m(use_by)),
    ("sethintstring", m(set_hint_string)),
    ("setcursorhint", m(set_cursor_hint)),
    ("usetriggerrequirelookat", m(use_trigger_require_look_at)),
    ("setteamfortrigger", m(set_team_for_trigger)),
    ("clientclaimtrigger", m(client_claim_trigger)),
    ("clientreleasetrigger", m(client_release_trigger)),
    ("releaseclaimedtrigger", m(release_claimed_trigger)),
];

fn bool_v(b: bool) -> Value {
    Value::Int(i32::from(b))
}

fn classname(g: &Game, n: u16) -> String {
    g.ent(n).map_or(String::new(), |e| e.classname.to_string())
}

/// `GetEntity`, then the entity itself.
fn ent_of(g: &Game, e: EntRef) -> Result<&Ent, String> {
    live(g, e)?;
    g.ent(e.num).ok_or_else(|| "not an entity".to_owned())
}

fn get_ent_by_num(g: &mut Game, vm: &mut Vm, a: Args) -> R {
    let n = a.int(0)?;
    match u16::try_from(n).ok().filter(|n| usize::from(*n) < 1024) {
        Some(n) if g.ent(n).is_some() => Ok(Value::Object(vm.entity(n, EntClass::Entity))),
        _ => Ok(Value::Undefined),
    }
}

/// `getbrushmodelcenter(ent)`: the middle of the entity's absolute bounds.
fn get_brush_model_center(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let n = a.entity(0)?.num;
    let origin = g.ent(n).ok_or("not an entity")?.origin;
    let (lo, hi) = g
        .world
        .as_ref()
        .and_then(|w| w.entity(n))
        .map_or((origin, origin), |l| (l.abs_min, l.abs_max));
    Ok(Value::Vector([
        (lo[0] + hi[0]) * 0.5,
        (lo[1] + hi[1]) * 0.5,
        (lo[2] + hi[2]) * 0.5,
    ]))
}

/// `combineangles(a, b)`: the angles of `b` applied in the frame of `a`.
fn combine_angles(_: &mut Game, _: &mut Vm, a: Args) -> R {
    let (x, y) = (a.vector(0)?, a.vector(1)?);
    let c = tags::mul3(&tags::angles_to_axis(y), &tags::angles_to_axis(x));
    Ok(Value::Vector(tags::axis_to_angles(&c)))
}

fn model_skeleton<'a>(g: &'a Game, a: &Args) -> Result<&'a std::sync::Arc<tags::Skeleton>, String> {
    let name = a.string(0)?;
    g.content
        .skeleton(name)
        .ok_or_else(|| format!("model '{name}' not found"))
}

fn get_num_parts(g: &mut Game, _: &mut Vm, a: Args) -> R {
    Ok(Value::Int(model_skeleton(g, &a)?.len() as i32))
}

fn get_part_name(g: &mut Game, _: &mut Vm, a: Args) -> R {
    let index = a.int(1)?;
    let skel = model_skeleton(g, &a)?;
    let n = skel.len() as i32;
    match usize::try_from(index).ok().filter(|i| *i < skel.len()) {
        Some(i) => Ok(Value::str(skel.name(i).unwrap_or(""))),
        None => Err(format!("index out of range (0 - {})", n - 1)),
    }
}

/// `getmovedelta(anim[, start, end])` and `getangledelta`: the root motion of the part of
/// the animation between the two normalized times.
fn anim_delta(g: &Game, a: Args, yaw: bool) -> R {
    let (mut start, mut end) = (0.0, 1.0);
    if a.len() != 1 {
        if a.len() != 2 {
            end = a.float(2)?;
            if !(0.0..=1.0).contains(&end) {
                return Err("end time must be between 0 and 1".into());
            }
        }
        start = a.float(1)?;
        if !(0.0..=1.0).contains(&start) {
            return Err("start time must be between 0 and 1".into());
        }
    }
    let Value::Anim(name) = a.get(0)? else {
        return Err(format!("type {} is not an anim", a.get(0)?.type_name()));
    };
    if g.content.anim(name).is_none() {
        return Err(format!("animation '{name}' is not loaded"));
    }
    let (rot, trans) = g
        .content
        .root_motion(name)
        .map_or(([0.0, 1.0], [0.0; 3]), |m| delta::rel_delta(m, start, end));
    Ok(if yaw {
        Value::Float(delta::rotation_to_yaw(rot))
    } else {
        Value::Vector(trans)
    })
}

// ---- attachments ----

/// The tag argument of the attach family: lowercase, empty when absent.
fn tag_arg(a: &Args, i: usize) -> Result<String, String> {
    if a.len() < i + 1 {
        Ok(String::new())
    } else {
        Ok(a.string(i)?.to_ascii_lowercase())
    }
}

fn attach(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    live(g, e)?;
    let model = a.string(0)?;
    let tag = tag_arg(&a, 1)?;
    let ignore = if a.len() < 3 { 0 } else { a.int(2)? };
    if g.detach_model(e.num, model, &tag) {
        return Err(format!("model '{model}' already attached to tag '{tag}'"));
    }
    if !g.attach_model(e.num, model, &tag, ignore != 0) {
        return Err(format!("failed to attach model '{model}' to tag '{tag}'"));
    }
    Ok(Value::Undefined)
}

fn detach(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    live(g, e)?;
    let model = a.string(0)?;
    let tag = tag_arg(&a, 1)?;
    if g.detach_model(e.num, model, &tag) {
        return Ok(Value::Undefined);
    }
    let mut text = String::from("Current attachments:\n");
    if let Some(ent) = g.ent(e.num) {
        for at in &ent.x.attached {
            text.push_str(&format!("model: '{}', tag: '{}'\n", at.model, at.tag));
        }
    }
    g.print(text);
    Err(format!("failed to detach model '{model}' from tag '{tag}'"))
}

fn detach_all(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    live(g, e)?;
    if let Some(ent) = g.ent_mut(e.num) {
        ent.x.attached.clear();
    }
    Ok(Value::Undefined)
}

fn get_attach_size(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    Ok(Value::Int(ent_of(g, e)?.x.attached.len() as i32))
}

fn attach_at<'a>(g: &'a Game, e: EntRef, a: &Args) -> Result<&'a crate::link::Attach, String> {
    let i = a.int(0)?;
    usize::try_from(i)
        .ok()
        .filter(|i| *i < MAX_ATTACH)
        .and_then(|i| ent_of(g, e).ok()?.x.attached.get(i))
        .ok_or_else(|| "bad index".to_owned())
}

fn get_attach_model_name(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    Ok(Value::str(&attach_at(g, e, &a)?.model))
}

fn get_attach_tag_name(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    Ok(Value::str(&attach_at(g, e, &a)?.tag))
}

fn get_attach_ignore_collision(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    Ok(bool_v(attach_at(g, e, &a)?.ignore_collision))
}

/// `hidepart` / `showpart`: the part is a bone of the entity's model (or of the named one
/// among its attachments); the entity records which bones are hidden.
fn set_part(g: &mut Game, e: EntRef, a: Args, hide: bool) -> R {
    live(g, e)?;
    let models = g.dobj_models(e.num);
    if models.is_empty() {
        return Err("entity has no model".into());
    }
    let tag = a.string(0)?.to_ascii_lowercase();
    let mut base = 0;
    let mut found = None;
    let wanted = (a.len() > 1).then(|| a.string(1)).transpose()?;
    for (name, skel) in &models {
        if wanted.is_none_or(|w| name.eq_ignore_ascii_case(w))
            && let Some(i) = skel.bone_index(&tag)
        {
            found = Some(base + i);
            break;
        }
        base += skel.len();
    }
    let Some(bone) = found else {
        return Err(match wanted {
            None => format!("cannot find part '{tag}' in entity model"),
            Some(w) => format!("cannot find part '{tag}' in entity model '{w}'"),
        });
    };
    if let Some(ent) = g.ent_mut(e.num) {
        let bit = 0x8000_0000u32 >> (bone & 31);
        let word = &mut ent.x.hide_bits[(bone >> 5).min(3)];
        if hide {
            *word |= bit;
        } else {
            *word &= !bit;
        }
    }
    Ok(Value::Undefined)
}

fn show_all_parts(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    live(g, e)?;
    if !g.dobj_exists(e.num) {
        return Err("entity has no model".into());
    }
    if let Some(ent) = g.ent_mut(e.num) {
        ent.x.hide_bits = [0; 4];
    }
    Ok(Value::Undefined)
}

// ---- linking ----

fn link_to(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    live(g, e)?;
    let parent = a
        .entity(0)
        .ok()
        .filter(|p| g.ent(p.num).is_some())
        .ok_or("not an entity")?;
    if !g.supports_linkto(e.num) {
        return Err(format!(
            "entity (classname: '{}') does not currently support linkTo",
            classname(g, e.num)
        ));
    }
    let tag = if a.len() >= 2 {
        Some(a.string(1)?)
    } else {
        None
    };
    let offset = if a.len() > 2 {
        Some((a.vector(2)?, a.vector(3)?))
    } else {
        None
    };
    g.link_to(e.num, parent.num, tag, offset)
        .map_err(|err| err.message())?;
    Ok(Value::Undefined)
}

fn unlink(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    live(g, e)?;
    g.unlink(e.num);
    Ok(Value::Undefined)
}

fn enable_link_to(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let ent = ent_of(g, e)?;
    if g.supports_linkto(e.num) {
        return Err("entity already has linkTo enabled".into());
    }
    if matches!(ent.kind, EntKind::Client | EntKind::Item | EntKind::World) {
        return Err(format!(
            "entity (classname: '{}') does not currently support enableLinkTo",
            ent.classname
        ));
    }
    if let Some(ent) = g.ent_mut(e.num) {
        ent.flags |= crate::link::FL_SUPPORTS_LINKTO;
    }
    Ok(Value::Undefined)
}

// ---- touch ----

/// An entity that is tested by shape instead of being the box: brush models and the
/// cylinder and disk triggers.
fn is_shaped(e: &Ent) -> bool {
    e.brush_model.is_some() || matches!(&*e.classname, "trigger_radius" | "trigger_disk")
}

/// `SV_EntityContact`: whether the box `mins..maxs` touches `other`.
pub(crate) fn entity_contact(g: &Game, mins: [f32; 3], maxs: [f32; 3], other: &Ent) -> bool {
    let center = [(mins[0] + maxs[0]) * 0.5, (mins[1] + maxs[1]) * 0.5];
    let d2 = |o: [f32; 3]| (o[0] - center[0]).powi(2) + (o[1] - center[1]).powi(2);
    match &*other.classname {
        "trigger_radius" => {
            if other.origin[2] < maxs[2] && mins[2] < other.origin[2] + other.maxs[2] {
                let dist = maxs[0] - center[0] + other.maxs[0];
                dist * dist > d2(other.origin)
            } else {
                false
            }
        }
        "trigger_disk" => {
            let dist = maxs[0] - center[0] + other.maxs[0] - 64.0;
            dist * dist <= d2(other.origin)
        }
        _ => {
            let Some(w) = g.world.as_ref() else {
                return false;
            };
            let (model, angles) = match other.brush_model {
                Some(n) => (ClipModel::Submodel(n), other.angles),
                None => (
                    ClipModel::Box {
                        mins: other.mins,
                        maxs: other.maxs,
                        contents: other.contents,
                    },
                    [0.0; 3],
                ),
            };
            let mut t = Trace::MISS;
            w.collision().transformed_trace(
                &mut t,
                [0.0; 3],
                [0.0; 3],
                mins,
                maxs,
                &model,
                contents::MASK_ALL,
                other.origin,
                angles,
            );
            t.start_solid
        }
    }
}

/// `istouching(other)`: the receiver's box (stretched to be at least as tall as it is wide)
/// against the other entity's shape. A brush or cylinder receiver swaps roles with its
/// argument.
fn is_touching(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let me = ent_of(g, e)?;
    let arg = a.entity(0)?;
    let other = ent_of(g, arg)?;
    let (boxed, shaped) = if is_shaped(me) {
        if is_shaped(other) {
            return Err("istouching cannot be called on 2 brush/cylinder entities".into());
        }
        (other, me)
    } else {
        (me, other)
    };
    let mut mins = [0.0; 3];
    let mut maxs = [0.0; 3];
    for i in 0..3 {
        mins[i] = boxed.origin[i] + boxed.mins[i];
        maxs[i] = boxed.origin[i] + boxed.maxs[i];
    }
    let wide = (maxs[0] - mins[0]).max(maxs[1] - mins[1]);
    let tall = maxs[2] - mins[2];
    if tall < wide {
        let d = (wide - tall) * 0.5;
        mins[2] -= d;
        maxs[2] += d;
    }
    Ok(bool_v(entity_contact(g, mins, maxs, shaped)))
}

// ---- tags ----

fn tag_matrix(g: &Game, e: EntRef, a: &Args) -> Result<tags::Mat43, String> {
    let ent = ent_of(g, e)?;
    let tag = a.string(0)?.to_ascii_lowercase();
    if !g.dobj_exists(e.num) {
        return Err(format!(
            "entity has no model defined (classname '{}')",
            ent.classname
        ));
    }
    g.world_tag(e.num, &tag).ok_or_else(|| {
        format!(
            "tag '{tag}' does not exist in model '{}' (or any attached submodels)",
            ent.model
        )
    })
}

fn get_tag_origin(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    Ok(Value::Vector(tag_matrix(g, e, &a)?[3]))
}

fn get_tag_angles(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let mat = tag_matrix(g, e, &a)?;
    Ok(Value::Vector(tags::axis_to_angles(&[
        mat[0], mat[1], mat[2],
    ])))
}

fn local_to_world(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let ent = ent_of(g, e)?;
    let p = tags::transform43(a.vector(0)?, &tags::frame(ent.origin, ent.angles));
    Ok(Value::Vector(p))
}

// ---- spawn points ----

/// `placespawnpoint()`: see [`Game::place_spawn_point`].
fn place_spawn_point(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    ent_of(g, e)?;
    g.place_spawn_point(e.num);
    Ok(Value::Undefined)
}

// ---- physics and corpses ----

/// `physicslaunch([point, force])`: the clients simulate the launched body; the server
/// stops treating it as solid and damageable.
fn physics_launch(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let ent = ent_of(g, e)?;
    if !matches!(
        &*ent.classname,
        "script_brushmodel" | "script_model" | "script_origin" | "light"
    ) {
        return Err(format!(
            "entity {} is not a script_brushmodel, script_model, script_origin, or light",
            e.num
        ));
    }
    let launch = if a.len() == 2 {
        (a.vector(0)?, a.vector(1)?)
    } else {
        ([0.0; 3], [0.0; 3])
    };
    if let Some(ent) = g.ent_mut(e.num) {
        ent.x.physics_launch = Some(launch);
        ent.contents = 0;
        ent.takedamage = false;
    }
    g.relink(e.num);
    Ok(Value::Undefined)
}

fn start_ragdoll(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    live(g, e)?;
    if !a.is_empty() {
        a.int(0)?;
    }
    if let Some(ent) = g.ent_mut(e.num) {
        ent.x.ragdoll = true;
    }
    Ok(Value::Undefined)
}

fn is_ragdoll(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    Ok(bool_v(ent_of(g, e)?.x.ragdoll))
}

fn get_corpse_anim(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    match &ent_of(g, e)?.x.corpse {
        Some(c) => Ok(Value::Anim(c.anim.clone())),
        None => Err("Only valid on player corpses".into()),
    }
}

/// The death animation of a body: chosen from the way the player was moving.
fn death_anim(g: &Game, n: u16) -> &'static str {
    let Some(c) = g.client(n) else {
        return "pb_death_run_onfront";
    };
    let v = c.ps.velocity;
    if v[0] * v[0] + v[1] * v[1] < 40.0 * 40.0 {
        return "pb_death_run_onfront";
    }
    let rel = crate::client::angle_180(v[1].atan2(v[0]).to_degrees() - c.ps.viewangles[1]);
    match rel.abs() {
        r if r < 45.0 => "pb_death_run_forward_crumple",
        r if r > 135.0 => "pb_death_run_back",
        _ if rel > 0.0 => "pb_death_run_left",
        _ => "pb_death_run_right",
    }
}

fn clone_player(g: &mut Game, vm: &mut Vm, e: EntRef, a: Args) -> R {
    live(g, e)?;
    if !g.is_client(e.num) {
        return Err(format!("entity {} is not a player", e.num));
    }
    let duration = a.int(0)?;
    let anim = death_anim(g, e.num);
    match g.clone_player(vm, e.num, duration, anim) {
        Some(n) => Ok(Value::Object(vm.entity(n, EntClass::Entity))),
        None => Ok(Value::Undefined),
    }
}

// ---- health ----

fn get_normal_health(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    let ent = ent_of(g, e)?;
    if let Some(c) = g.client(e.num) {
        return Ok(Value::Float(if ent.health != 0 {
            ent.health as f32 / c.max_health as f32
        } else {
            0.0
        }));
    }
    Ok(Value::Float(ent.health as f32))
}

fn set_normal_health(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    ent_of(g, e)?;
    let normal = a.float(0)?.min(1.0);
    let health = match g.client(e.num) {
        Some(c) => (c.max_health as f32 * normal).round_ties_even() as i32,
        None => normal as i32,
    };
    if health > 0 {
        if let Some(ent) = g.ent_mut(e.num) {
            ent.health = health;
        }
    } else {
        g.print("ERROR: Cannot setnormalhealth to 0 or below.\n");
    }
    Ok(Value::Undefined)
}

// ---- use triggers ----

fn is_use_trigger(class: &str) -> bool {
    matches!(class, "trigger_use" | "trigger_use_touch")
}

fn require_use_trigger(g: &Game, e: EntRef, who: &str) -> Result<(), String> {
    if is_use_trigger(&ent_of(g, e)?.classname) {
        Ok(())
    } else {
        Err(format!(
            "{who}: trigger entity must be of type trigger_use or trigger_use_touch"
        ))
    }
}

/// `useby(player)`: the trigger's `trigger` notify with the user.
fn use_by(g: &mut Game, vm: &mut Vm, e: EntRef, a: Args) -> R {
    live(g, e)?;
    let other = a.entity(0)?;
    if g.ent(other.num).is_none() {
        return Err("not an entity".into());
    }
    let who = g.entity_value(vm, other.num);
    vm.notify_entity(e.num, "trigger", &[who]);
    Ok(Value::Undefined)
}

const HINTS: [&str; 4] = [
    "HINT_NOICON",
    "HINT_ACTIVATE",
    "HINT_HEALTH",
    "HINT_FRIENDLY",
];

/// A hint type name's value: `HINT_INHERIT` (-1, use triggers only) or its place in [`HINTS`] plus one.
pub(crate) fn hint_value(hint: &str, use_trigger: bool) -> Option<i32> {
    if use_trigger && hint.eq_ignore_ascii_case("HINT_INHERIT") {
        Some(-1)
    } else {
        HINTS
            .iter()
            .position(|h| h.eq_ignore_ascii_case(hint))
            .map(|i| i as i32 + 1)
    }
}

fn set_cursor_hint(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    let ent = ent_of(g, e)?;
    let hint = a.string(0)?;
    let use_trigger = is_use_trigger(&ent.classname);
    let value = hint_value(hint, use_trigger);
    let Some(value) = value else {
        let mut list = String::from("List of valid hint type strings\n");
        if use_trigger {
            list.push_str("HINT_INHERIT (for trigger_use or trigger_use_touch entities only)\n");
        }
        for h in HINTS {
            list.push_str(h);
            list.push('\n');
        }
        g.print(list);
        return Err(format!(
            "{hint} is not a valid hint type. See above for list of valid hint types\n"
        ));
    };
    if let Some(ent) = g.ent_mut(e.num) {
        ent.x.cursor_hint = value;
    }
    Ok(Value::Undefined)
}

fn set_hint_string(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if !is_use_trigger(&ent_of(g, e)?.classname) {
        return Err(
            "The setHintString command only works on trigger_use or trigger_use_touch entities.\n"
                .into(),
        );
    }
    let empty = matches!(a.get(0)?, Value::Str(s) if s.is_empty());
    let hint = if empty {
        None
    } else {
        let mut text = String::new();
        for i in 0..a.len() {
            text.push_str(&a.display(i)?);
        }
        Some(g.hint_string_index(&text)?)
    };
    if let Some(ent) = g.ent_mut(e.num) {
        ent.x.hint = hint;
    }
    Ok(Value::Undefined)
}

fn use_trigger_require_look_at(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    if &*ent_of(g, e)?.classname != "trigger_use" {
        return Err(
            "The UseTriggerRequireLookAt command only works on trigger_use entities.\n".into(),
        );
    }
    if let Some(ent) = g.ent_mut(e.num) {
        ent.x.require_look_at = true;
    }
    Ok(Value::Undefined)
}

fn set_team_for_trigger(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if !is_use_trigger(&ent_of(g, e)?.classname) {
        return Err(
            "setteamfortrigger: trigger entity must be of type trigger_use or trigger_use_touch"
                .into(),
        );
    }
    let team = match a.string(0)? {
        "allies" => Team::Allies,
        "axis" => Team::Axis,
        "none" => Team::Free,
        _ => {
            return Err("setteamfortrigger: invalid team used must be allies, axis or none".into());
        }
    };
    if let Some(ent) = g.ent_mut(e.num) {
        ent.x.trigger_team = team;
    }
    Ok(Value::Undefined)
}

fn client_claim_trigger(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if !g.is_client(e.num) {
        return Err("clientclaimtrigger: claimer must be a client.".into());
    }
    let t = a.entity(0)?;
    require_use_trigger(g, t, "clientclaimtrigger")?;
    if let Some(ent) = g.ent_mut(t.num)
        && ent.x.claimed_by.is_none_or(|c| c == e.num)
    {
        ent.x.claimed_by = Some(e.num);
    }
    Ok(Value::Undefined)
}

fn client_release_trigger(g: &mut Game, _: &mut Vm, e: EntRef, a: Args) -> R {
    if !g.is_client(e.num) {
        return Err("clientreleasetrigger: releaser must be a client.".into());
    }
    let t = a.entity(0)?;
    require_use_trigger(g, t, "clientreleasetrigger")?;
    if let Some(ent) = g.ent_mut(t.num)
        && ent.x.claimed_by == Some(e.num)
    {
        ent.x.claimed_by = None;
    }
    Ok(Value::Undefined)
}

fn release_claimed_trigger(g: &mut Game, _: &mut Vm, e: EntRef, _: Args) -> R {
    require_use_trigger(g, e, "releaseclaimedtrigger")?;
    if let Some(ent) = g.ent_mut(e.num) {
        ent.x.claimed_by = None;
    }
    Ok(Value::Undefined)
}
