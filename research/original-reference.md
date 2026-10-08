<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Original game reference notes (ticket #157)

Observed behaviour of the original CoD4 1.7 MP client (version string `1.7.568`), run through Proton in a
1920x1060 window. Recorded as text only: no content, no screenshots. Coordinates are in window pixels
(1920x1060, origin top-left) unless a 640x480 value is stated; they were measured from screenshots and
are accurate to a few pixels.

## Input and capture
- Mouse works: the game draws its own arrow cursor (small grey arrow, ~25 px, tip at the hotspot). It is
  invisible in plain `grim` screenshots, visible with `grim -c`. The cursor follows an absolute Hyprland
  move followed by a 1 px relative uinput wiggle. See `scripts/original/README.md`.
- Left click on a menu row activates it; hover highlights the row (grey-white gradient bar from the row's left
  fade to its right edge, ~49 px tall).
- Escape is the universal "back": see the per-screen notes.

## Main menu (front end)
Right-aligned text column, right edge x~473, row centres y:
Join Game 343, Start New Server 396, (gap) Select Profile 471, Create a Class 524 (greyed, disabled),
Rank & Challenges 577, (gap) Controls 651, Options 705, Mods 758, (gap) Single Player 834, Quit 885.
Version `1.7.568` bottom right (x~1823,y~1002); bottom-left tip text "Game experience may change during online
play." (two lines, x~90). Logo top right. Hovering a row highlights it; no row is selected at start.
`Create a Class` stays greyed with a freshly reset stats file (rank 1).

## Start New Server (priority a)
Title "Start New Server" gold, centred x~433, y~39, in a title plate at the top-left (width ~855).
Rows (labels right-aligned, right edge x~460; values left-aligned, left edge x~540 (Server Name value x~535);
rows are ~53 px apart):

| y | label | value shown |
|---|-------|-------------|
| 99 | Game Mode: | game mode name (text, e.g. `Headquarters`) |
| 153 | Server Name: | `CoD4Host` (default text) |
| 206 | Dedicated: | `No` |
| 260 | Maximum Players: | `24` |
| 312 | Minimum Ping: | `0` |
| 365 | Maximum Ping: | `0` |
| 419 | Password: | (empty) |
| gap |
| 492 | Voice Chat: | `No` |
| 546 | Auto-Balance Teams: | `No` |
| 599 | Allow Voting: | `Yes` |
| 651 | PunkBuster: | `No` |
| gap |
| 727 | Game Mode Settings | (button, no value) |

- The mode shown on first open was whatever gametype the dvar held (here `Headquarters`).
- Left click on the Game Mode row (anywhere on the row, value area included) advances to the next mode;
  right click goes to the previous. Cycle order: Free-for-all, Domination, Search and Destroy, Sabotage,
  Team Deathmatch, Headquarters, then wraps to Free-for-all. (Six modes only; no Capture the Flag.)
- Keyboard Right arrow with the row hovered did not change the value.
- Right side: a map preview image (x 1335..1784, y 233..531) and below it a map list box
  (x 1337..1783, y 590..940) with a scroll bar on its right (up/down arrows, thumb). Alphabetical: Ambush,
  Backlot, Bloc, Bog, Countdown, Crash, Crossfire, District, Downpour, Overgrown, Pipeline, Shipment,
  Showdown, ... The list scrolls to the selected map; the preview image follows the selection.
- Bottom: `Back` (x~428, y~1022), `Start` (x~1484,y~1022) on a dark bottom bar with angled ends.
- Escape returns to the main menu.

## Select Profile (priority d)
Opens a popup column on the right (x~1245..1830): header "Select Profile" (centred, blue-underlined plate,
y~191), column header "Name" (y~298), a framed list box with scroll bar (y 322..890) holding one entry
(the install's profile, highlighted as the selected row on open), buttons along the bottom (y~941):
`OK` (x~1307), `Cancel` (x~1446), `New` (x~1582), `Delete` (x~1717). No popup appears at start.
Clicking OK/Enter did not close it in the harness (the buttons show no hover highlight); Escape closes it
and returns to the main menu. Create a Class remains greyed (no unlocks yet: see the stats-file note below).

### Stats file rejection (coordinator note)
On its first run the original judged the profile's stats file (`mpdata`) invalid, renamed it to
`mpdata.corrupt` and wrote a fresh reset file (no unlocks, rank 1). The backup must not be touched.

## In-game flow (priorities b, c)
`devmap mp_backlot` with `g_gametype war` (Team Deathmatch) opens straight into Choose Team.

### Choose Team
Title "CHOOSE TEAM" (gold caps, centred x~403,y~39). Items, right-aligned to x~713: OpFor 475,
Marines 528, Auto-Assign 582, (gap), Controls 658, Options 710, Leave Game 763. Right: a map title plate
"Backlot" (x 1335..1880, y 250..295) and a minimap with the player arrow (yellow) below it, then a thin
bar (y~835). `Back` at the bottom.
Via the Escape menu's `Change Team` the same screen lists only the other team: OpFor 528 (team you are
not on), Auto-Assign 582, (gap), Spectator 658; your own team's row is absent, the Controls/Options/Leave
rows are replaced by Spectator. The own team emblem and name (`Marines`, dim letters, x~115,y~408) sit on
the left. Escape from this screen closes the menu back to the game.

### Choose Class
Title "CHOOSE CLASS". Section header "Default Classes" (blue plate, centred x~508, y~99). Rows (right edge
x~713): Assault 153 (highlighted on open), Spec Ops 206, Heavy Gunner 260, Demolitions 312 (greyed,
locked), Sniper 365 (greyed, locked). Left: team emblem and name `Marines` (dim). Right: a loadout panel
(x 1122..1748, y 75..865) with header bar, primary weapon name (gold) + image and attachment name, secondary
weapon (`M9`, `No Attachment`), `Perks & Inventory` bar, then three perk rows (icon, gold name, grey
description) and the grenade row (`Frag / Stun x 1`). Assault = M16A4 with Grenade Launcher, Stopping Power,
Extreme Conditioning. Picking a class spawns the player immediately (no further confirm).

### In-game Escape menu
Title "OPTIONS". Header "Team Deathmatch" (blue plate, centred x~958, y~111) and the mode description
"Gain points by eliminating enemy players.  First team to 750 wins." under it (y~166). Items right-aligned
to x~713: Choose Class 475, Change Team 528, Controls 582, Options 634, (gap), Call Vote 710, Mute Players
763, Leave Game 815. Team emblem + name at left, map plate + minimap at right, Back at the bottom.
Escape toggles: in the game it opens this menu; in this menu it closes it (back to the game).
`Change Team` opens the team screen above; Escape there returns to the game.

## Running the original with a mouse-driven walk (notes for repeating)
- The first launch shows two Windows dialogs (safe mode after a crash kill; "Set Optimal Settings?"): answer No to both.
- `+devmap mp_backlot +set g_gametype war` on the command line goes straight to CHOOSE TEAM after the map loads.
- Loading screen: map-photo background, centred three lines "Team Deathmatch" / "Backlot" / "Setting up game.." (pixel
  font, white with shadow, y 170..300), a boxed "INTEL" tip panel bottom-left (x 320..955, y 595..900; header
  "INTEL" gold, body white; one tip was about Create-a-class), a white progress bar below it (x 670..1255, y~972).
- In-game the console treats lines as chat unless they are known commands typed with the console already open.
  Bare dvar names or `scr_testclients 6` are sent as chat ("<name>: text" in the top-left chat feed, small white
  text starting x~12,y~230).
- Bots: `scr_testclients N` only works when `developer 1`, `developer_script 1`, `sv_cheats 1` are set on the
  command line BEFORE the map loads (`+set developer 1 +set developer_script 1 +set sv_cheats 1 +set scr_testclients 5
  +set sv_botsPressAttackBtn 1 +devmap mp_backlot`). Setting them from the console afterwards adds nothing.
  Bots appear as bot0..botN, ping column shows 999 (team modes) or -1 (free-for-all), a green "C" icon before
  the name, team games split them across both teams. In free-for-all rows are plain olive bars (no team headers).
- Bots stand still and did not kill the player in ~10 minutes of waiting at the spawn, so no kill/killcam was captured.
- Console typing: `grave` opens the small one-line console; Shift+grave opens the full-screen console. Console
  commands typed with a leading `/` are treated as chat when the console is closed; type them without `/`.
- The Key Code dialog (Options > Multiplayer Options > Enter Key Code) has four boxes plus a dash box, buttons
  Verify / Cancel (Cancel becomes Close after verify), and says "Key code appears to be valid." on success. The
  user's key was entered there only (stored by the game in the Wine prefix, not in this repo).
- A second client joining the first (`connect 127.0.0.1`) first showed "Invalid Server." (lan-only server) and, when
  the key was valid, "An error occurred while reading the stats data. Your stats have been reset." (same stats-file
  complaint as above); the client then sat at "Awaiting connection". Two windows overlapping at the same geometry made
  focus/typing unreliable; not resolved.
- Two cursors can be on screen at once in `grim -c` shots: the game's own arrow (grey, ~25 px) and the OS
  pointer (a small blue-grey arrow/hand drawn by the compositor) when the OS pointer is not hidden.

## In-game HUD, class Assault, TDM, just after spawn (priority e, partial)
Positions in 1920x1060 px.
- Spawn splash, top centre: line 1 "Tied 0 - 0" (yellow, y~26), line 2 big pixel-font text "Marine Force Recon"
  (white, y~100, ~45 px tall), team emblem under it (y~195, ~75 px wide). It fades after a few seconds.
- Minimap, top left: framed square (x 12..245, y 37..266) with a compass-letter strip above it (N NE E SE S as
  you turn). Player is a yellow arrow in the centre.
- Bottom left: team emblem in a circle (x 10..100, y 940..1040) and two score bars (own team, enemy) with
  numeric 0 / 0 and a small triangle marker at the bar end; round timer to its right ("9:24", x~373, y~1015,
  white).
- Bottom centre-left, two action/equipment slots at y~975..1020: a **grenade-launcher (underbarrel) icon**
  with the count `2` to its right and the key label `[5]` under it (grey, bracketed) at x~795; a second icon
  (a claymore/NVG style glyph) with label `[N]` at x~960 (no count).
- Bottom right, grenade slots: a triangle badge holding the frag grenade icon with `1` right of it, then a
  round "radar ring" badge holding the special-grenade (stun) icon with `1` right of it (x 1800..1900,
  y~980); under them the ammo clip bar (thin ticked bar, x 1725..1845) and ammo count `60` big at the right end
  (y~1012).
- Right side, perk notices: two lines right-aligned to x~1830 with a rounded-square icon at the far right:
  "Stopping Power" (y~755, red icon) and "Extreme Conditioning" (y~825, green running icon).
- Bottom edge: a thin compass tick strip along the very bottom.
- Scoreboard (hold Tab): a header strip at the top ("<own team emblem> 0", "<enemy emblem> 0", centre
  gametype "Team Deathmatch" or, after a while, "Tied with 0 of 750 points.", timer top right); a panel
  at x~415..1500: team row "Marines  ( 1 )" with column headers Score, Kills, Assists, Deaths, Ping
  (x 985, 1120, 1228, 1335, 1448), player row with a rank icon + level, highlighted bar; then "OpFor ( 0 )". Bottom
  left server name, bottom right "Listen Server".

## Main-menu Options (found while looking for the key dialog)
Title "Options" (gold). Right-aligned list (right edge x~720 of 1920): Graphics..., Texture Settings..., Sound...,
Voice Chat..., Game Options..., Multiplayer Options..., Optimal System Settings; Back at the bottom. Hovering
"Multiplayer Options..." shows a panel to the right: header "Multiplayer Options...", rows PunkBuster Yes, Allow
Downloading Yes, (gap) Player Name <name>, Enter Key Code.

## Scoreboard (hold Tab)
- Team game: top strip (own emblem + score at left, enemy emblem + score, centre text "Team Deathmatch" or
  "Tied with 0 of 750 points.", timer right); body x 415..1500 (1920 wide): team header "Marines ( 4 )" with columns Score,
  Kills, Assists, Deaths, Ping (right-aligned at x~1040, 1150, 1260, 1340, 1475), rank chevron+level before each
  name, own row gold, then "OpFor ( 2 )". Footer: server name left, "Listen Server" right.
- Free-for-all: strip with one emblem + score, centre "Free-for-all"; a single list headed "( 6 )" with no team header.

## End of round
After the time limit the round ends with a "Game Summary" dialog (title gold, Rank: chevron + "Private First Class",
"XP Required:" bar and 30, "Next Rank: ... I", Score / Challenge Completed / Match Bonus XP lines, "Total XP Earned:",
a ">>>" button). Clicking it returns to Choose Team on the next map (rotation went from mp_crash to mp_backlot).
Loading screen for a free-for-all map showed "Free-for-all / Bloc / Setting up game..." and an INTEL tip box.

## Open items not captured
- (e) kill/connect console messages, radar/airstrike notices, grenade-key labels in use: no kill obtained.
- (f) killcam: not captured (needs a kill). The host is otherwise ready; see the h.js helper in scripts/original.
- Create a Class and unlock-gated screens: Create a Class stays greyed at rank 1 with the reset stats file.

## Killcam attempt 2 (time-boxed)
Relaunch with one bot (`scr_testclients 1`, `sv_botsPressAttackBtn 1`, TDM, no limits) and ~15 minutes of
randomised walking plus minimap-based homing: the bot never found or shot the player, so no death, obituary or
killcam was captured. Killcam band height/colour/opacity, viewmodel and camera-follow remain unrecorded.
The original game was closed afterwards; ydotoold is left running.
