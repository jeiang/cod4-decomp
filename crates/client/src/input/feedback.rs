// SPDX-License-Identifier: GPL-3.0-only
//! What the cgame hands back to the input layer: the original's `CL_SetStance` / `CL_SetADS` calls from the own
//! player's events (`cg_event.cpp`: `EV_STANCE_FORCE_*`, `EV_RESET_ADS`) and from a respawn (`cg_predict_mp.cpp`:
//! `PMF_RESPAWNED`), and the `PMF_FROZEN` gate on looking.
//!
//! Without it the stance a `goprone` / `gocrouch` latched is never given up: the movement code forces the player
//! out of it (sprinting, a ladder, a blocked stand-up) and the client keeps asking for it again.

use super::buttons;
use sim::pm::{PlayerState, ev, pmf};

/// What [`scan_own`] has already acted on: the event counter and the spawn count.
#[derive(Clone, Copy, Debug, Default)]
pub struct Seen {
    events: Option<u8>,
    spawn: Option<u16>,
}

/// What to tell [`super::Input::apply`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feedback {
    /// Stances the player was forced into, oldest first: `0`, `buttons::CROUCH` or `buttons::PRONE`.
    pub stances: Vec<u32>,
    /// The ADS the client was holding on is over (a reset, a respawn).
    pub leave_ads: bool,
    /// The player state is frozen: no looking.
    pub frozen: bool,
    /// The held weapon ran dry (`EV_NOAMMO`): [`scan_own`]'s caller switches weapons and does not pass this on.
    pub out_of_ammo: bool,
}

impl Feedback {
    /// Adds `later` after what is already here, so nothing is lost between two `Input::apply` calls.
    pub fn merge(&mut self, later: Feedback) {
        self.stances.extend(later.stances);
        self.leave_ads |= later.leave_ads;
        self.frozen = later.frozen;
    }

    /// Takes everything collected, leaving it empty (the frozen flag stays: it is a state, not an event).
    pub fn take(&mut self) -> Feedback {
        let frozen = self.frozen;
        Feedback {
            stances: std::mem::take(&mut self.stances),
            leave_ads: std::mem::take(&mut self.leave_ads),
            frozen,
            ..Feedback::default()
        }
    }
}

/// The events of `ps` raised since `last` (the previous call's `event_sequence`; the first call only learns it),
/// as feedback. `ps` is the own player's state; its four-slot event ring keeps at most the newest four.
pub fn scan_own(seen: &mut Seen, ps: &PlayerState) -> Feedback {
    let mut fb = Feedback {
        frozen: ps.pm_flags & pmf::FROZEN != 0,
        ..Feedback::default()
    };
    let seq = ps.event_sequence;
    if let Some(prev) = seen.events.replace(seq) {
        let fresh = usize::from(seq.wrapping_sub(prev)).min(4);
        for k in (1..=fresh).rev() {
            match ps.events[usize::from(seq.wrapping_sub(k as u8)) & 3] {
                ev::STANCE_FORCE_STAND => fb.stances.push(0),
                ev::STANCE_FORCE_CROUCH => fb.stances.push(buttons::CROUCH),
                ev::STANCE_FORCE_PRONE => fb.stances.push(buttons::PRONE),
                ev::RESET_ADS => fb.leave_ads = true,
                ev::NOAMMO => fb.out_of_ammo = true,
                _ => {}
            }
        }
    }
    // A new life: stand, once (`CL_SetStance(STAND)` in the original; ADS is reset by its own event).
    if seen
        .spawn
        .replace(ps.spawn_count)
        .is_some_and(|n| n != ps.spawn_count)
    {
        fb.stances.push(0);
    }
    fb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps_with(events: &[u8]) -> PlayerState {
        let mut ps = PlayerState::default();
        for &e in events {
            ps.add_event(e, 0);
        }
        ps
    }

    #[test]
    fn only_events_new_since_the_last_look_count() {
        let mut last = Seen::default();
        // The first look learns the counter: old events are not replayed.
        assert_eq!(
            scan_own(&mut last, &ps_with(&[ev::STANCE_FORCE_PRONE])),
            Feedback::default()
        );
        let mut ps = ps_with(&[ev::STANCE_FORCE_PRONE]);
        ps.add_event(ev::STANCE_FORCE_CROUCH, 0);
        ps.add_event(ev::STANCE_FORCE_STAND, 0);
        ps.add_event(ev::RESET_ADS, 0);
        let fb = scan_own(&mut last, &ps);
        assert_eq!(fb.stances, [buttons::CROUCH, 0]);
        assert!(fb.leave_ads);
        // Nothing new the next time.
        assert_eq!(scan_own(&mut last, &ps), Feedback::default());
    }

    #[test]
    fn running_dry_is_reported_once() {
        let mut seen = Seen::default();
        let mut ps = ps_with(&[]);
        scan_own(&mut seen, &ps);
        ps.add_event(ev::NOAMMO, 0);
        assert!(scan_own(&mut seen, &ps).out_of_ammo);
        assert!(!scan_own(&mut seen, &ps).out_of_ammo);
    }

    #[test]
    fn a_new_spawn_stands_once_and_frozen_is_a_state() {
        let mut seen = Seen::default();
        let mut ps = PlayerState::default();
        ps.pm_flags |= pmf::FROZEN;
        assert!(scan_own(&mut seen, &ps).stances.is_empty());
        ps.spawn_count += 1;
        let fb = scan_own(&mut seen, &ps);
        assert_eq!(fb.stances, [0]);
        assert!(!fb.leave_ads && fb.frozen);
        assert!(
            scan_own(&mut seen, &ps).stances.is_empty(),
            "once per spawn"
        );
        let mut held = fb.clone();
        held.leave_ads = true;
        let taken = held.take();
        assert!(taken.leave_ads);
        assert!(held.frozen && held.stances.is_empty() && !held.leave_ads);
    }
}
