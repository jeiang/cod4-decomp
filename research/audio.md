# Audio playback: formats, alias features, Miles usage, Rust libraries

Ticket: jeiang/cod4-decomp#7. Original install: CoD4 1.7 (`iw3mp.exe`, Miles Sound System 7.0w).
Verified = checked against the real files in `COD4/`. Inference = labelled `[INFERENCE]`.
Nothing from the install is reproduced here beyond identifiers, counts and numeric parameters.

## 1. Sources and licenses

| Source | Used for | License | Reuse |
|---|---|---|---|
| Real install (`COD4/`) | all "verified" claims | proprietary | observation only; never committed |
| OpenAssetTools (Laupetin) `IW3_Assets.h`, `ZoneCode/Game/IW3/XAssets/*.txt`, `LoadedSoundDumperIW3.cpp` | IW3 struct layouts (re-checked against the files) | GPL-3.0 (GitHub API `license`) | GPL-3-compatible: code may be reused with attribution |
| Symphonia 0.6.1 | decoders | MPL-2.0 (crates.io) | usable as a dependency from a GPL-3 project; MPL file-level copyleft only |
| kira 0.12.5 | mixer | MIT OR Apache-2.0 | ok |
| cpal 0.18.2 | device output | Apache-2.0 | ok with GPLv3 |
| rodio 0.22.2, oddio 0.7.4, hound 3.5.1, fundsp 0.23, rubato 5.0.1, web-audio-api 1.7.0 (MIT), audionimbus 0.16 (MIT OR Apache-2.0), hrtf 0.9 (MIT), minimp3 0.6.1 (MIT), audio-codec-algorithms 0.8.1 (0BSD OR Apache-2.0) | evaluated | as listed (crates.io) | ok |
| CoD4X (AGPL) | not used | AGPL | fact source only; not consulted for code |
| codresearch.dev / zeroy wiki / forum pages | channel list `auto, menu, weapon, voice, item, body, local, music, announcer` only | unknown (community docs) | facts only; the channel list is unverified against `channels.def`, which is not shipped |

## 2. Where sound lives (verified)

Two storage routes, selected per alias file entry (`SoundFile.type`):

1. **Loaded** (type 1): the PCM payload is inside the fastfile, inline in the alias list. 
2. **Streamed** (type 2): the fastfile holds only `dir` + `name` strings. The audio file is read at play time from an IWD as `sound/<dir>/<name>`. (The engine strings also mention a third type "primed": `Unknown sound type '%s'; should be primed, streamed or loaded`. No primed entries were seen in the MP zones; [INFERENCE] it is unused in stock MP.)

Both fastfile route entries sit inside `snd_alias_list_t` assets (asset type 7 in the OAT IW3 enum). There is **no separate loaded_sound asset (type 9) in any shipped zone** (histogram of the asset tables of `mp_crossfire`, `mp_backlot`, `common_mp`, `localized_common_mp`). Loaded sounds appear only as inline `LoadedSound` blocks.

### Zone layout facts
- Each alias list in `mp_*.ff` map zones carries the map's ambient/FX aliases (about 131-158 lists, 310-340 aliases per map).
- `common_mp.ff` has no alias lists: its 116 weapon assets reference sound aliases by name only.
- `localized_common_mp.ff` holds the MP alias set: 2,769 sound assets (asset table), voice, weapons and UI. Parsed: 2,717 lists with 7,972 aliases.
- The same ~16.5 MB set of loaded PCM (about 35 sounds) is duplicated inside each map zone (map zones of identical content size: backlot, convoy, overgrown, pipeline, showdown, strike all have 16,549,596 bytes of loaded PCM). Dedup by name is needed if zones are merged.

## 3. Codec inventory (verified against the real files)

### 3.1 Loaded sounds in fastfiles
Each `LoadedSound` = `name`, `AILSOUNDINFO{format, data_ptr, data_len, rate, bits, channels, samples, block_size, initial_ptr}` (0x24 bytes) + data pointer (struct 0x2C bytes; matches OAT `IW3_Assets.h`).
Structural scan of every zone (72 `.ff`): every `format` value seen is **1 = PCM, 16 bit, little-endian, headerless** (the zone has no RIFF header; OAT's dumper also accepts only PCM and errors on anything else).
MP zones (pattern `AILSOUNDINFO` with data pointer == initial pointer, parsed through the alias lists):

| rate Hz | ch | loaded sounds (counts across all MP zones, duplicates included) |
|---|---|---|
| 44100 | 1 | 1093 |
| 48000 | 1 | 222 |
| 22050 | 1 | 192 |
| 44100 | 2 | 30 |
| 32000 | 1 | 5 |
| 11025 | 1 | 1 |

Per-zone totals (separate full-zone scan): 35-94 loaded PCM sounds per map zone (16.5-32.6 MB), `localized_common_mp` 702 sounds (69.2 MB). No ADPCM (bits 4) or 8-bit entries found; no MP3 in fastfiles (the `ID3` byte hits are random matches: no `RIFF`/`WAVE`/`fmt ` anywhere in the decompressed zones).
`format 0` with zero info appeared 348 times in my parse: [INFERENCE] duplicate/back-referenced entries or stubs whose payload comes from another block; unresolved.

### 3.2 IWD `sound/` entries (zip, deflate)
All `sound/**` files in `main/*.iwd` (17,112 files): 16,993 `.wav` + 119 `.mp3`; nothing else (no ogg, no `.flac`).

WAV, full RIFF chunk walk of all 16,993:
- Every file is `fmt` tag **1 (PCM), 16 bit**. No ADPCM / Miles ADPCM / float.
- fmt chunk size 16 or 18; extra chunks before `data`: `bext` (2,157 files, BWF), `minf` (2,155), `elm1` (2,132), `elmo` (23), `JUNK` (9). `fact`, `cue `, `smpl` are absent: **loop points are not in the files**; looping is alias metadata.
- Format mix (files): 22.05 kHz mono 16,546; 44.1 kHz mono 268+7; 44.1 kHz stereo 54+1; 32 kHz stereo 48; 22.05 kHz stereo 43; 32 kHz mono 11; 48 kHz mono 9+1; 11.025 kHz mono 1.
- Bytes by format: 22.05 kHz mono ≈ 1.15 GB (voice/battlechatter, localized IWDs), 44.1 kHz mono 162 MB, 44.1 kHz stereo 132 MB.

MP3 (119 files, 437.6 MB): all MPEG Layer III, joint stereo, no Xing/Info/VBRI header (CBR):
- MPEG-1 44.1 kHz: 128 kbps ×60 (music, ID3v2), 192 ×19, 160 ×10, 256 ×1.
- MPEG-2 (LSF) 22.05 kHz: 112 ×11, 128 ×7, 144 ×6, 96 ×5.
- Ambient set is shipped as `*_lr.mp3` + `*_lsrs.mp3` pairs (front L/R and surround L/R stereo files, names from `sound/ambient/`): [INFERENCE] each is one stereo pair of a 4-channel ambience.
- `mssmp3.asi` in `COD4/miles/` is the Miles MP3 decoder plugin; `mssvoice.asi` is Miles' voice codec (present, no use found in MP content).

Cross-check: 1,706 distinct streamed file names referenced by MP zones resolve (by file name) to an IWD entry: 1,683 wav + 23 mp3, 0 misses. Streamed alias dirs are mostly stored as pointer back-references, so only the file name resolves cheaply.

### 3.3 Decoder smoke test (real files)
Throwaway Symphonia 0.6.1 program (`default-features=false, features = mp3, wav, pcm`) decoded **269/269** files with no error: all 119 MP3 and 150 random WAVs (including `bext/minf/elm1` ones and 18-byte fmt chunks). 22.05 kHz mono voice, 44.1 kHz music and MPEG-2 LSF all decode.

## 4. Sound alias data model (verified)

`snd_alias_list_t { name, head, count }` then `count` × `snd_alias_t` (0x5C bytes, 32-bit). The IW3 layout in OAT matches: parsing sequentially reproduced the exact start of the next list in 100% of ~13k lists checked (no failures across MP zones). Serialization order: record array, then per alias: strings (name/subtitle/secondary/chain), `SoundFile` (12 bytes; streamed adds `dir`,`name` strings; loaded adds a 0x2C `LoadedSound` + name + data), then `SndCurve` (72 bytes, inline on first use, later a pointer), then `SpeakerMap` (inline 0x198+8 bytes on first use, later a pointer).

| Field | Meaning / evidence |
|---|---|
| `aliasName`, `subtitle`, `secondaryAliasName`, `chainAliasName` | strings; secondary non-empty on 1,495 aliases; chain and subtitle empty throughout MP; engine errors mention secondary alias recursion and "looping alias cannot have a looping secondary" |
| `soundFile` | type 1 loaded / 2 streamed; shared by pointer across aliases |
| `sequence` | zero throughout MP; `SND_GetAliasWithOffset` exists in the exe |
| `volMin/volMax` | 0..1 floats (random range per play) |
| `pitchMin/pitchMax` | e.g. 0.8-1.2 |
| `distMin/distMax` | game units. Most common pairs: (50, 3200), (50, 1000), (300, 7800), (7, 750), (360, 600), (60, 120); (100000, 500000) used 792 times for "heard everywhere" |
| `flags` | packed int; see below |
| `slavePercentage` | master/slave ducking amount (CSV `masterslave`); present on every alias |
| `probability` | variant weight, 1.0 for 13,633 of 14,670 aliases; others 0.1-3.0 |
| `lfePercentage`, `centerPercentage` | 5.1 mix; lfe non-zero on ~3.8k, center non-zero on 896 |
| `startDelay` | non-zero on 2,141 aliases |
| `volumeFalloffCurve` | `SndCurve{filename, knotCount, knots[8][2]}`; piecewise-linear (x = distance fraction 0..1, y = gain). Named curves seen: `weapon1`, `weapon2`, `weapon4`, `reaction`, `helicopter1`, `weapon_grenadebounce`; many aliases use a 2-knot linear (1→0) curve, 80 have 0 knots. Engine reads them from `soundaliases/<name>.vfcurve` when building |
| `envelopMin/Max/Percentage` | all zero in MP (unused) |
| `speakerMap` | `SpeakerMap{isDefault, name, MSSChannelMap channelMaps[2][2]}`; each map has up to 6 speakers × (speaker id, numLevels, levels[2]). Names seen: `amb_front`, `radio`, `ambience`, default; engine file `soundaliases/<name>.spkrmap`, hot reload `snd_refreshSpeakerMaps` |

CSV columns recovered from `iw3mp.exe` strings (order as stored): `name, sequence, file, vol_min, vol_max, vol_mod, pitch_min, pitch_max, dist_min, dist_max, channel, type, loop, probability, loadspec, masterslave, secondaryaliasname, chainaliasname, volumefalloffcurve, startdelay, speakermap, reverb(fulldrylevel/nowetlevel), lfe percentage, center percentage, envelop_min, envelop_max, envelop percentage`. `loadspec` is a build-time language selector (`'!'` prefix rule). `vol_mod` refers to `soundaliases/volumemodgroups.def` (runtime-refreshable, `snd_refreshVolumeModGroups`); channels come from `soundaliases/channels.def` with a per-channel max voice count. None of these `.csv/.def/.vfcurve/.spkrmap` files exist in the IWDs or zones: curves and speaker maps are compiled into the zones; `channels.def` and volume mod groups are not shipped. [INFERENCE] channel limits and volume mod groups must be reconstructed from the engine, which is a decompile task (see ticket #10).

### `flags` word (partly resolved)
Distinct values in MP zones: 0x1580 (1741 aliases), 0x444 (1729), 0x1544 (1178), 0xc40 (667), 0x1084 (648), 0x1f92 (640), 0xc44 (620), 0xb40 (575), 0x184 (567)... The word packs at least channel, loop, master/slave, fulldrylevel/nowetlevel, randomized pitch/volume and an error-if-missing bit. **The bit layout is not verified** (public sources disagree; later-IW layouts do not apply). It needs one decompile of the alias CSV loader. Open unknown.

## 5. Miles features the engine uses (verified: import list of `iw3mp.exe`, Miles 7.0w)

Imported `AIL_*` (51): driver `open_digital_driver, startup/shutdown, set_preference, set_DirectSound_HWND, digital_CPU_percent, set_speaker_configuration, speaker_configuration, find_filter/open_filter, set_sample_processor, sample_stage_property, process_digital_audio, size_processed_digital_audio`; samples `allocate/init/set_sample_info/stop/end/resume, sample_status, set_sample_ms_position, set_sample_playback_rate, set_sample_loop_count, sample_volume_pan/levels, set_sample_channel_levels, set_sample_volume_levels`; 3D `set_sample_3D_position, set_sample_3D_distances, set_3D_distance_factor, set_3D_rolloff_factor`; reverb `set_digital_master_reverb_levels, set_sample_reverb_levels, set_room_type`; streams `open_stream, close_stream, pause_stream, stream_info, stream_ms_position, set_stream_ms_position, set_stream_loop_count, stream_sample_handle, stream_status`; `AIL_WAV_info`, `set_file_callbacks` (the engine feeds IWD bytes to Miles).

Plugins shipped in `COD4/miles/`: `milesEq.flt` (EQ), `msseax.flt` (EAX reverb), `mssds3d.flt`, `mssdsp.flt`, `mssmp3.asi`, `mssvoice.asi`. Engine error `unable to load eq filter`.

Consequent features (strings + imports):
- **3D positioning**: per-sample 3D position + min/max distance; distance volume is the alias curve (`vfcurve`) or Miles rolloff; `snd_enable3D`, `snd_enable2D`, `snd_draw3D`, `snd_playLocal`. Speaker config 2/4/5.1 (`snd_outputConfiguration`) via speaker maps and per-channel levels (`MSSChannelMap`).
- **Occlusion**: no occlusion API imported, and no obstruction/occlusion strings in the exe. Distance + curve are the only world-attenuation inputs found. [INFERENCE] no geometric occlusion. (`lowpass` appears as a string, used by EQ/shellshock-style filtering.)
- **Reverb**: `snd_enableReverb`, `setReverb "priority" "roomtype" dry wet fade`, `deactivateReverb`, `snd_setEnvironmentEffects`, priorities `snd_enveffectsprio_level` and `snd_enveffectsprio_shellshock`, `snd_channelvolprio_holdbreath/pain/shellshock`; room types are Miles EAX room types. Per-alias `fulldrylevel`/`nowetlevel`.
- **EQ**: `snd_setEq <channel> <eqIndex> <band> <type> <gain> <freq> <q>` and variants, `snd_enableEq`, a `milesEq` filter per channel.
- **Ducking**: `masterslave` aliases with `slavePercentage` and `snd_slaveFadeTime`; channel-volume priorities (holdbreath/pain/shellshock) fade whole channel groups; `snd_levelFadeTime`; `volumemodgroups`.
- **Streaming music/ambience**: `ambientPlay(alias, fadetime)`, `ambientStop(fadetime)`; ambient/music aliases must be type `streamed` and channel `local`/music (engine warns otherwise); `snd_enableStream`, `snd_touchStreamFilesOnLoad`.
- **Other**: pitch (`set_sample_playback_rate`), loop counts, start delay, voice limits per channel with priorities, `snd_khz` (11/22/44 mixer rate), `snd_volume`, `snd_cinematicVolumeScale`, subtitle file `soundaliases/subtitle.st`. Bink videos use `_BinkOpenMiles`; video audio is out of scope for MP.

## 6. Rust library fit

Facts from crates.io metadata and the maintainers' repos.

| Need | cpal 0.18 | kira 0.12 | rodio 0.22 | Symphonia 0.6 | OAT/own code |
|---|---|---|---|---|---|
| Device output: mac (CoreAudio), Windows (WASAPI/ASIO), Linux (ALSA; Pulse/PipeWire/JACK optional), browser (Web Audio via `wasm-bindgen`, optional AudioWorklet) | yes (README platform table) | via cpal; wasm target selects `cpal` with `wasm-bindgen` | via cpal | no | |
| PCM 16-bit / WAV with extra chunks | | static+streaming sounds via Symphonia | via Symphonia | yes (verified 150 real files) | PCM is trivial: raw i16 to f32 |
| MP3 (MPEG-1/2 L3, CBR, ID3) | | feature `mp3` | yes | yes `mp3` (verified 119/119) | |
| Mixer tracks, send tracks, per-track effects (reverb, EQ filter, lowpass, delay, compressor, panning, volume) | | yes | partial | | |
| Spatial: listener position/orientation, spatial track with distance range + easing attenuation + `spatialization_strength` | | yes, but stereo panning only, no speaker maps, no custom knot curves | no | | |
| Tweens, clocks, streaming | | yes | | | |
| Custom effects/sounds | | `Effect`/`Sound` traits allow own DSP | `Source` trait | | |
| Headless server | no audio dependency needed | not linked | not linked | | server never loads audio (glossary: headless server has no audio) |

Limits to design around:
- kira's spatialization is a pan + attenuation; Miles-style 4/5.1 speaker maps, LFE/center percentages, per-alias `vfcurve` knots and per-channel voice limits/priority have no direct kira equivalent. They would be done in game code (per-play volume from the knot curve, speaker-map levels applied as gains onto a surround-capable track or downmixed to stereo).
- kira `StaticSoundData` decodes whole files; the 1.15 GB of 22.05 kHz voice WAV and the 437 MB of MP3 must stay on disk and stream (kira `streaming` sound or own decoder).
- cpal is stereo-or-more device I/O; 5.1 output depends on the OS device.
- Symphonia is MPL-2.0: dependency use is fine under GPL-3.0-or-later; modified Symphonia files must stay MPL.
- Web: cpal `wasm-bindgen` backend is documented in its README; the `audioworklet` backend needs nightly + atomics + COOP/COEP headers. Not exercised here (browser spike is a separate ticket).

### Recommendation
1. **Output: `cpal`** on all four targets (mac, linux, windows, wasm).
2. **Decode: `symphonia`** with features `mp3`, `wav`, `pcm` only. Fastfile loaded sounds need no decoder (raw i16 LE). No ADPCM/OGG/FLAC needed for stock content. Mods may bring ogg/other: enable `vorbis`,`ogg` later if wanted.
3. **Mixer: own thin mixer on cpal + symphonia is the faithful path** (alias curves, 5.1 speaker maps, master/slave ducking, channel voice limits, EQ and EAX-like reverb are all game-defined data). Use **kira** if the project prefers to get tracks, send-reverb, EQ/filter, tweens and a listener for free and accepts approximating speaker maps by pan. Decision between the two is open and should wait for the flags/channels decompile. Either way the engine should define its own alias model and not leak the library types.
4. Reverb/EQ: kira has `reverb`, `eq_filter`, `filter` effects; EAX room presets are not replicable exactly (Miles EAX filter `msseax.flt` closed source). Treat as approximate.
5. Resampling if device rate differs from 22.05/44.1/48 kHz sources: `rubato` (MIT OR Apache-2.0). kira/rodio resample internally.
6. Do not adopt `audionimbus`/Steam Audio or `hrtf`: no original feature maps to them, extra native dependency.

## 7. Open unknowns
1. `snd_alias_t.flags` bit layout (channel, loop, master/slave, dry/wet, randomisation); needs one decompile of the alias loader.
2. `channels.def` (names, per-channel voice caps, priority) and `volumemodgroups.def` content: not shipped; must come from engine tables.
3. Whether any `primed` entries exist in any zone (none found in MP).
4. Exact semantics of `SpeakerMap.channelMaps[2][2]` indices (appears to be [source mono/stereo][2D/3D?]).
5. The 348 zero-info loaded sounds in my parse (stubs vs back-references).
6. Miles reverb room-type table and EQ filter parameter mapping.
7. Real-time performance/CPU budget with 32 bots-worth of sound is a client concern, not measured.
