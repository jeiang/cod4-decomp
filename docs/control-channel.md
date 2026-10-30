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
| `state [what]` | the state document, or one of its sections: `client menu player entities scoreboard hud draw sound fx net console cvars binds view server` |
| `cvar name [value]` | read a variable, or set it |
| `cvars [filter]` | every variable, or those whose name contains `filter` |
| `binds command` | the keys bound to a command |
| `key {name\|bind} [action]` | inject a key: `name` is a physical key (`w`, `escape`, `mouse1`), `bind` a command; `action` is `tap` (default), `down` or `up` |
| `text text` | type into the console's field (the client must have it open) |
| `mouse {x,y}` / `{dx,dy}` / `button` / `wheel` | move the pointer, look, click, scroll |
| `exec line` | run a console line as Enter does |
| `viewpos {origin,angles}` | place the camera (`setviewpos`) |
| `capture [dir] [base64]` | save the frame on screen as a PNG, after it is drawn |
| `wait {state, timeout_ms}` | answer when the state document matches (or the clock runs out: `ok` false with `observed`) |
| `assert {state}` | one look: `ok` false unless the state matches now |
| `step frames` | answer after that many frames |
| `server {op}` | the listen server: `state`, `exec`, `cvar` (a client started with `--connect` runs no listen server and answers with an error, so the queue never waits on it) |
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
Paths a stage leans on: `menu.open` (the stack), `menu.items[]` (`menu`, `name`, `text` as shown, `visible`, and
`rect` `[x, y, w, h]` in window pixels, the rectangle a click is tested against: a static text item's is its text's),
`console.open` and `console.text` (the console's lines as one text, for `contains`), `player.alive|dead|origin|angles|
team|client`, `entities.list[]` (`kind`, `origin`, `team`, `dead`, `client`, `actor`), `hud.report` (what the HUD shows
now: `live`, `health`, `frag`, `second`, `overlay_alpha`, `unresolved_text`), `hud.stats` (what it drew over the run:
`messages`, `chat`, `window_lines`, `scoreboard_frames`, `killcam_frames`, `names_max`, `unresolved`, ...),
`hud.live` (`killcam`, `dead`, `intermission`, `following`, `feed`, `players_listed`), `hud.scoreboard_shown`, and
`net.spawned` with `net.view` (`death_view_frames`, `death_view_range_max`, `dof_frames`).

```sh
printf '%s\n' '{"id":1,"cmd":"state","what":"menu"}' | nc -q1 127.0.0.1 18095
```

## Transports

The channel is a queue of requests plus a `Transport` (`crates/devctrl`): `TcpTransport` (loopback), `StdioTransport`
and `QueueTransport` (a page polls it, and replies by id), so a browser transport plugs in without changing a request.
A request whose handler cannot answer yet (a `wait`, a capture between frames, a step with frames left, a listen
server's reply) stays at the head of the queue and is offered again on the next frame.

## The stages

`control-channel` in the harness runs the acceptance of #378: console opened with the player's key, `bots 3` typed in,
four players waited for, the entity list and scoreboard read, a menu item clicked at the centre of the rectangle the
state document gave for it, and a capture written with no visible window.

`crates/harness/src/control.rs` is the harness's side: `Driven` starts a client with `--control 0`, asks, waits,
presses binds and keys, types, clicks a menu item by name or text where the state document put it, and captures;
`Walker` is a stage's own player in a match, turning toward the nearest living enemy with the mouse (or the arrow
keys when the pointer could not be locked), walking with the forward key and firing with the attack button. The
match stages move onto it one by one (#376): `client-hud` plays its whole match through the channel.
