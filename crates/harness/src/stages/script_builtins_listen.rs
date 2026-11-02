// SPDX-License-Identifier: GPL-3.0-only
//! The `script-builtins` stage's listen-client part: `line` and `print3d` run by the listen server must be drawn
//! by its own player (#409; `RB_DrawDebugLines`/`RB_DrawDebugStrings`, KisakCOD `gfx_d3d/rb_debug.cpp:158-226`).
//! The client is started with `--listen`, `developer 1` is set on its server and the `devline` hook runs a script
//! calling both builtins in front of the player; the client's `hud.stats.debug_lines`/`debug_strings` (what reached
//! the screen in the last frame) must then be at least one each.
use super::client_flythrough::locate_client;
use super::client_models::no_display;
use crate::control::Driven;
use crate::stage::StageCtx;
use serde_json::json;
use std::time::Duration;

/// Server frames the drawings are kept: 20 s at the stock 20 frames a second, far longer than the wait for them.
const FRAMES: u32 = 400;

/// Runs the check; `Ok` carries the stage's metrics. Without a client binary or a display the check cannot run and
/// says so in `listen_debug_draw_skipped`.
pub fn listen_debug_draw(ctx: &StageCtx) -> Result<Vec<(String, f64)>, String> {
    let skipped = || -> Result<Vec<(String, f64)>, String> {
        Ok(vec![("listen_debug_draw_skipped".to_owned(), 1.0)])
    };
    let Some(install) = ctx.install.as_deref() else {
        return skipped();
    };
    let Some(client) = locate_client() else {
        return skipped();
    };
    if no_display(&client, "script-builtins")
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return skipped();
    }
    let mut driven = Driven::start(
        &client,
        install,
        &ctx.dir.join("listen-debug-draw"),
        &["--listen", "--bots", "0", "--duration", "600"],
    )?;
    let c = &mut driven;
    let outcome = (|| -> Result<Vec<(String, f64)>, String> {
        let server = |c: &mut Driven, line: &str| {
            c.ask(json!({"cmd": "server", "op": "exec", "line": line}))
                .map(|_| ())
                .map_err(|e| format!("{line}: {e}"))
        };
        c.until(
            json!({"player.alive": true}),
            Duration::from_secs(150),
            "the player did not spawn",
        )?;
        let before = c.lookup("hud.stats.debug_lines")?;
        if before.as_u64() != Some(0) {
            return Err(format!(
                "hud.stats.debug_lines was {before} before any line() ran"
            ));
        }
        server(c, "set developer 1")?;
        server(c, &format!("devline human {FRAMES}"))?;
        c.until(
            json!({"hud.stats.debug_lines": {"ge": 1}, "hud.stats.debug_strings": {"ge": 1}}),
            Duration::from_secs(15),
            "the listen client did not draw the line()/print3d() of its server",
        )?;
        let lines = c.lookup("hud.stats.debug_lines")?.as_f64().unwrap_or(0.0);
        let strings = c.lookup("hud.stats.debug_strings")?.as_f64().unwrap_or(0.0);
        c.capture("debug-draw")?;
        server(c, "set developer 0")?;
        Ok(vec![
            ("listen_debug_lines".to_owned(), lines),
            ("listen_debug_strings".to_owned(), strings),
        ])
    })();
    let status = driven.finish(Duration::from_secs(30));
    let metrics = outcome?;
    if !status.is_some_and(|s| s.success()) {
        return Err(format!(
            "the listen client did not exit successfully: {status:?}"
        ));
    }
    Ok(metrics)
}
