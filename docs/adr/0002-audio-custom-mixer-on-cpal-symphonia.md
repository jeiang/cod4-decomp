# Audio: a custom lock-free mixer on cpal and Symphonia, not kira

Sound is the `audio` crate: cpal for the device on desktop (CoreAudio, ALSA, WASAPI) and an embedded AudioWorklet in the browser (cpal is not built there), Symphonia (MPL-2.0, dependency use only) for WAV and MP3, and our own mixer. The mixer is one function, `Mixer::fill(&mut [f32])`, with no I/O and no threads: commands come in over a bounded lock-free queue, streamed audio over per-voice SPSC rings, counters leave through atomics, and finished voices are handed back to the game thread so the callback never allocates or frees (`tests/realtime.rs` counts allocations). In a browser (`wasm32`) the same mixer feeds an AudioWorklet through a ring (`transport.rs`, `device/web.rs`, `device/worklet.js`).

## Why not kira

kira covers tracks, reverb, EQ and tweens, but its spatial model is a pan plus an easing curve. The original's behavior is data in the aliases and needs game-side logic either way: per-alias `.vfcurve` knot curves, speaker maps, pools of 8 2D, 32 3D and 13 streamed voices with a per-channel cap and priority replacement, one sound per entity on restricted channels, master/slave ducking. Wrapping all of that around kira would leave kira doing resampling and mixing only, and a heavier dependency in the browser build.

## Facts resolved from `iw3mp.exe` and the install

- `snd_alias_t.flags`: bit 0 loop, 1 master, 2 slave, 3 full dry level, 4 no wet level, 5 random loop, bits 6-7 file kind (1 loaded, 2 streamed), bits 8-13 entity channel. Checked on every alias of `localized_common_mp` (8,076 aliases): the kind bits agree with the sound file's kind in all of them and every channel index is a row of `channels.def` whose name fits the alias (bullet impacts on `bulletimpact`, `Land_*` on `body`, ambients on `ambient`, `mp_defeat` on `music`).
- `soundaliases/channels.def` is a rawfile of `code_post_gfx_mp` (33 rows: name, priority, 2d/3d, restricted, pausable, max voices). It is read at run time, not copied into the source.
- Voice pools (`MSS_Init`): 8 2D, 32 3D, 13 streams, from the decompile. Miles rolloff is off (`set_3D_rolloff_factor(0)`); distance gain is the alias's own curve between `dist_min` and `dist_max`, computed by the game.
- Speaker maps: `channel_maps[stereo source][multichannel output]` holds, per output speaker (0 FL, 1 FR, 2 C, 3 LFE, 4 SL, 5 SR), the gain of each source channel. Positioned sounds ignore them; 2D sounds use them (a mono 2D sound is 0.5 per side).
- Alias choice: weighted by probability with a per-alias anti-repeat sequence, as the original's picker. A secondary alias plays together with its parent (chain of up to 10).
- The 348 "zero-info" loaded sounds are `,null.wav` placeholders: deliberately silent.
- Occlusion: none in the original (distance and curve are the only world inputs).
- Reverb (`snd_setEnvironmentEffects`, GSC `setReverb`/`deactivateReverb`): two priorities (`snd_enveffectsprio_level` 1, `_shellshock` 2) over a base effect; the highest active one is heard and a change fades the wet level over its fade time. The room names (`generic`, `paddedcell`, `room`, ..., `psychotic`, 26 in all) index Miles EAX room types. The wet level applies to every voice except aliases flagged no-wet (bit 4). The dry level is accepted and **ignored**: `MSS_GetDryLevel` returns a constant 1. `msseax.flt` is closed source, so the reverb itself is ours: a Freeverb-style send bus tuned per room by the public EAX preset's decay time, damping and size (`reverb.rs`). Those numbers are an approximation, not Miles'.
- EQ (`snd_setEq`, ...): 2 EQs of 3 bands per entity channel, each a low-pass, high-pass, low shelf, high shelf or bell (RBJ biquads in `eq.rs`; gain taken as decibels, which is an inference). Stock MP scripts never set one; only the console commands do, so the mixer supports it (`Sound::set_eq`) and no game path drives it.
- Channel volumes (`setChannelVolumes`/`deactivateChannelVolumes`, shell shock ducking): a base group plus priorities 1 (hold breath), 2 (pain) and 3 (shell shock) of one volume per entity channel, as `SND_SetChannelVolumes`. The highest active group sets every voice's channel gain; setting one fades from the volume in force over its fade time, clearing it fades back to the next lower active group. The 64-float goal list travels inline in the mixer command (a box would be freed on the mixer thread). A shell shock also fades in its room reverb at priority 2, rings its tinnitus loops and plays end stings (`crates/client/src/look.rs`).
- Stereo output only; 4 and 5.1 speaker maps are read but unused.

## Consequences

- The game's alias model never leaks library types; swapping the decoder or device is local.
- Reverb is a send bus inside `Mixer::fill` (fixed buffers, no allocation) and the EQ a per-voice biquad chain; both run in the same callback an AudioWorklet would run.
- No occlusion: the original has none.
- Browser output: the mixer runs on the page's main thread (no wasm threads), a 10 ms timer keeps 80 ms rendered ahead of the worklet, which reads a `SharedArrayBuffer` ring when cross-origin isolated and posted chunks otherwise. The embedded worklet loads from a Blob URL, so nothing is served for it. Streamed files have no decoder thread there: the owner calls `Sound::pump` once per frame and each job decodes a bounded slice. Headless Chrome (48 kHz): no underruns in steady state, and a main-thread stall of 70 ms is survived; 100 ms is not.
