#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Print the harness stages a diff can affect: `just stages-for <base> <head>`.

The changed paths are `git diff --name-only --no-renames <base>...<head>` (the head's own change since its merge base
with <base>). Each path is decided by the first matching rule of scripts/stage-map.txt; a path no rule matches selects
the full default suite. Stage facts (names, the default suite, which module or scenario each stage runs and which
modules those name) are read from <head>'s crates/harness/src/stage.rs and stages/*.rs, so the list follows the
stages as they are at the head.

stdout is one line: the selected stages, comma separated, in suite order (empty when nothing is selected), ready for
`just slot <n> <ref> "$(just stages-for origin/main HEAD)"`. stderr says, for each changed path, the rule that decided
it and what it selected. Exit status 2 on a bad ref or a map that names a stage the head does not have.
"""

import argparse
import fnmatch
import os
import re
import subprocess
import sys

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
MAP = os.path.join(SCRIPTS, "stage-map.txt")
STAGE_RS = "crates/harness/src/stage.rs"
STAGES_DIR = "crates/harness/src/stages"
CONTROL_RS = "crates/harness/src/control.rs"


class Fail(Exception):
    pass


def git(*args, cwd=None):
    p = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)
    if p.returncode != 0:
        raise Fail(f"git {' '.join(args)}: {p.stderr.strip() or 'exit ' + str(p.returncode)}")
    return p.stdout


def glob_regex(pattern):
    """A repository-path glob as a regex: `*`/`?` stay in one directory, `**` spans directories."""
    out, i = "", 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out += "(?:.*/)?"
            i += 3
        elif pattern.startswith("**", i):
            out += ".*"
            i += 2
        elif pattern[i] == "*":
            out += "[^/]*"
            i += 1
        elif pattern[i] == "?":
            out += "[^/]"
            i += 1
        else:
            out += re.escape(pattern[i])
            i += 1
    return re.compile(out + r"\Z")


class Rule:
    def __init__(self, line_no, pattern, selections):
        self.line_no, self.pattern, self.selections = line_no, pattern, selections
        self.regex = glob_regex(pattern)


class Map:
    """scripts/stage-map.txt: ordered rules, the timing set and the default stages that are not @client."""

    def __init__(self, path=MAP, name="scripts/stage-map.txt"):
        self.name, self.rules, self.timing, self.no_client = name, [], [], []
        with open(path) as f:
            for n, line in enumerate(f, 1):
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                words = line.split()
                if words[0] == "timing:":
                    self.timing += words[1:]
                    continue
                if words[0] == "no-client:":
                    self.no_client += words[1:]
                    continue
                if len(words) < 2:
                    raise Fail(f"{name}:{n}: a rule needs a pattern and a selection")
                for s in words[1:]:
                    if not (s in ("@full", "@none", "@module", "@client", "@web", "+timing")
                            or s.startswith("@uses:") or re.fullmatch(r"[a-z0-9*?-]+", s)):
                        raise Fail(f"{name}:{n}: unknown selection {s}")
                self.rules.append(Rule(n, words[0], words[1:]))
        if not self.timing:
            raise Fail(f"{name}: no `timing:` line")

    def check(self, stages):
        """Every stage the map names exists at the head: a stale map is an error, not a silent gap."""
        names = {s.name for s in stages}
        for t in self.timing:
            if t not in names:
                raise Fail(f"{self.name}: timing stage {t} is not a stage at the head")
        for t in self.no_client:
            if t not in names:
                raise Fail(f"{self.name}: no-client stage {t} is not a stage at the head")
        for r in self.rules:
            for sel in r.selections:
                if not sel.startswith(("@", "+")) and not any(fnmatch.fnmatchcase(n, sel) for n in names):
                    raise Fail(f"{self.name}:{r.line_no}: {sel} names no stage at the head")


class Stage:
    def __init__(self, name, default, module, scenario):
        self.name, self.default, self.module, self.scenario = name, default, module, scenario
        self.files = set()  # repository paths of the code the stage includes
        self.text = ""  # that code, for the @client/@web/@uses tests


def parse_stages(stage_rs):
    """The STAGES table of stage.rs: name, default flag and the module or scenario each runs."""
    start = stage_rs.find("pub static STAGES")
    if start < 0:
        raise Fail(f"{STAGE_RS}: no `pub static STAGES`")
    stages = []
    for m in re.finditer(r"StageDef \{(.*?)\n    \},", stage_rs[start:], re.S):
        body = m.group(1)
        name = re.search(r'\bname: "([^"]+)"', body)
        default = re.search(r"\bdefault: (true|false)", body)
        if not name or not default:
            raise Fail(f"{STAGE_RS}: a StageDef without a name or default flag")
        builtin = re.search(r"Kind::Builtin\(\s*crate::stages::(\w+)::", body)
        server = re.search(r'Kind::(?:Server|Script)\(\s*include_str!\("\.\./scenarios/([^"]+)"\)', body)
        stages.append(Stage(name.group(1), default.group(1) == "true",
                            builtin.group(1) if builtin else ("server" if server else None),
                            f"crates/harness/scenarios/{server.group(1)}" if server else None))
    if not stages:
        raise Fail(f"{STAGE_RS}: no stages found")
    return stages


def module_refs(src, modules):
    """The stage modules a module's source names."""
    refs = set(re.findall(r"\b(?:super|crate::stages)::(\w+)", src))
    for group in re.findall(r"\b(?:super|crate::stages)::\{([^}]*)\}", src):
        refs.update(w.split("::")[0].strip() for w in group.split(","))
    return {r for r in refs if r in modules}


def stage_facts(head, cwd=None):
    """The stages at <head>, each with the files and code it includes."""
    stages = parse_stages(git("show", f"{head}:{STAGE_RS}", cwd=cwd))
    names = git("ls-tree", "--name-only", f"{head}:{STAGES_DIR}", cwd=cwd).split()
    sources = {n[:-3]: git("show", f"{head}:{STAGES_DIR}/{n}", cwd=cwd) for n in names if n.endswith(".rs")}
    sources.pop("mod", None)
    # Stages that drive the client through crate::control (crates/harness/src/control.rs) start it there: @client.
    refs = {m: module_refs(src, sources) for m, src in sources.items()}
    for s in stages:
        seen, todo = set(), [s.module] if s.module in sources else []
        while todo:
            m = todo.pop()
            if m not in seen:
                seen.add(m)
                todo.extend(refs[m] - seen)
        s.files = {f"{STAGES_DIR}/{m}.rs" for m in seen}
        s.text = "\n".join(sources[m] for m in sorted(seen))
        if s.scenario:
            s.files.add(s.scenario)
    return stages


def matches_stage(sel, stage, m):
    if sel == "@client":
        return stage.name not in m.no_client
    if sel == "@web":
        return "browser_cdp" in stage.text or "COD4E_WEB_DIR" in stage.text
    if sel.startswith("@uses:"):
        return re.search(r"(?<![\w:])" + re.escape(sel[6:]) + r"::", stage.text) is not None
    raise Fail(f"unknown selection {sel}")


def select(paths, m, stages):
    """(set of stage names, explanation lines)."""
    m.check(stages)
    defaults = [s for s in stages if s.default]
    timing, map_name = m.timing, m.name
    chosen, notes = set(), []
    for path in paths:
        rule = next((r for r in m.rules if r.regex.match(path)), None)
        if rule is None:
            chosen.update(s.name for s in defaults)
            notes.append(f"{path}: unmapped -> full default suite")
            continue
        got, why = set(), []
        for sel in rule.selections:
            if sel == "@full":
                got.update(s.name for s in defaults)
            elif sel == "@none":
                pass
            elif sel == "+timing":
                got.update(timing)
            elif sel == "@module":
                hit = {s.name for s in stages if path in s.files}
                if not hit:
                    hit = {s.name for s in defaults}
                    why.append("no stage includes it -> full default suite")
                got.update(hit)
            elif sel.startswith("@"):
                got.update(s.name for s in defaults if matches_stage(sel, s, m))
            else:
                got.update(s.name for s in stages if fnmatch.fnmatchcase(s.name, sel))
        chosen |= got
        label = " ".join(rule.selections)
        notes.append(f"{path}: {rule.pattern} ({map_name}:{rule.line_no}) {label}"
                     + (f" [{'; '.join(why)}]" if why else "") + f" -> {len(got)} stage(s)")
    return chosen, notes


def ordered(chosen, stages):
    return [s.name for s in stages if s.name in chosen]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("base", nargs="?", default="origin/main")
    ap.add_argument("head", nargs="?", default="HEAD")
    ap.add_argument("--map", default=MAP, help=argparse.SUPPRESS)
    args = ap.parse_args(argv)
    try:
        for ref in (args.base, args.head):
            git("rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}")
        m = Map(args.map, "scripts/stage-map.txt" if args.map == MAP else args.map)
        stages = stage_facts(args.head)
        paths = git("diff", "--name-only", "--no-renames", f"{args.base}...{args.head}").split("\n")
        paths = [p for p in paths if p]
        chosen, notes = select(paths, m, stages)
    except Fail as e:
        print(f"stages-for: {e}", file=sys.stderr)
        return 2
    for n in notes:
        print(n, file=sys.stderr)
    if any(p.startswith("scripts/") or p == "justfile" for p in paths):
        print("scripts or justfile changed: also run `just script-tests`", file=sys.stderr)
    print(",".join(ordered(chosen, stages)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
