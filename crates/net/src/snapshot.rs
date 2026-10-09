// SPDX-License-Identifier: GPL-3.0-only
//! One snapshot (the world as one client sees it for one tick) and its delta coding against the
//! last snapshot that client acknowledged.

use crate::bits::{BitReader, BitWriter, Overflow};
use crate::entity::{self, EntityState, MAX_ENTITIES};
use crate::field::{self, changed_count, read_delta, read_sparse, write_delta, write_sparse};
use crate::ps;
use crate::ui::{self, HudElem, MAX_OBJECTIVES, Objective};
use sim::pm::PlayerState;
use sim::weapon::PlayerWeapons;

/// Snapshots a sender keeps per client to delta against; an ack older than this forces a full one.
pub const BACKUP: u32 = 32;

/// Set when the snapshot is not the receiving client's own view: a spectator following another
/// player, or a killcam replaying the past.
///
/// What the client must do: `ps` (and `inv`) belong to `followed`, not to the client, so it must
/// not predict or send movement from them and must identify itself with [`Snapshot::own`], not
/// `ps.client_num`. The world in `entities` is the state `archive_ms` milliseconds before
/// `server_time`: draw it at `server_time` as usual (the history is replayed in real time) but
/// evaluate hud element times (`HudElem::time` and the `*_start` fields, which are in the
/// archived frame's own clock) at `server_time - archive_ms`. `ps_offset_ms` is how far behind
/// the world the followed player's own view was (their latency); a killcam shows others
/// `INTERP_DELAY_MS + ps_offset_ms` behind. `entity` is the entity the scripts asked the camera
/// to track (`killcamentity`, for example the grenade or helicopter that killed), if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Follow {
    /// The receiving client's own slot.
    pub own: u16,
    /// The client whose view `ps` is.
    pub followed: u16,
    /// 0 for live spectating, else this is a killcam replay of the past.
    pub archive_ms: u32,
    pub ps_offset_ms: i32,
    pub entity: Option<u16>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Sender's sequence number for this snapshot; the receiver acknowledges it.
    pub num: u32,
    pub server_time: i32,
    pub ps: PlayerState,
    /// The viewer's weapon inventory ([`PlayerWeapons::to_words`]).
    pub inv: Box<[i32; PlayerWeapons::WORDS]>,
    /// Sorted by entity number.
    pub entities: Vec<EntityState>,
    /// The script hud elements this client sees, ascending by id.
    pub hud: Vec<HudElem>,
    /// The compass objectives this client sees.
    pub objectives: [Objective; MAX_OBJECTIVES],
    /// `None`: the snapshot is the client's own view.
    pub follow: Option<Follow>,
}

impl Snapshot {
    /// The receiving client's own slot, whoever `ps` shows.
    pub fn own(&self) -> u16 {
        self.follow.map_or(self.ps.client_num, |f| f.own)
    }

    /// True during a killcam replay.
    pub fn killcam(&self) -> bool {
        self.follow.is_some_and(|f| f.archive_ms > 0)
    }

    pub fn empty() -> Self {
        Self {
            num: 0,
            server_time: 0,
            ps: PlayerState::default(),
            inv: Box::new([0; PlayerWeapons::WORDS]),
            entities: Vec::new(),
            hud: Vec::new(),
            objectives: [Objective::default(); MAX_OBJECTIVES],
            follow: None,
        }
    }

    pub fn entity(&self, number: u16) -> Option<&EntityState> {
        self.entities
            .binary_search_by_key(&number, |e| e.number)
            .ok()
            .map(|i| &self.entities[i])
    }

    /// Rounds quantized fields as the wire does, so a sender's copy equals the receiver's.
    pub fn canonical(mut self) -> Self {
        field::canonicalize(ps::fields(), &mut self.ps);
        self.entities = self
            .entities
            .into_iter()
            .map(EntityState::canonical)
            .collect();
        self.objectives = ui::canonical_objectives(self.objectives);
        self
    }
}

/// `delta_num` is how many snapshots back `base` is (0 = full, against nothing).
pub fn write_snapshot(w: &mut BitWriter, base: Option<&Snapshot>, snap: &Snapshot) {
    w.write_u32(snap.num);
    let delta = base.map_or(0, |b| snap.num.wrapping_sub(b.num));
    w.write_bits(delta, 6);
    match base {
        Some(b) => w.write_ivar(snap.server_time.wrapping_sub(b.server_time)),
        None => w.write_i32(snap.server_time),
    }
    let zero = PlayerState::default();
    write_delta(w, ps::fields(), base.map_or(&zero, |b| &b.ps), &snap.ps);
    let zero_inv = [0i32; PlayerWeapons::WORDS];
    write_sparse(w, base.map_or(&zero_inv[..], |b| &b.inv[..]), &snap.inv[..]);
    write_entities(w, base.map_or(&[], |b| &b.entities), &snap.entities);
    ui::write_hud(w, base.map_or(&[], |b| &b.hud), &snap.hud);
    let zero_obj = [Objective::default(); MAX_OBJECTIVES];
    ui::write_objectives(
        w,
        base.map_or(&zero_obj, |b| &b.objectives),
        &snap.objectives,
    );
    match snap.follow {
        None => w.write_bool(false),
        Some(f) => {
            w.write_bool(true);
            w.write_uvar(u32::from(f.own));
            w.write_uvar(u32::from(f.followed));
            w.write_uvar(f.archive_ms);
            w.write_ivar(f.ps_offset_ms);
            w.write_uvar(f.entity.map_or(0, |e| u32::from(e) + 1));
        }
    }
}

/// The snapshot `num` just read says which base it needs: `lookup(num - delta)` supplies it.
pub fn read_snapshot<'a>(
    r: &mut BitReader,
    lookup: impl FnOnce(u32) -> Option<&'a Snapshot>,
) -> Result<Snapshot, SnapshotError> {
    let num = r.read_u32()?;
    let delta = r.read_bits(6)?;
    let base = if delta == 0 {
        None
    } else {
        Some(lookup(num.wrapping_sub(delta)).ok_or(SnapshotError::MissingBase)?)
    };
    let server_time = match base {
        Some(b) => b.server_time.wrapping_add(r.read_ivar()?),
        None => r.read_i32()?,
    };
    let mut snap = match base {
        Some(b) => b.clone(),
        None => Snapshot::empty(),
    };
    snap.num = num;
    snap.server_time = server_time;
    read_delta(r, ps::fields(), &mut snap.ps)?;
    read_sparse(r, &mut snap.inv[..])?;
    snap.entities = read_entities(r, &snap.entities)?;
    snap.hud = ui::read_hud(r, &snap.hud)?;
    ui::read_objectives(r, &mut snap.objectives)?;
    snap.follow = if r.read_bool()? {
        let small = |v: u32| u16::try_from(v).map_err(|_| Overflow);
        Some(Follow {
            own: small(r.read_uvar()?)?,
            followed: small(r.read_uvar()?)?,
            archive_ms: r.read_uvar()?,
            ps_offset_ms: r.read_ivar()?,
            entity: match r.read_uvar()? {
                0 => None,
                n => Some(small(n - 1)?),
            },
        })
    } else {
        None
    };
    Ok(snap)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotError {
    /// Delta from a snapshot this side no longer (or never) had.
    MissingBase,
    Malformed,
}

impl From<Overflow> for SnapshotError {
    fn from(_: Overflow) -> Self {
        SnapshotError::Malformed
    }
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBase => f.write_str("delta from a snapshot that is not available"),
            Self::Malformed => f.write_str("malformed snapshot"),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// Entities that changed, appeared or vanished, ascending by number: the gap from the previous
/// number plus one, a removal flag, and for the rest a field delta. A gap of 0 ends the list.
fn write_entities(w: &mut BitWriter, old: &[EntityState], new: &[EntityState]) {
    let (mut i, mut j) = (0, 0);
    let mut last = 0u32;
    let blank = EntityState::default();
    while i < old.len() || j < new.len() {
        let (on, nn) = (
            old.get(i).map_or(u16::MAX, |e| e.number),
            new.get(j).map_or(u16::MAX, |e| e.number),
        );
        let num = on.min(nn);
        let (o, n) = ((on == num).then(|| &old[i]), (nn == num).then(|| &new[j]));
        if o.is_some() {
            i += 1;
        }
        if n.is_some() {
            j += 1;
        }
        match (o, n) {
            (Some(_), None) => {
                w.write_uvar(u32::from(num) + 1 - last);
                w.write_bool(true);
                last = u32::from(num) + 1;
            }
            (o, Some(n)) => {
                let from = o.unwrap_or(&blank);
                if o.is_some() && changed_count(entity::fields(), from, n) == 0 {
                    continue;
                }
                w.write_uvar(u32::from(num) + 1 - last);
                w.write_bool(false);
                write_delta(w, entity::fields(), from, n);
                last = u32::from(num) + 1;
            }
            (None, None) => unreachable!(),
        }
    }
    w.write_uvar(0);
}

fn read_entities(r: &mut BitReader, old: &[EntityState]) -> Result<Vec<EntityState>, Overflow> {
    let mut out = Vec::with_capacity(old.len() + 8);
    let mut i = 0;
    let mut last = 0u32;
    loop {
        let gap = r.read_uvar()?;
        let at = if gap == 0 {
            u32::MAX
        } else {
            last = last.checked_add(gap).ok_or(Overflow)?;
            if last as usize > MAX_ENTITIES {
                return Err(Overflow);
            }
            last - 1
        };
        // Entities before `at` are unchanged.
        while i < old.len() && u32::from(old[i].number) < at {
            out.push(old[i].clone());
            i += 1;
        }
        if gap == 0 {
            return Ok(out);
        }
        let mut e = if i < old.len() && u32::from(old[i].number) == at {
            i += 1;
            old[i - 1].clone()
        } else {
            EntityState::new(at as u16)
        };
        if r.read_bool()? {
            continue;
        }
        read_delta(r, entity::fields(), &mut e)?;
        e.number = at as u16;
        out.push(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::etype;

    fn world(n: usize, t: i32) -> Snapshot {
        let mut s = Snapshot::empty();
        s.server_time = t;
        s.ps.origin = [t as f32 * 0.1, 5.0, 8.0];
        s.ps.command_time = t;
        s.inv[3] = 17;
        for k in 0..n {
            let num = (k * 3 + 2) as u16;
            s.entities.push(
                EntityState {
                    number: num,
                    etype: if k < 18 {
                        etype::PLAYER
                    } else {
                        etype::SCRIPT_MODEL
                    },
                    origin: [k as f32 * 100.3 + t as f32 * 0.5, -(k as f32) * 20.1, 40.0],
                    angles: [0.0, (k * 17 + t as usize) as f32 % 360.0, 0.0],
                    model: (k % 40) as u16,
                    perks: if k % 2 == 0 {
                        0x100 | (k as u32) << 10
                    } else {
                        0
                    },
                    ..EntityState::default()
                }
                .canonical(),
            );
        }
        s.canonical()
    }

    fn encode(base: Option<&Snapshot>, s: &Snapshot) -> Vec<u8> {
        let mut w = BitWriter::new();
        write_snapshot(&mut w, base, s);
        w.into_bytes()
    }

    #[test]
    fn full_and_delta_snapshots_reproduce_the_sender_state() {
        let mut a = world(40, 1000);
        a.num = 1;
        let full = encode(None, &a);
        let got = read_snapshot(&mut BitReader::new(&full), |_| None).unwrap();
        assert_eq!(got, a);

        let mut b = world(40, 1033);
        b.num = 2;
        // an entity vanishes, one appears, others move
        b.entities.remove(5);
        b.entities.insert(
            7,
            EntityState {
                number: 1000,
                etype: etype::MISSILE,
                ..EntityState::default()
            },
        );
        b.entities.sort_by_key(|e| e.number);
        let delta = encode(Some(&a), &b);
        let got2 = read_snapshot(&mut BitReader::new(&delta), |n| (n == 1).then_some(&a)).unwrap();
        assert_eq!(got2, b);
        assert!(delta.len() < full.len());
    }

    #[test]
    fn a_follow_view_round_trips_and_leaves_with_the_next_own_view() {
        let mut a = world(40, 1000);
        a.num = 1;
        a.follow = Some(Follow {
            own: 3,
            followed: 9,
            archive_ms: 4500,
            ps_offset_ms: -80,
            entity: Some(700),
        });
        assert_eq!(
            (a.own(), a.ps.client_num != 3, a.killcam()),
            (3, true, true)
        );
        let full = encode(None, &a);
        assert_eq!(
            read_snapshot(&mut BitReader::new(&full), |_| None).unwrap(),
            a
        );
        let mut b = world(40, 1033);
        b.num = 2;
        assert_eq!(b.own(), b.ps.client_num);
        assert!(!b.killcam());
        let delta = encode(Some(&a), &b);
        let got = read_snapshot(&mut BitReader::new(&delta), |n| (n == 1).then_some(&a)).unwrap();
        assert_eq!(got.follow, None);
    }

    #[test]
    fn an_unchanged_world_is_a_few_bytes() {
        let mut a = world(60, 1000);
        a.num = 1;
        let mut b = a.clone();
        b.num = 2;
        b.server_time += 33;
        let d = encode(Some(&a), &b);
        assert!(d.len() < 16, "{} bytes", d.len());
    }

    #[test]
    fn a_missing_base_and_garbage_are_errors() {
        let mut a = world(3, 10);
        a.num = 5;
        let mut b = world(3, 20);
        b.num = 6;
        let d = encode(Some(&a), &b);
        assert_eq!(
            read_snapshot(&mut BitReader::new(&d), |_| None).unwrap_err(),
            SnapshotError::MissingBase
        );
        for cut in 0..d.len() {
            let _ = read_snapshot(&mut BitReader::new(&d[..cut]), |_| Some(&a));
        }
        let junk: Vec<u8> = (0..200).map(|i| (i * 37 + 11) as u8).collect();
        let _ = read_snapshot(&mut BitReader::new(&junk), |_| Some(&a));
    }

    #[test]
    fn a_long_chain_of_deltas_stays_in_sync() {
        let mut sender: Vec<Snapshot> = Vec::new();
        let mut receiver: Option<Snapshot> = None;
        for t in 1..200u32 {
            let mut s = world(30 + (t as usize % 5), 1000 + t as i32 * 33);
            s.num = t;
            let base = sender.last();
            let bytes = encode(base, &s);
            let got = read_snapshot(&mut BitReader::new(&bytes), |n| {
                receiver.as_ref().filter(|r| r.num == n)
            })
            .unwrap();
            assert_eq!(got, s, "tick {t}");
            receiver = Some(got);
            sender.push(s);
        }
    }
}
