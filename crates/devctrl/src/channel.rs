// SPDX-License-Identifier: GPL-3.0-only
//! The channel: takes requests off its transport, asks the handler, writes the replies back.
//!
//! Requests are answered in the order they arrived. A handler that cannot answer yet returns
//! [`Outcome::Wait`]; the request stays at the front of the queue and is offered again by the next [`Channel::service`],
//! which is what makes `wait` cost a frame rather than a sleep. A request that never completes is the handler's to
//! time out.

use crate::protocol::{Reply, Request};
use crate::transport::Transport;
use serde_json::Value;
use std::collections::VecDeque;

/// What a handler did with one request.
pub enum Outcome {
    /// Answered: the reply goes back to the connection that asked.
    Done(Reply),
    /// Not yet: the request is offered again after the next frame.
    Wait,
}

/// Answers requests against the running game.
pub trait Handler {
    fn handle(&mut self, req: &Request) -> Outcome;
}

pub struct Channel {
    transport: Box<dyn Transport>,
    /// Requests that arrived and have no reply yet, oldest first.
    queue: VecDeque<Request>,
    next_seq: u64,
    answered: u64,
}

impl Channel {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Self {
            transport,
            queue: VecDeque::new(),
            next_seq: 0,
            answered: 0,
        }
    }

    /// Takes the transport's new lines and answers what it can. Returns how many requests were answered.
    ///
    /// Called once a frame; a handler that waits is retried on the next call.
    pub fn service(&mut self, h: &mut dyn Handler) -> usize {
        let mut done = 0;
        for (conn, line) in self.transport.poll() {
            if line.len() > crate::MAX_LINE {
                let why = format!("the request is longer than {} bytes", crate::MAX_LINE);
                self.transport
                    .reply(conn, &Reply::failed(Value::Null, why).line());
                continue;
            }
            self.next_seq += 1;
            match Request::parse(&line, conn, self.next_seq) {
                Ok(req) => self.queue.push_back(req),
                Err(why) => {
                    tracing::warn!(error = %why, "control: a request could not be read");
                    self.transport
                        .reply(conn, &Reply::failed(Value::Null, why).line());
                }
            }
        }
        while let Some(req) = self.queue.front() {
            match h.handle(req) {
                Outcome::Done(reply) => {
                    self.transport.reply(req.conn, &reply.line());
                    self.queue.pop_front();
                    self.answered += 1;
                    done += 1;
                }
                Outcome::Wait => break,
            }
        }
        done
    }

    /// Requests that arrived and have no reply yet.
    pub fn waiting(&self) -> usize {
        self.queue.len()
    }

    /// The request at the front of the queue: the one a wait is holding.
    pub fn front(&self) -> Option<&Request> {
        self.queue.front()
    }

    /// Requests answered so far.
    pub fn answered(&self) -> u64 {
        self.answered
    }

    /// The transport, for a caller that polls it itself (the browser's page).
    pub fn transport_mut(&mut self) -> &mut dyn Transport {
        &mut *self.transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// A transport with a queue in and a record out, so a test can read the replies in order.
    #[derive(Default, Clone)]
    struct Recorder {
        out: Arc<Mutex<Vec<(u64, String)>>>,
        inbox: Arc<Mutex<VecDeque<(u64, String)>>>,
    }

    impl Recorder {
        fn push(&self, conn: u64, line: &str) {
            self.inbox
                .lock()
                .unwrap()
                .push_back((conn, line.to_owned()));
        }

        fn replies(&self) -> Vec<(u64, String)> {
            self.out.lock().unwrap().clone()
        }
    }

    impl Transport for Recorder {
        fn poll(&mut self) -> Vec<(u64, String)> {
            self.inbox.lock().unwrap().drain(..).collect()
        }

        fn reply(&mut self, conn: u64, line: &str) {
            self.out.lock().unwrap().push((conn, line.to_owned()));
        }
    }

    /// A handler that answers `ping`, waits three times on `wait`, and refuses anything else.
    #[derive(Default)]
    struct Pinger {
        pings: usize,
        waits: usize,
    }

    impl Handler for Pinger {
        fn handle(&mut self, req: &Request) -> Outcome {
            match req.cmd.as_str() {
                "ping" => {
                    self.pings += 1;
                    Outcome::Done(Reply::ok(req.id.clone()).set("pong", true))
                }
                "wait" => {
                    self.waits += 1;
                    if self.waits < 3 {
                        Outcome::Wait
                    } else {
                        Outcome::Done(Reply::ok(req.id.clone()))
                    }
                }
                _ => Outcome::Done(Reply::failed(req.id.clone(), "no such command")),
            }
        }
    }

    fn channel(t: &Recorder) -> Channel {
        Channel::new(Box::new(t.clone()))
    }

    #[test]
    fn requests_are_answered_in_order_and_waits_come_back() {
        let t = Recorder::default();
        t.push(7, r#"{"id":1,"cmd":"ping"}"#);
        t.push(7, r#"{"id":2,"cmd":"wait"}"#);
        let mut ch = channel(&t);
        let mut h = Pinger::default();
        // The ping is answered; the wait holds the queue.
        assert_eq!(ch.service(&mut h), 1);
        assert_eq!(ch.waiting(), 1);
        assert_eq!(ch.front().map(|r| r.id.clone()), Some(json!(2)));
        assert_eq!(ch.front().map(|r| r.conn), Some(7));
        assert_eq!(ch.service(&mut h), 0);
        assert_eq!(ch.service(&mut h), 1);
        assert_eq!(ch.waiting(), 0);
        assert_eq!(h.pings, 1);
        let got = t.replies();
        assert_eq!(got.len(), 2);
        assert!(got[0].1.contains(r#""pong":true"#));
        assert!(got[1].1.contains(r#""id":2"#));
        assert_eq!(ch.answered(), 2);
    }

    #[test]
    fn a_line_that_is_not_a_request_is_refused_on_its_own_connection() {
        let t = Recorder::default();
        t.push(3, "garbage");
        let mut ch = channel(&t);
        ch.service(&mut Pinger::default());
        assert_eq!(ch.waiting(), 0);
        let got = t.replies();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 3);
        assert!(got[0].1.contains(r#""ok":false"#));
    }
}
