# cod4-decomp

A GPL-3.0-or-later Rust reimplementation of the Call of Duty 4: Modern Warfare (1.7) multiplayer engine. It runs on macOS, Linux, and Windows, and includes a headless server. It contains no game content: you supply your own copy of the game.

Planning and decisions: [wayfinder map](https://github.com/jeiang/cod4-decomp/issues/1). Terms: [CONTEXT.md](CONTEXT.md). Decisions with lasting effect: [docs/adr](docs/adr).

## Build

```sh
nix develop -c cargo build --workspace
nix develop -c cargo test --workspace
```

Without Nix, install rustup; `rust-toolchain.toml` selects the toolchain.

## Original install

The engine reads a CoD4 1.7 install directly: `main/*.iwd`, `zone/<language>/*.ff`, and `localization.txt`. Set `COD4_PATH` to the install directory. The default is `./COD4`, which is gitignored. Never commit or upload anything from the install. Tests that need the install skip when it is missing.

## Harness

`cod4e-harness` runs the staged suite under a watchdog (each stage in a child process; crashes, hangs and panics are caught) and writes `cod4e-run-<date>.zip` (at most 100 MB, user and host names scrubbed) with `manifest.json`, `summary.json`, logs, CSVs, a Perfetto-readable `trace.json` and minidumps.

```sh
nix develop -c cargo run --release -p harness -- run --out bundles   # default suite
nix develop -c cargo run --release -p harness -- stages              # list stages
nix develop -c cargo run --release -p harness -- diff a.zip b.zip    # compare two bundles
```

Stages that need engine commands read console scripts (`scenarios/*.cfg`, or `--script file`); commands the engine lacks yet are reported as skipped. Double-clicking the program runs the suite with the zip next to it. The CI `windows` job uploads that portable folder and smoke-tests it with no install. `scripts/remote-linux.sh <ref> -- <run options>` runs it on the Linux test host (`COD4E_LINUX_HOST`) and copies `bundles/` back.

Stage 3 (`client-flythrough`) needs a GPU and a display: it runs the `cod4e` client (`$COD4E_CLIENT`, else next to the harness) on mp_crash once per display mode (native borderless, windowed 1080p and 720p, plus an exclusive run at another native-size refresh when the monitor has one), 12 s each (`COD4E_FLYTHROUGH_SECS`), and records one short video (a few MB), a screenshot and per-frame timings per mode. It also runs mp_crash uncapped at native size (`--present immediate` and `mailbox`) and two more stock maps (mp_bog, mp_crash_snow), so the bundle shows sun shadows, fog and the film/glow post effects on three maps. `frames.csv` carries GPU pass time from timestamp queries where the device has them (`gpu_ms`, a few frames behind the render call, matched by frame number). Without the client, an install or a display it is skipped.

## Headless server

```sh
COD4_PATH=/path/to/install nix develop -c cargo run --release -p server -- +set g_gametype war +exec server.cfg +map mp_crash
```

`cod4e-server` boots like the original's dedicated server: command-line `set`s, `default_mp.cfg`, the dedicated zone set, then `+exec` and `+map`. It runs the stock gametype scripts at 30 Hz (`sv_fps`) on one thread. Console commands include `bots N` (server-side test clients that pick a team and class and play: they roam a navigation mesh generated from the map's collision data, shoot enemies they see, and inject usercmds directly, with no netchan), `status` and `expect <stat> <min>`. Harness stage 2 (`headless-bots`, plus `headless-bots-sd` for Search and Destroy; every server stage fails over 512 MiB peak RSS) plays an 18-bot team deathmatch round on mp_crash to its time limit and the map rotation, and records `ticks.csv` (per-tick total and gsc/bot/client/game columns), RSS, map-load and navigation-generation time and match counters; `headless-bots-32` is the 32-player budget run.

### Browser clients (WebTransport)

`--webtransport [host:]port` (cvar `net_wt`, empty = off) also serves WebTransport next to UDP: a browser sends and receives the same datagrams as a UDP client (connect packets and netchan), peers named by their QUIC address. A datagram up to the session's maximum datagram size (capped at 1200 bytes, the browsers' limit) is one WebTransport datagram; a longer one, up to 8192 bytes, is one unidirectional stream carrying exactly those bytes, ended by FIN; both forms are accepted in both directions (`net::wt` module docs). Anyone may connect; bans, rate limits and the password apply to the connect packets as for UDP.

Without `--wt-cert cert.pem --wt-key key.pem` (cvars `net_wt_cert`, `net_wt_key`) the server makes an ECDSA P-256 certificate valid 13 days at each start and logs its SHA-256 (base64 and hex). With `--wt-info file.json` (`net_wt_info`) it writes what the page needs for `new WebTransport(url, { serverCertificateHashes: [{ algorithm: "sha-256", value }] })`: `{"url":"https://localhost:4433/","certHashSha256Base64":"..."}` (`localhost` when bound to every address, which includes IPv6; a loaded certificate has no hash field). The page decodes the base64 to the 32-byte `value`.

```sh
COD4_PATH=/path/to/install nix develop -c cargo run --release -p server -- --webtransport 4433 --wt-info wt.json +map mp_crash
```

## Playing

`cod4e` with no options starts in the stock main menu, drawn from the install's own menu assets (`ui_mp`/`common_mp` menus, fonts, localized strings and images; `src/ui`). Mouse and keyboard drive it like the original: Start New Server picks a map and gametype and starts a listen server with bots, Join Game takes a server, Options and Controls edit the dvars and binds, Esc opens the in-game menu. The 640x480 menu space is anchored to a 16:9 safe area at any window shape. `--ui-tour <dir>` opens the menus one by one with no world and saves a screenshot of each (harness stage `client-ui`); `--ui-script click=Start New Server,menu=createserver,click=Start,ingame,shot=a` drives them like a player.

In a match the stock HUD menus are drawn with the client's own owner-draw pieces (`src/ownerdraw.rs`): the magazine graphic and reserve, weapon name, grenade counts, the sprint meter, the low-health overlay with the original's pulse phases (`hud_healthOverlay_*`), the hold-breath, mantle and no-ammo hints, and the corner compass with the level's map image (`setMiniMap` and `northyaw`, `src/compass.rs`): it scrolls with the player and turns with the view, shows teammates and, for a couple of seconds, enemies that fire, and the full-screen map used while picking an airstrike location. The `hud_fade_*`, `hud_enable`, `compass*` and `cg_draw*` dvars apply. Health reaches the client in the player state, so the bar (`cg_drawHealth`) and overlay follow the server. Not done: use-trigger cursor hints (the server does not yet say what the crosshair is on), the d-pad action slots and the talker icons.

```sh
COD4_PATH=/path/to/install nix develop -c cargo run --release -p client -- --listen --bots 9   # solo: a team deathmatch against bots
COD4_PATH=/path/to/install nix develop -c cargo run --release -p client -- --connect host:28960 # a `cod4e-server` (set net_port; default 28960)
```

The client talks to the server over UDP (`net`: sequenced, fragmented netchan, delta-coded snapshots about 36 per second, up to 3 usercmds per packet with redundancy). `--listen` runs the same server inside the client process. The player's own movement is predicted with the very `sim::pm::run_usercmd` the server runs (other players are collision boxes from the snapshot; server corrections fade over 100 ms); other players are drawn 100 ms behind the server clock between two snapshots, and the server rewinds them to that moment when it judges a shot (`g_lagcomp`, at most 250 ms back). Keys, mouse, gamepad and the config file are the input layer's (`bind w +forward`). Esc releases the mouse (it never quits); a click takes it again. `COD4E_INPUT_DEBUG=1` prints one `input:` line per second to stderr: events per second from GCMouse and winit, which source feeds the look, the GCMouse devices and whether our handler is on each, window focus, pointer lock, and how many frames of motion the focus/lock gate threw away.

Stage 4 (`client-match`) is the real client with `--listen --autoplay`: a scripted player walks toward and shoots the bots for 90 seconds. It asserts connected and spawned, walked, saw the bots, prediction rarely corrected, the view model drawn, no script errors, shots fired and hits registered; it reports bandwidth, snapshot rate, frame times and the server's tick cost. It needs a display and skips without one. `net-loopback` and `net-match` cover the protocol and 4 clients with 12 bots without a display; `net-ui` plays the menus a person would (team, class) against Domination and checks the hud elements, objectives, configstrings, client dvars and print lines the stock scripts send (`net::ui` is the client-side contract); `net-killcam` has a bot kill the client and checks the stock killcam replays the killer's view from the server's state ring and ends.

The client draws what the original's cgame draws over the stock HUD menus (`src/hud`): the script hud elements (text, values, timers, materials, clocks, waypoints with an off-screen pointer, with their alignment, move, fade, scale and glow), the `iprintln` and kill-feed message windows (the stock `gamemessages`, `boldgamemessages` and `subtitles` menus place them), team-coloured kill lines with the weapon's kill icon, chat, the centre string, the scoreboard rows under the stock top bar (hold Tab; rows ask the server every 2 s), the killcam (the stock `killcam` menu is shown while the server replays a kill) and the name of the player a spectator follows. Harness stage `client-hud` plays a bot match, holds the scoreboard and waits for a kill, and checks what the HUD drew (`hud_draw` in `ui-script.json`; `--ui-script` also takes `scores=on|off` and waits `killcam|dead|intermission|feed`).

## Layout

| crate | role |
|---|---|
| `assets` | VFS, IWD, fastfile decoding, IWI, typed assets |
| `gsc` | GSC compiler and bytecode VM |
| `sim` | collision traces, player movement, weapons |
| `net` | netchan, snapshot delta codec, transport trait |
| `server` | match simulation and `cod4e-server`; no GPU, window, or audio dependencies |
| `sm3` | D3D9 SM3 bytecode to WGSL |
| `render` | wgpu renderer |
| `audio` | sound alias tables, realtime-safe mixer (`Mixer::fill`, AudioWorklet-ready), WAV/MP3 decoding, cpal output |
| `client` | `cod4e`, the player-facing client |
| `harness` | `cod4e-harness`, scenario runner and run bundles |
