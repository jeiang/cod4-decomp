# The dev control channel

A running client (and, for the server's half, the listen server it started) answers one line of JSON per request on a
socket, so an agent can drive the game and read its state without a screen recorder or an OS-level tool. It is off
unless `--control` asks for it; a play run never opens one.

## Starting it

```sh
cod4e --install ./COD4 --listen --bots 0 --control 0 --control-file /tmp/control.addr --duration 300
```

`--control <port>` binds loopback (`127.0.0.1`); `0` takes a free port and writes `host:port` to `--control-file`.
`--control-stdio` speaks the same protocol on stdin/stdout instead (one request per line). Outside the harness the
channel is a development tool: it injects input and reads state, so it stays behind the flag.

## Requests

Every request is an object with an optional `id` (echoed back) and a `cmd`:

| `cmd` | what it does |
|---|---|
| `ping` | the client's name and frames drawn |
| `state [what]` | the state document, or one of its sections: `client menu player entities scoreboard hud draw render sound fx events net console cvars binds view look server watch` (`events`: `world` by kind, `own` by `sim::pm::ev` number; `menu.items` carry each item's `name`, shown `text`, `visible` and `rect`); `report`, `gfx` and `cgame`, named only, are the match's counters and the graphics and cgame facts (below) |
| `cvar name [value]` | read a variable, or set it |
| `cvars [filter]` | every variable, or those whose name contains `filter` |
| `binds command` | the keys bound to a command |
| `key {name\|bind} [action]` | inject a key: `name` is a physical key (`w`, `escape`, `mouse1`), `bind` a command; `action` is `tap` (default), `down` or `up` |
| `text text` | type into the console's field (the client must have it open) |
| `mouse {x,y}` / `{dx,dy}` / `button` / `wheel` | move the pointer, look, click, scroll |
| `exec line` | run a console line as Enter does |
| `dev {op, arg}` | run one of the client's dev hooks (below): scenario setup no key or menu of the player's reaches |
| `viewpos {origin,angles}` | place the camera (`setviewpos`) |
| `capture [dir] [base64] [after]` | save the frame on screen as a PNG, after it is drawn; `"after":"distortion"` saves the first frame from now that copied the scene for a distortion material, or fails after 10 s |
| `wait {state, timeout_ms}` | answer when the state document matches (or the clock runs out: `ok` false with `observed`) |
| `assert {state}` | one look: `ok` false unless the state matches now |
| `step frames` | answer after that many frames |
| `server {op}` | the listen server: `state`, `exec`, `cvar`, `command_frame` (a client started with `--connect` runs no listen server and answers with an error, so the queue never waits on it) |
| `watch {number, model, from}` / `{clear}` | count one entity in every snapshot the client accepts from now on (with `from`, first the held snapshots numbered `from` or later); read back in the `watch` section |
| `quit` | end the run as `quit` does |

The injected events enter the same queue the window's events do (`crates/client/src/input/event.rs`), so a key
request goes through the console field, the menus and the bind table exactly as the player's key does.

A state predicate is a map from dotted path to expected value; an expectation is a literal, or `{"gt": 99}`,
`{"ge": 4}`, `{"contains": "word"}`, `{"exists": true}`, `{"len": 3}`. `wait` is what timing-sensitive checks use:
`{"cmd":"wait","state":{"entities.count":{"ge":4}},"timeout_ms":120000}` costs a frame, not a sleep.

The `menu` section lists each open menu's items with their `name`, the `text` they show (localized) and their `rect`,
so an agent can click a stock button by its label. The listen server's `clients` (`{"cmd":"server","op":"state",
"what":"clients"}`) carry each client's `weapons` (owned, by name) and the cheat commands' state (`noclip`, `ufo`,
`godmode`, `demigod`, `notarget`), which the `client-host-console` stage reads.

`server {"op":"command_frame","collect_ms":150}` collects actual UDP input for the requested wall-clock
interval (0–190 ms) without running simulation, then runs one normal server tick and returns its `clients`
state immediately. The cap follows the server's limits: one tick takes at most 24 usercmds of a client and
at most 64 wait queued, so at the client's 125 Hz send cap about 190 ms fits in one tick and none is dropped.
This development-only scheduling fixture lets `client-hud` prove latest-backlog selection without requiring
the GPU to outpace the server. It neither injects usercmds nor changes their selection; `collect_ms:0` ticks
without polling new traffic to check retained input. Ordinary listen-server scheduling is unchanged outside
this explicit request.

Paths a stage leans on: `menu.open` (the stack), `menu.items[]` (`menu`, `name`, `text` as shown, `visible`, and
`rect` `[x, y, w, h]` in window pixels, the rectangle a click is tested against: a static text item's is its text's),
`console.open` and `console.text` (the console's lines as one text, for `contains`), `player.alive|dead|origin|angles|
team|client|snapshot|teleport`, `entities.list[]` (`kind`, `origin`, `team`, `dead`, `client`, `actor`, `model`,
`model_name`), `watch` (`first`, `last`, `received`, `present`, `matched`, `breaks`), `hud.report` (what the HUD shows
now: `live`, `health`, `frag`, `second`, `overlay_alpha`, `unresolved_text`), `hud.stats` (what it drew over the run:
`messages`, `chat`, `window_lines`, `scoreboard_frames`, `killcam_frames`, `names_max`, `unresolved`, ...),
`hud.live` (`killcam`, `dead`, `intermission`, `following`, `feed`, `players_listed`), `hud.scoreboard_shown`, and
`net.spawned` with `net.view` (`death_view_frames`, `death_view_range_max`, `dof_frames`), and `look` (`naked`,
`night`, `goggles`, `night_vision`, `channel`, `blend` with `ms`, `missing`, `applied`, the last goggles `switch`).

`client.gpu_health` is what the graphics device went through (`device_losses`, `errors`) and `client.load` what the
last map load measured until the player spawned (`map`, `load_ms`, `to_spawn_ms`, `frames`, `max_frame_gap_ms`,
`slow_gaps_ms`), the same objects `ui-script.json` reports as `gpu_health` and `load`. A look at `gpu_health` leaves
the device's errors where they are, so a lost device's own noise, which may show between the loss and the recovery,
is still dropped when the client records the loss. `gfx` is what the graphics
settings came to in the renderer and on the surface, by the names of the `--ui-script` step `gfxis=` (`aa`, `aa_max`,
`specular`, `dof`, `blur`, `blur_radius`, `blur_shape`, `glow`, `shadows`, `aniso`, `mipmode`, `dlights`, `aspect`,
`fullscreen`, `uncapped`, `zfeather`, `nearfar`, `depthhack_near`, `outdoor`, `sun_tweak`, `film_tweak`,
`glow_tweak`, `sun_flare`, `sun_fx_position`, `water`, `picmip`, `pace`), and `cgame` what the cgame drew last
frame, by the names of `cgis=` (`gun`, `hud2d`, `fps`, `fpstext`, `fullbright`, `shock`, `centertime`,
`heli_camera`, `air_camera`, `brightness`, `film_sample`, `crosshair_alpha`, `window_position`, `levelfade`,
`eyebob`, `subtitle`). Each fact is the step's own text (numbers separated by spaces), `null` where it needs what is
not up (a world renderer, a match). Like `report`, both are built only when named (`state gfx`): a bare `state`, a
`wait` and an `assert` leave them out rather than query the surface and the adapter every frame, so a stage reads
them. Effect counts are `fx.played.<kind>`.

`report` is what the match saw so far, the `net` object of `client.json` and `ui-script.json`: the run's counters,
such as `vehicles_max_drawn` and `vehicles_unloaded`, `projectiles_max_drawn`, `missiles_seen` (by weapon name),
`brush_models_seen` and `brush_models_max`, `spawned`, `players_seen` and `players_drawn`. It is built only when
named (`state report`): a bare `state`, a `wait` and an `assert` leave it out, so a stage polls it.

The dev hooks of `dev` are the `--ui-script` steps of the same name, for what a scenario sets up that the player has
no key for: `losedevice` (destroys the graphics device, as a driver reset does), `dlight` (`arg` `1` puts a bright
test light in front of the camera, `0` removes it), `pacereset` (counts frames from now for `gfx.pace`), and in a
match `fxdemo` (`arg` an effect played in front of the player every 1.5 s, `off`), `bobmark` (starts `cgame.eyebob`
over), `hitflesh` (a bullet of the weapon in hand strikes a player 64 units ahead), `shock` (`arg` ms of the default
shellshock) and `subtitle` (`arg` a string key shown as a started sound's subtitle).

`entities.list[].model_name` resolves each snapshot model index through the server's model configstrings.
`entities.snapshot` and `entities.server_time` name the snapshot the list came from, so a check that compares
snapshots reads the list and its snapshot in one request.
`player.snapshot` is the newest snapshot's number. `player.teleport` (`before`, `after`) names the newest pair of
snapshots, accepted one after the other, whose player teleport bits differ: the server flips the bit when it moves
the player (a spawn, `setviewpos`), so `after` is the first snapshot built since the move, where a `watch` can start
(`from`). It is `null` until the bit first flips on the level, and a new level forgets it.
`watch` follows one entity (`number`, and optionally the model index `model` it must carry, as `entities.list[].model`)
through every snapshot the client accepts, where reading `entities` now and then would miss one that drops it
between two readings. The `watch` section counts them: `first` and `last` (the snapshot numbers counted),
`received`, `present` (carried the entity) and `matched` (with the model asked for), `missed` and `breaks` (snapshot
numbers skipped, and how often), and the first 16 `gaps` (`[before, after]`), `absent` snapshots and `wrong_model`
(`[snapshot, model]`). The server numbers a client's snapshots one by one, so `breaks` 0 means none was lost or
turned away. `{"cmd":"watch","clear":true}` stops it; the section is `null` while nothing is followed.
For same-camera rendering comparisons, the diagnostic console commands `drawscriptmodels 0` and `drawprops 0`
hide those two classes independently; `1` restores them, as with `drawvehicles`.

```sh
printf '%s\n' '{"id":1,"cmd":"state","what":"menu"}' | nc -q1 127.0.0.1 18095
```

## Transports

The channel is a queue of requests plus a `Transport` (`crates/devctrl`): `TcpTransport` (loopback), `StdioTransport`
and the browser's `PageTransport` (`crates/client/src/web.rs`), so a transport plugs in without changing a request.
A request whose handler cannot answer yet (a `wait`, a capture between frames, a step with frames left, a listen
server's reply) stays at the head of the queue and is offered again on the next frame.

### The browser

The page is the wire when it is opened with `?control`: `web/serve.py` keeps a request queue, the harness `POST`s one
request line to `/control` and gets the reply line as the response (504 after 30 s without one, so a long `wait` is
asked for in pieces), the page long-polls `GET /control/next` and hands each line to the module between frames
(`cod4.control_next`), and the module's reply goes back through `cod4.control_reply` to `POST /control/reply`, paired
by `id`. The requests and the handler are the native ones; only `capture` differs: the page reads the canvas (the
module has no file to write and WebGPU maps buffers asynchronously), so the reply carries `png_base64`, `width`,
`height` and `lit_fraction` and no `path`. Without `?control` nothing polls and the module opens no channel. `server`
requests answer with an error: the browser hosts no server.

## The stage

`control-channel` in the harness runs the acceptance of #378: console opened with the player's key, `bots 3` typed in,
four players waited for, the entity list and scoreboard read, a menu item clicked at the centre of the rectangle the
state document gave for it, and a capture written with no visible window.

`web-match` runs the browser client through the same requests over the page transport: menus clicked, movement, a
mouse delta after a trusted click grants pointer lock, fire, grenade, HUD, sound, scoreboard, killcam, map change,
Leave Game and the no-hosting popup, each asserted on the state document (see the README's Browser section). It then
runs the native client on the same map, gametype and bot count and records the measured movement, shot and HUD
ownerdraw differences.
`crates/harness/src/control.rs` is the harness's side: `Driven` starts a client with `--control 0`, asks, waits,
presses binds and keys, types, clicks a menu item by name or text where the state document put it, and captures;
`Walker` is a stage's own player in a match, turning toward the nearest living enemy with the mouse (or the arrow
keys when the pointer could not be locked), walking with the forward key and firing with the attack button. The
match stages move onto it one by one (#376): `client-hud` plays its whole match through the channel.

Mouse look needs what a person's does: the window focused and the pointer locked (`client.focused`,
`client.pointer_locked`, and `client.look_gate` names what drops the motion). A headless wlroots backend supplies
an output but no keyboard/pointer, so `scripts/virtual-display.sh` keeps `headless-seat.py` alive on the private
display. It creates virtual keyboard/pointer devices through the compositor's protocols and supplies an XKB keymap;
startup fails if the seat does not advertise both capabilities. No physical device, `/dev/uinput`, or elevated
privilege is used, and no key or motion is sent by that helper. The client's own channel still drives its input.
The helper's connection closes when the display wrapper exits, destroying its devices.

`client-hud` exercises both paths: it first tests the key-only fallback against a fixed bearing (snapshot aim must
settle within four degrees and firing must consume magazine ammunition), restores the ordinary key speeds, and then
uses the mouse-driven walker in the match. Key-only turning uses slower configurable `cl_yawspeed`/`cl_pitchspeed`
and feedback pulses: each channel request crosses a rendered frame, so the original 140-degree/second keys would
overshoot a small aiming error before the key-up even arrived. The stage's notes say which match path ran and what
the client reported about focus and pointer lock.
