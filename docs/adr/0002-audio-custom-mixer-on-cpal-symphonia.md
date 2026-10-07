# Audio: a custom lock-free mixer on cpal and Symphonia, not kira

Sound is the `audio` crate: cpal for the device (CoreAudio, ALSA, WASAPI, Web Audio), Symphonia (MPL-2.0, dependency use only) for WAV and MP3, and our own mixer. The mixer is one function, `Mixer::fill(&mut [f32])`, with no I/O and no threads: commands come in over a bounded lock-free queue, streamed audio over per-voice SPSC rings, counters leave through atomics, and finished voices are handed back to the game thread so the callback never allocates or frees (`tests/realtime.rs` counts allocations). The same code can run in a browser AudioWorklet.

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
- Reverb room table: the 26 `setReverb` room names (`generic`, `paddedcell`, `room`, ..., `psychotic`) index Miles EAX room types. Reverb itself is not implemented: `msseax.flt` is closed source. Stereo output only; 4 and 5.1 speaker maps are read but unused.

## Consequences

- The game's alias model never leaks library types; swapping the decoder or device is local.
- No reverb, EQ or occlusion for now; adding reverb is a send bus in `Mixer::fill`.
