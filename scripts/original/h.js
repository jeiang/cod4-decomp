// Mac-side helper (Bun): drives the reference host over ssh. Load with: %load scripts/original/h.js
const HOST = process.env.ORIG_HOST;
const Y = process.env.ORIG_YDOTOOL ?? "ydotool", G = process.env.ORIG_GRIM ?? "grim";
const D = process.env.ORIG_EVIDENCE ?? `${process.env.HOME}/cod4e-evidence/original-ref/`;
const ENV = `export HYPRLAND_INSTANCE_SIGNATURE=$(ls /run/user/1000/hypr | head -1) WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000 YDOTOOL_SOCKET=/run/user/1000/.ydotool_socket; Y=${Y}; `;
globalThis.D = D; globalThis.ENV = ENV;
globalThis.sh = async (c) => { const p = Bun.spawn(["ssh", HOST, "bash -s"], { stdin: new Blob([c]), stdout: "pipe", stderr: "pipe" }); const [o, e] = await Promise.all([new Response(p.stdout).text(), new Response(p.stderr).text()]); await p.exited; return (o + e).trim(); };
globalThis.hc = (c) => sh(`${ENV} hyprctl ${c}`);
globalThis.focus = () => hc(`dispatch 'hl.dsp.focus({window="class:iw3mp.exe"})'`);
globalThis.S = async (n) => { await sh(`${ENV} ${G} -c -g "0,10 1920x1060" ~/cod4e/ref-shots/${n}.png`); await Bun.$`scp -q ${HOST}:cod4e/ref-shots/${n}.png ${D}${n}.png`; return D + n + ".png"; };
const mvs = (x, y) => `hyprctl dispatch 'hl.dsp.cursor.move({x=${x},y=${y + 10}})' >/dev/null; $Y mousemove -x 1 -y 0; sleep 0.1; $Y mousemove -x -1 -y 0; sleep 0.25;`;
globalThis.mv = (x, y) => sh(`${ENV} ${mvs(x, y)}`);
globalThis.clk = (x, y, b = "0xC0") => sh(`${ENV} ${x !== undefined ? mvs(x, y) : ""} $Y click ${b}; sleep 0.7`);
const KC = { esc: 1, enter: 28, space: 57, up: 103, down: 108, left: 105, right: 106, tab: 15, backspace: 14, grave: 41, f1: 59 };
globalThis.key = (...ks) => sh(`${ENV} ${ks.map(k => { const c = KC[k] ?? k; return `$Y key ${c}:1 ${c}:0; sleep 0.3`; }).join("; ")}`);
globalThis.typ = (t) => sh(`${ENV} $Y type --key-delay 40 '${t}'`);
// console command: open with grave, type, enter, close
globalThis.con = (t) => sh(`${ENV} $Y key 41:1 41:0; sleep 0.5; $Y type --key-delay 30 '${t}'; $Y key 28:1 28:0; sleep 0.4; $Y key 41:1 41:0; sleep 0.4`);
