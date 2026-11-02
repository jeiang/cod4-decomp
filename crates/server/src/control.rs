// SPDX-License-Identifier: GPL-3.0-only
//! The headless server's dev control channel: line JSON over an optional loopback TCP listener.
//!
//! The channel is serviced by the simulation thread between console commands and ticks. `wait` remains at the
//! front of [`devctrl::Channel`]'s queue until its expectation matches or its wall-clock timeout expires; it never
//! sleeps or moves the server onto another thread.

use crate::server::Server;
use devctrl::{Channel, Handler, Outcome, Reply, Request, lookup, reply};
use serde_json::{Map, Value, json};
use std::time::{Duration, Instant};

impl Server {
    /// Attaches the control channel opened by the dedicated server's command line.
    pub fn attach_control(&mut self, channel: Channel) {
        self.control = Some(channel);
    }

    /// Services queued control requests on the simulation thread.
    ///
    /// Taking the channel out for the call lets the handler borrow the whole server. In particular, there is no
    /// mutex around `Server`, and a nested `run_frames` from `control_request` does not recursively service it.
    pub(crate) fn service_control(&mut self) {
        let Some(mut channel) = self.control.take() else {
            return;
        };
        channel.service(self);
        self.control = Some(channel);
    }

    /// Answers one listen-server request against the running simulation.
    ///
    /// The client sends these through its own `server` control request. Keeping this body on `Server` lets the
    /// dedicated and listen servers share state, console and cvar behavior, including the command-frame fixture.
    pub fn control_request(&mut self, req: &Map<String, Value>) -> Result<Value, String> {
        let op = req.get("op").and_then(Value::as_str).unwrap_or("state");
        self.control_op(op, req)
    }

    /// The shared request body, with the operation name supplied by either wire protocol.
    fn control_op(&mut self, op: &str, req: &Map<String, Value>) -> Result<Value, String> {
        match op {
            "state" => {
                let what = req.get("what").and_then(Value::as_str);
                Ok(self.control_state(what))
            }
            "exec" => {
                let line = req
                    .get("line")
                    .and_then(Value::as_str)
                    .ok_or("exec needs the line")?;
                self.exec_line(line)?;
                Ok(json!({"ok": true, "line": line}))
            }
            "cvar" => {
                let name = req
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("cvar needs a name")?;
                match req.get("value").and_then(Value::as_str) {
                    None => Ok(json!({"name": name, "value": self.control_cvar(name)})),
                    // `"force": true` writes as the engine does (`devmap` sets the init-protected `sv_cheats` so).
                    Some(v) if req.get("force").and_then(Value::as_bool) == Some(true) => {
                        self.game.cvars.force(name, v);
                        Ok(json!({"name": name, "value": self.control_cvar(name)}))
                    }
                    Some(v) => {
                        self.exec_line(&format!("set {name} \"{}\"", v.replace('"', "'")))?;
                        Ok(json!({"name": name, "value": self.control_cvar(name)}))
                    }
                }
            }
            "command_frame" => {
                let ms = req.get("collect_ms").and_then(Value::as_u64).unwrap_or(0);
                // 24 usercmds per tick (`MAX_CMDS_PER_FRAME`) at the 125 Hz send cap: about 190 ms fits one tick.
                if ms > 190 {
                    return Err("command_frame collect_ms must be at most 190".to_owned());
                }
                Ok(self.control_command_frame(Duration::from_millis(ms)))
            }
            other => Err(format!("no such server request: {other}")),
        }
    }

    /// Answers a dedicated-server request that completes immediately.
    fn control_answer(&mut self, req: &Request) -> Result<Value, String> {
        match req.cmd.as_str() {
            "ping" => Ok(json!({"server": "cod4e-server", "ticks": self.ticks})),
            "state" => {
                let what = req
                    .str("what")
                    .or_else(|| req.str("part"))
                    .or_else(|| req.str("of"));
                let state = self.control_state(what);
                Ok(match what {
                    // `Driven::state` takes the named section out of the reply.
                    Some(name) => json!({(name): state}),
                    None => state,
                })
            }
            "cvars" => {
                let all = self.control_state(Some("cvars"));
                match req.str("filter") {
                    None => Ok(all),
                    Some(filter) => {
                        let filter = filter.to_ascii_lowercase();
                        let mut out = Map::new();
                        if let Value::Object(vars) = all {
                            for (name, value) in vars {
                                if name.to_ascii_lowercase().contains(&filter) {
                                    out.insert(name, value);
                                }
                            }
                        }
                        Ok(Value::Object(out))
                    }
                }
            }
            "cvar" | "exec" => self.control_op(req.cmd.as_str(), &req.args),
            "quit" => {
                self.quit = true;
                Ok(json!({"quitting": true}))
            }
            other => Err(format!("no such command: {other}")),
        }
    }

    /// Seconds since request `seq` was first offered, starting its clock on the first call.
    fn control_waited(&mut self, seq: u64) -> f64 {
        self.control_waits
            .entry(seq)
            .or_insert_with(Instant::now)
            .elapsed()
            .as_secs_f64()
    }

    /// Holds a wait at the front of the channel until the state matches or its timeout expires.
    fn control_wait(&mut self, req: &Request) -> Outcome {
        let Some(spec) = expectation(req) else {
            return Outcome::Done(reply(
                &req.id,
                Err("wait needs the state to wait for".to_owned()),
            ));
        };
        let timeout_ms = req.number("timeout_ms").unwrap_or(5000.0);
        let doc = self.control_state(None);
        match devctrl::matches(&doc, spec) {
            Ok(()) => {
                let waited_ms = (self.control_waited(req.seq) * 1000.0).round() as u64;
                let seen = observed(&doc, spec);
                self.control_waits.remove(&req.seq);
                Outcome::Done(reply(
                    &req.id,
                    Ok(json!({"waited_ms": waited_ms, "observed": seen})),
                ))
            }
            Err(mismatches) => {
                if self.control_waited(req.seq) * 1000.0 < timeout_ms {
                    return Outcome::Wait;
                }
                let seen = observed(&doc, spec);
                self.control_waits.remove(&req.seq);
                Outcome::Done(
                    Reply::failed(
                        req.id.clone(),
                        format!("timed out after {timeout_ms} ms: {}", mismatches.join("; ")),
                    )
                    .set("timeout", true)
                    .set("observed", seen),
                )
            }
        }
    }

    /// Checks an assertion against the state now, without holding the channel's queue.
    fn control_assert(&mut self, req: &Request) -> Reply {
        let Some(spec) = expectation(req) else {
            return Reply::failed(req.id.clone(), "assert needs the expected state");
        };
        let doc = self.control_state(None);
        match devctrl::matches(&doc, spec) {
            Ok(()) => Reply::ok(req.id.clone()).set("observed", observed(&doc, spec)),
            Err(mismatches) => Reply::failed(req.id.clone(), mismatches.join("; "))
                .set("observed", observed(&doc, spec))
                .set("mismatches", mismatches),
        }
    }
}

impl Handler for Server {
    fn handle(&mut self, req: &Request) -> Outcome {
        match req.cmd.as_str() {
            "wait" => self.control_wait(req),
            "assert" => Outcome::Done(self.control_assert(req)),
            _ => Outcome::Done(reply(&req.id, self.control_answer(req))),
        }
    }
}

/// The state a `wait` or an `assert` expects.
fn expectation(req: &Request) -> Option<&Value> {
    req.value("state")
        .or_else(|| req.value("until"))
        .or_else(|| req.value("expect"))
}

/// The values of the expected paths, for a reply that says what was seen as well as what was wanted.
fn observed(doc: &Value, spec: &Value) -> Value {
    let Value::Object(want) = spec else {
        return json!({});
    };
    let mut out = Map::new();
    for path in want.keys() {
        out.insert(
            path.clone(),
            lookup(doc, path).cloned().unwrap_or(Value::Null),
        );
    }
    Value::Object(out)
}
