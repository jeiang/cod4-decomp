CoD4 engine portable folder

This folder holds three programs and needs your own copy of Call of Duty 4
(the folder that contains "main" and "zone"; Steam or retail):

  cod4e-harness.exe   runs the test stages and writes a zip to send back
  cod4e-server.exe    a dedicated server (no window)
  cod4e.exe           the game client

Nothing is installed; delete the folder when done. The server and the client
find your Call of Duty 4 folder themselves (the --install option, then the
COD4_PATH variable, then the folder cod4e or the harness asked you for last
time, then the Steam and registry locations, then a "COD4" folder next to the
program). cod4e asks for the folder in a dialog when it finds none and
remembers your answer. cod4e-server never asks: if it finds nothing it says
where it looked; start cod4e once, or give it --install "<folder>".


PLAY A MATCH

1. Start the server. Open a Command Prompt in this folder and run:

     cod4e-server.exe +set g_gametype war +set bot_count 3 +set net_port 28960 +map mp_crash

   war is team deathmatch (others: dm, dom, koth, sab, sd), bot_count is the
   number of computer players, net_port is the UDP port (28960 is the one
   the client's server list looks at; 28961 to 28963 work too), mp_crash is
   the map. If it cannot find the game folder, add
   --install "C:\Program Files (x86)\Steam\steamapps\common\Call of Duty 4".
   The server is ready when it prints "map mp_crash loaded".

2. Windows asks whether to let cod4e-server through its firewall the first
   time. Allow it (tick Private networks for a game on your home network).
   This is needed for other PCs only; the same PC always works.

3. Join. Start cod4e.exe (double-click is fine), then either:
   - Join Game, click "Source" until it says Local, wait a moment for the
     list to fill (Refresh List asks again), click your server, then Join
     Server. Local finds servers on the same PC and on your network.
   - or skip the menus and run, on any PC that can reach the server:
       cod4e.exe --connect 192.168.1.20:28960
     (the server PC's address and the port from step 1; use 127.0.0.1 on the
     server's own PC). Inside the game you can also press ~ and type
       connect 192.168.1.20:28960

4. Play. The server's menus ask for a team (Auto Assign is fine) and a class.
   WASD move, mouse look, left click fires. The bots fill the teams on their
   own.

5. Stop. In the game press Esc, choose Leave Game and confirm (Quit on the
   main menu closes the program, or just close the window); the server sees
   you leave. To stop the server type  quit  in its window (or press Ctrl+C).


RUN THE TEST STAGES

1. Double-click cod4e-harness.exe. A console window opens.
2. If it cannot find your Call of Duty 4 install it asks for the folder
   (the one containing "main" and "zone"), and cod4e and cod4e-server use
   that choice afterwards.
3. Wait for it to finish (about 10 minutes once all stages exist; the first
   stages are quick). The client stages need a GPU and a display; they record
   a short video (a few MB) per display mode, and are skipped when the client
   or a display is missing. The stage client-dedicated starts cod4e-server and
   joins it with cod4e, as in the steps above. It writes
   cod4e-run-<date>.zip next to the program.
4. Send that zip. It is scrubbed of your user name, home folder and host name,
   and holds no game files. It stays under 100 MB.
