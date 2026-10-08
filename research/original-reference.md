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
