#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Review the screenshots and videos of a run bundle with two vision models.

python3 scripts/footage-review.py <bundle.zip> [--reference <bundle.zip>] [--out review.md]

EmbeddingGemma 2 (COD4E_EMBED_URL) sorts each screenshot and each second of
video into a scene, drops near-duplicate video frames and, with --reference,
scores each frame against the same frame of that bundle. A vision chat model
(COD4E_REVIEW_URL, Qwen3.5) lists the defects it sees in the remaining frames.
Both URLs are llama-server bases (`http://host:port`). Videos need ffmpeg.
"""
import argparse, base64, json, math, os, re, subprocess, sys, tempfile, urllib.request, zipfile

SCENES = {
    "menu": "a video game menu screen with buttons and options",
    "gameplay": "first-person shooter gameplay holding a gun in a 3D level",
    "killcam": "a killcam replay showing how the player was killed",
    "scoreboard": "a multiplayer scoreboard table listing players and scores",
    "loading": "a loading screen with a map picture and a progress bar",
    "black": "a black, empty screen",
}
PROMPT = (
    "This frame comes from a reimplementation of Call of Duty 4 multiplayer (PC, 2007). "
    "List only rendering or UI defects you are certain of: magenta or missing textures, flat untextured "
    "polygons, black or blank regions where the game draws content, stretched or exploded geometry, "
    "raw localization keys (text starting with @ or in CAPS_WITH_UNDERSCORES), overlapping or cut-off text. "
    "The stock menus have a dark, blurred background by design; that is not a defect. "
    'Answer JSON: {"defects": [{"what": "...", "where": "..."}]}, at most 3 entries, '
    "an empty list when the frame looks correct."
)


def post(url, body):
    req = urllib.request.Request(url, json.dumps(body).encode(), {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return json.load(r)


def unit(v):
    n = math.sqrt(sum(x * x for x in v))
    return [x / n for x in v]


def dot(a, b):
    return sum(x * y for x, y in zip(a, b))


def data_uri(name, data):
    mime = "image/jpeg" if name.lower().endswith((".jpg", ".jpeg")) else "image/png"
    return f"data:{mime};base64,{base64.b64encode(data).decode()}"


def frames(path):
    """(key, label, image name, bytes) for every screenshot and every second of every video."""
    z = zipfile.ZipFile(path)
    with tempfile.TemporaryDirectory() as tmp:
        for name in sorted(z.namelist()):
            # Stage folders carry their run order (`stages/03-client-gfx`), which differs between suites.
            key = re.sub(r"^stages/\d+-", "stages/", name)
            if name.lower().endswith((".png", ".jpg", ".jpeg")):
                yield key, name, name, z.read(name)
            elif name.lower().endswith(".mp4"):
                video = os.path.join(tmp, "video.mp4")
                with open(video, "wb") as f:
                    f.write(z.read(name))
                out = os.path.join(tmp, "f%05d.png")
                subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", video, "-vf", "fps=1", out], check=True)
                for i, png in enumerate(sorted(p for p in os.listdir(tmp) if p.startswith("f"))):
                    with open(os.path.join(tmp, png), "rb") as f:
                        yield f"{key}@{i}s", f"{name} @{i}s", png, f.read()
                    os.remove(os.path.join(tmp, png))


def embed(base, name, data):
    image = {"content": [{"type": "image_url", "image_url": {"url": data_uri(name, data)}}]}
    return unit(post(f"{base}/v1/embeddings", {"input": image})["data"][0]["embedding"])


def review(base, name, data):
    body = {
        "messages": [{"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": data_uri(name, data)}},
            {"type": "text", "text": PROMPT},
        ]}],
        "temperature": 0,
        "max_tokens": 400,
        "response_format": {"type": "json_object"},
    }
    text = post(f"{base}/v1/chat/completions", body)["choices"][0]["message"]["content"]
    try:
        return [f"{d['what']} ({d['where']})" for d in json.loads(text)["defects"]]
    except (ValueError, KeyError, TypeError):
        return [f"unparsed answer: {text[:200]}"]


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("bundle")
    ap.add_argument("--reference", help="bundle whose frames of the same name are the expected picture")
    ap.add_argument("--min-similarity", type=float, default=0.95, help="flag frames less similar to the reference")
    ap.add_argument("--dedup", type=float, default=0.98, help="skip a video frame this similar to the last one kept")
    ap.add_argument("--out", help="markdown report (default: stdout)")
    args = ap.parse_args()
    embed_url = os.environ.get("COD4E_EMBED_URL") or sys.exit("set COD4E_EMBED_URL")
    review_url = os.environ.get("COD4E_REVIEW_URL") or sys.exit("set COD4E_REVIEW_URL")

    text = post(f"{embed_url}/v1/embeddings", {"input": [f"task: classification | query: {t}" for t in SCENES.values()]})
    scenes = dict(zip(SCENES, (unit(d["embedding"]) for d in sorted(text["data"], key=lambda d: d["index"]))))
    reference = {}
    if args.reference:
        reference = {key: embed(embed_url, name, data) for key, _, name, data in frames(args.reference)}

    rows, last = [], {}
    for key, label, name, data in frames(args.bundle):
        v = embed(embed_url, name, data)
        source = key.split("@")[0]
        if "@" in key and source in last and dot(v, last[source]) >= args.dedup:
            continue
        last[source] = v
        scene = max(scenes, key=lambda s: dot(v, scenes[s]))
        sim = dot(v, reference[key]) if key in reference else None
        defects = review(review_url, name, data)
        rows.append((label, scene, sim, defects))
        print(f"{label}: {scene}, {len(defects)} defect(s)", file=sys.stderr)

    flagged = [r for r in rows if r[3] or (r[2] is not None and r[2] < args.min_similarity)]
    counts = {s: sum(r[1] == s for r in rows) for s in SCENES}
    out = [f"# Footage review: {os.path.basename(args.bundle)}", ""]
    out.append(f"{len(rows)} frames reviewed; scenes: " + ", ".join(f"{s} {n}" for s, n in counts.items() if n) + ".")
    if args.reference:
        out.append(f"Reference: {os.path.basename(args.reference)} (flag below {args.min_similarity}).")
    out += ["", "Model findings are leads to check by eye, not verdicts.", ""]
    for title, table in (("Flagged", flagged), ("All frames", rows)):
        out += [f"## {title}", "", "| frame | scene | ref sim | defects |", "|---|---|---|---|"]
        out += [f"| {l} | {s} | {'-' if m is None else f'{m:.3f}'} | {'; '.join(d).replace('|', '/') or '-'} |" for l, s, m, d in table]
        out.append("")
    report = "\n".join(out)
    if args.out:
        with open(args.out, "w") as f:
            f.write(report)
    else:
        print(report)


if __name__ == "__main__":
    main()
