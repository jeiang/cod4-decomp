// SPDX-License-Identifier: GPL-3.0-only
//! The master-server flow both ends share: the constants, and a client's side of asking a master for servers
//! (`getchallenge`, then `getservers` with the challenge it answers, then a list in one or more packets).

use crate::oob::{Oob, PROTOCOL};
use std::net::SocketAddr;

/// The port a master listens on and a `host` without one is assumed to use.
pub const MASTER_PORT: u16 = 28950;
/// Seconds between a server's heartbeats; a master keeps a silent server three times as long.
pub const HEARTBEAT_SECS: u64 = 60;
/// How many `sv_master`/`cl_master` entries are read.
pub const MASTER_SLOTS: usize = 5;

/// Starts a query: asks every master for a challenge.
pub fn begin(masters: &[SocketAddr], mut send: impl FnMut(SocketAddr, Oob)) {
    for m in masters {
        send(*m, Oob::GetChallenge);
    }
}

/// Feeds one packet to a query. A challenge from a master is answered with `getservers`; a list from one is
/// returned. Packets from other addresses are ignored (only a master the user named may add servers).
pub fn reply(
    masters: &[SocketAddr],
    from: SocketAddr,
    oob: Oob,
    mut send: impl FnMut(SocketAddr, Oob),
) -> Option<Vec<SocketAddr>> {
    if !masters.contains(&from) {
        return None;
    }
    match oob {
        Oob::Challenge(challenge) => {
            send(
                from,
                Oob::GetServers {
                    challenge,
                    protocol: PROTOCOL,
                },
            );
            None
        }
        Oob::Servers(list) => Some(list),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_named_master_gets_an_answer_or_adds_servers() {
        let m = SocketAddr::from(([192, 0, 2, 1], MASTER_PORT));
        let other = SocketAddr::from(([192, 0, 2, 2], MASTER_PORT));
        let mut sent = Vec::new();
        assert_eq!(
            reply(&[m], other, Oob::Challenge(5), |a, o| sent.push((a, o))),
            None
        );
        assert!(sent.is_empty());
        assert_eq!(
            reply(&[m], m, Oob::Challenge(5), |a, o| sent.push((a, o))),
            None
        );
        assert_eq!(
            sent,
            vec![(
                m,
                Oob::GetServers {
                    challenge: 5,
                    protocol: PROTOCOL
                }
            )]
        );
        let list = vec![SocketAddr::from(([203, 0, 113, 4], 28960))];
        assert_eq!(
            reply(&[m], other, Oob::Servers(list.clone()), |_, _| {}),
            None
        );
        assert_eq!(
            reply(&[m], m, Oob::Servers(list.clone()), |_, _| {}),
            Some(list)
        );
    }
}
