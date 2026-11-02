#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Report how run bundles and CI bear on a pull request's current head: a report, never a merge approval.

python3 scripts/evidence-report.py --pr <n> --stage <a,b,...> <bundle.zip>... [--review <file>] [--check ci/windows]
                                   [--repo owner/name] [--json]

Reads the PR's head, base, potential merge commit and checks with one `gh pr view`, and each named bundle's
manifest.json and summary.json. Resolves each bundle's build hash to a local commit with git and compares its tree
with the head's. Runs only `gh pr view` and the read-only git commands rev-parse, cat-file and merge-base: it never
fetches, merges, pushes, uploads or writes. Exit status: 0 CONSISTENT, 1 FAIL, 3 INCOMPLETE, 4 ERROR (2 is usage).
"""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import zipfile
import zlib

HEX = re.compile(r"[0-9a-f]{12,64}")
OID = re.compile(r"[0-9a-f]{40}|[0-9a-f]{64}")
STATUSES = {"passed", "failed", "skipped", "crashed", "hung"}
BAD = {"failed", "crashed", "hung"}
SCHEMA = 1
JSON_CAP = 16 << 20
PR_FIELDS = "number,state,isDraft,headRefOid,baseRefOid,baseRefName,potentialMergeCommit,statusCheckRollup,reviewDecision"
# The only commands this tool may run. Each is read only; gh pr view reads GitHub, never writes it.
GH_READS = {("pr", "view")}
GIT_READS = {"rev-parse", "cat-file", "merge-base"}
# Tree states that make a bundle evidence for the head.
FOR_HEAD = {"EXACT", "SAME_TREE"}
EXIT = {"CONSISTENT": 0, "FAIL": 1, "INCOMPLETE": 3, "ERROR": 4}


class Refused(Exception):
    """A command outside the read-only allow-list."""


def _run(cmd):
    env = dict(os.environ)
    # No index refresh, no credential prompt, no lazy fetch of missing objects in a partial clone, no gh prompt.
    env.update(GIT_OPTIONAL_LOCKS="0", GIT_TERMINAL_PROMPT="0", GIT_NO_LAZY_FETCH="1",
               GH_PROMPT_DISABLED="1", GH_NO_UPDATE_NOTIFIER="1", GH_SPINNER_DISABLED="1")
    try:
        p = subprocess.run(cmd, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120)
    except (OSError, subprocess.TimeoutExpired) as e:
        return 127, "", str(e)
    return p.returncode, p.stdout, p.stderr


def run_gh(args):
    if tuple(args[:2]) not in GH_READS:
        raise Refused(f"gh {' '.join(args[:2])} is not read only")
    return _run(["gh", *args])


def run_git(args):
    if not args or args[0] not in GIT_READS:
        raise Refused(f"git {args[0] if args else ''} is not read only")
    return _run(["git", *args])


class Git:
    def __init__(self, run):
        self.run = run

    def commits(self, prefix):
        """Every local commit whose name starts with `prefix`, or None when git fails."""
        code, out, _ = self.run(["rev-parse", f"--disambiguate={prefix}"])
        if code:
            return None
        found = []
        for oid in out.split():
            code, kind, _ = self.run(["cat-file", "-t", oid])
            if code:
                return None
            if kind.strip() == "commit":
                found.append(oid)
        return found

    def tree(self, oid):
        code, out, _ = self.run(["rev-parse", "--verify", "--quiet", f"{oid}^{{tree}}"])
        return out.strip() if code == 0 and out.strip() else None

    def parents(self, oid):
        code, out, _ = self.run(["rev-parse", f"{oid}^@"])
        return out.split() if code == 0 else None

    def is_ancestor(self, a, b):
        """True or False, or None when git cannot tell (an object is not local)."""
        code, _, _ = self.run(["merge-base", "--is-ancestor", a, b])
        return {0: True, 1: False}.get(code)


def pr_facts(gh, pr, repo):
    """The PR's facts, or (None, why) when gh failed or printed something unexpected."""
    args = ["pr", "view", str(pr), "--json", PR_FIELDS] + (["--repo", repo] if repo else [])
    code, out, err = gh(args)
    if code:
        return None, f"gh pr view failed (exit {code}): {err.strip()[:300] or 'no message'}"
    try:
        d = json.loads(out)
    except ValueError:
        return None, "gh pr view printed invalid JSON"
    if not isinstance(d, dict):
        return None, "gh pr view printed no object"
    for k in ("headRefOid", "baseRefOid"):
        if not isinstance(d.get(k), str) or not OID.fullmatch(d[k]):
            return None, f"gh pr view gave no usable {k}"
    if d.get("number") != pr:
        return None, f"gh pr view answered for PR {d.get('number')!r}, not #{pr}"
    rollup = d.get("statusCheckRollup")
    if rollup is None:
        d["statusCheckRollup"] = []
    elif not isinstance(rollup, list):
        return None, "gh pr view gave a statusCheckRollup that is not a list"
    return d, None


def ci_status(rollup, check):
    """PASS, FAIL, PENDING, MISSING or UNKNOWN for `workflow/name` (or a status context), with what was seen."""
    workflow, _, name = check.rpartition("/")
    seen = []
    for c in rollup:
        if not isinstance(c, dict):
            continue
        if c.get("__typename") == "StatusContext":
            if c.get("context") != check:
                continue
            state = c.get("state")
            seen.append(("PASS" if state == "SUCCESS" else "PENDING" if state in ("PENDING", "EXPECTED")
                         else "FAIL" if isinstance(state, str) else "UNKNOWN", f"state {state}"))
        elif c.get("name") == name and (c.get("workflowName") or "") == workflow:
            status, conclusion = c.get("status"), c.get("conclusion")
            if status != "COMPLETED":
                seen.append(("PENDING" if isinstance(status, str) else "UNKNOWN", f"status {status}"))
            else:
                # SKIPPED and NEUTRAL are not passes.
                seen.append(("PASS" if conclusion == "SUCCESS" else "FAIL", f"conclusion {conclusion}"))
    if not seen:
        return "MISSING", "no such check on the head"
    states = {s for s, _ in seen}
    state = next(s for s in ("FAIL", "UNKNOWN", "PENDING", "PASS") if s in states)
    return state, ", ".join(d for _, d in seen)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _json_member(z, names, name):
    if names.count(name) != 1:
        raise ValueError(f"{name}: {names.count(name)} copies in the zip")
    info = z.getinfo(name)
    if info.file_size > JSON_CAP:
        raise ValueError(f"{name}: {info.file_size} bytes, over the {JSON_CAP} byte limit")
    try:
        return json.loads(z.read(name))
    except ValueError as e:
        raise ValueError(f"{name}: not JSON ({e})") from None


def read_bundle(path):
    """A bundle's identity and stages, with `problems` naming every way it is unreadable or inconsistent."""
    b = {"path": path, "sha256": None, "build": None, "os": None, "stages": [], "run_ok": False, "problems": []}
    try:
        b["sha256"] = sha256(path)
        with zipfile.ZipFile(path) as z:
            names = z.namelist()
            broken = z.testzip()
            if broken:
                b["problems"].append(f"zip member {broken} fails its CRC")
                return b
            manifest = _json_member(z, names, "manifest.json")
            summary = _json_member(z, names, "summary.json")
    except (OSError, zipfile.BadZipFile, zlib.error, ValueError, RuntimeError, NotImplementedError, EOFError) as e:
        b["problems"].append(f"unreadable: {e}")
        return b
    p = b["problems"]
    if not isinstance(manifest, dict) or not isinstance(summary, dict):
        p.append("manifest.json or summary.json is not an object")
        return b
    for name, doc in (("manifest.json", manifest), ("summary.json", summary)):
        if doc.get("schema") != SCHEMA:
            p.append(f"{name}: schema {doc.get('schema')!r}, this tool reads {SCHEMA}")
    build = manifest.get("build")
    mhash = build.get("hash") if isinstance(build, dict) else None
    shash = summary.get("build")
    if not isinstance(mhash, str) or not isinstance(shash, str):
        p.append("no build hash in manifest.json or summary.json")
    elif mhash != shash:
        p.append(f"manifest.json build {mhash} differs from summary.json build {shash}")
    else:
        b["build"] = mhash
    os_ = manifest.get("os")
    if isinstance(os_, dict):
        b["os"] = f"{os_.get('family')} {os_.get('arch')}"
    stages = summary.get("stages")
    if not isinstance(stages, list):
        p.append("summary.json: stages is not a list")
        return b
    for s in stages:
        if not isinstance(s, dict) or not isinstance(s.get("name"), str) or s.get("status") not in STATUSES:
            p.append(f"summary.json: malformed stage entry {json.dumps(s)[:120]}")
            continue
        b["stages"].append({"name": s["name"], "status": s["status"], "reason": s.get("reason")})
    lists = {k: summary.get(k) for k in ("skipped", "allowed_skips", "unexpected_skips")}
    if not all(isinstance(v, list) and all(isinstance(n, str) for n in v) for v in lists.values()):
        p.append("summary.json: skipped, allowed_skips or unexpected_skips is not a list of names")
        return b
    skipped = {s["name"] for s in b["stages"] if s["status"] == "skipped"}
    if set(lists["skipped"]) != skipped or set(lists["allowed_skips"]) | set(lists["unexpected_skips"]) != skipped:
        p.append("summary.json: the skip lists do not match the skipped stages")
    ok = not any(s["status"] in BAD for s in b["stages"]) and not lists["unexpected_skips"]
    if summary.get("ok") is not ok:
        p.append(f"summary.json: ok is {summary.get('ok')!r} but its stages say {ok}")
    b["run_ok"] = ok and summary.get("ok") is True
    if not b["stages"]:
        p.append("summary.json: no stages ran")
    return b


def place(b, pr, git):
    """Where the bundle's build stands against the PR head: (state, commit, tree, detail)."""
    build = b["build"]
    if build is None:
        return "MALFORMED", None, None, "no usable build hash"
    if build == "unknown":
        return "UNKNOWN", None, None, "the bundle's build hash is 'unknown'"
    if build.endswith("-dirty") and HEX.fullmatch(build[:-6]):
        return "DIRTY", None, None, f"built from uncommitted changes on {build[:-6]}; no commit names that tree"
    if not HEX.fullmatch(build):
        return "MALFORMED", None, None, f"build hash {build!r} is not a commit prefix"
    commits = git.commits(build)
    if commits is None:
        return "UNKNOWN", None, None, "git could not list the commits for the build hash"
    if not commits:
        hint = " (a prefix of the PR head)" if pr and pr["headRefOid"].startswith(build) else ""
        return "UNKNOWN", None, None, f"no local commit {build}{hint}; this tool does not fetch"
    if len(commits) > 1:
        return "AMBIGUOUS", None, None, f"{build} names {len(commits)} local commits"
    commit = commits[0]
    tree = git.tree(commit)
    if pr is None:
        return "UNKNOWN", commit, tree, "the PR head is unknown"
    if commit == pr["headRefOid"]:
        return "EXACT", commit, tree, "the bundle's commit is the PR head"
    head_tree = git.tree(pr["headRefOid"])
    if head_tree is None or tree is None:
        return "UNKNOWN", commit, tree, "the PR head is not in the local repo; this tool does not fetch"
    if tree == head_tree:
        return "SAME_TREE", commit, tree, "another commit with the head's tree"
    return "STALE", commit, tree, "the bundle's tree differs from the PR head's tree"


def integration(pr, git, tree):
    """Whether the tested tree is the tree a merge of the PR would produce now: (PROVEN|DIFFERS|NOT_PROVEN, detail)."""
    head, base = pr["headRefOid"], pr["baseRefOid"]
    merge = (pr.get("potentialMergeCommit") or {}).get("oid")
    if isinstance(merge, str) and OID.fullmatch(merge):
        parents = git.parents(merge)
        if parents == [base, head]:
            mt = git.tree(merge)
            if mt == tree:
                return "PROVEN", f"the tree of GitHub's merge commit {merge[:12]} of base {base[:12]}"
            if mt:
                return "DIFFERS", f"GitHub's merge commit {merge[:12]} has another tree"
        elif parents:
            return "NOT_PROVEN", f"GitHub's merge commit {merge[:12]} is not of the current base and head"
    if git.is_ancestor(base, head) is True:
        return "PROVEN", f"the head contains the current base {base[:12]}, so a merge keeps the head's tree"
    if git.is_ancestor(base, head) is False:
        return "NOT_PROVEN", f"the head does not contain the current base {base[:12]} and no local merge commit"
    return "NOT_PROVEN", "the base, head or merge commit is not in the local repo"


def stage_verdicts(required, bundles):
    """name -> (state, detail, bundles where it passed as evidence for the head)."""
    out = {}
    for name in required:
        seen = [(b, s) for b in bundles for s in b["stages"] if s["name"] == name]
        bad = [(b, s) for b, s in seen if s["status"] in BAD]
        good = [b for b, s in seen if s["status"] == "passed" and b["tree"] in FOR_HEAD and not b["problems"]]
        if bad:
            b, s = bad[0]
            detail = f"{s['status']} in {b['path']}" + (f": {s['reason']}" if s.get("reason") else "")
            out[name] = ("FAIL", detail, [])
        elif good:
            out[name] = ("PASS", "passed in " + ", ".join(b["path"] for b in good), good)
        elif any(s["status"] == "skipped" for _, s in seen):
            out[name] = ("SKIPPED", "skipped; a skip is not a pass", [])
        elif any(b["tree"] == "UNKNOWN" and not b["problems"] for b, _ in seen):
            out[name] = ("UNKNOWN", "passed only in bundles not placed against the head", [])
        elif seen:
            out[name] = ("NOT_FOR_HEAD", "passed only in bundles that are not evidence for the head", [])
        else:
            out[name] = ("MISSING", "in none of the bundles", [])
    return out


def report(args, gh=run_gh, git_run=run_git):
    git = Git(git_run)
    rep = {"pr": args.pr, "reasons": []}
    reasons = rep["reasons"]
    pr, err = pr_facts(gh, args.pr, args.repo)
    rep["github"] = pr and {k: pr.get(k) for k in PR_FIELDS.split(",") if k != "statusCheckRollup"}
    if err:
        reasons.append(("ERROR", err))
        rep["ci"] = ("UNKNOWN", "gh pr view failed")
    else:
        rep["ci"] = ci_status(pr["statusCheckRollup"], args.check)
        rep["checks"] = [f"{c.get('workflowName') or ''}/{c.get('name') or c.get('context')}: "
                         f"{c.get('conclusion') or c.get('state') or c.get('status')}"
                         for c in pr["statusCheckRollup"] if isinstance(c, dict)]
        if pr.get("state") != "OPEN":
            reasons.append(("INCOMPLETE", f"the PR is {pr.get('state')}, not OPEN"))
    ci, detail = rep["ci"]
    if ci == "FAIL":
        reasons.append(("FAIL", f"CI {args.check}: {detail}"))
    elif ci in ("PENDING", "MISSING") or (ci == "UNKNOWN" and not err):
        reasons.append(("INCOMPLETE", f"CI {args.check} {ci.lower()}: {detail}"))

    bundles, digests = [], set()
    for path in args.bundles:
        b = read_bundle(path)
        if b["sha256"] in digests:
            continue
        if b["sha256"]:
            digests.add(b["sha256"])
        b["tree"], b["commit"], tree, b["tree_detail"] = place(b, pr, git) if not b["problems"] else (
            "MALFORMED", None, None, "; ".join(b["problems"]))
        b["base_in_bundle"] = git.is_ancestor(pr["baseRefOid"], b["commit"]) if pr and b["commit"] else None
        b["integration"] = integration(pr, git, tree) if b["tree"] in FOR_HEAD else None
        for problem in b["problems"]:
            reasons.append(("FAIL", f"{path}: {problem}"))
        if b["tree"] in ("STALE", "DIRTY", "AMBIGUOUS") or (b["tree"] == "MALFORMED" and not b["problems"]):
            reasons.append(("FAIL", f"{path}: {b['tree']}: {b['tree_detail']}"))
        elif b["tree"] == "UNKNOWN":
            reasons.append(("INCOMPLETE", f"{path}: {b['tree_detail']}"))
        if not b["problems"] and not b["run_ok"]:
            reasons.append(("FAIL", f"{path}: the run failed (a stage failed, crashed or hung, or skipped unallowed)"))
        bundles.append(b)
    rep["bundles"] = bundles

    verdicts = stage_verdicts(args.stages, bundles)
    rep["stages"] = {name: (state, detail) for name, (state, detail, _) in verdicts.items()}
    for name, (state, detail) in rep["stages"].items():
        if state != "PASS":
            reasons.append(("INCOMPLETE" if state == "UNKNOWN" else "FAIL", f"stage {name}: {state}: {detail}"))
    proven = True
    for name, (state, _, good) in verdicts.items():
        if state != "PASS":
            proven = False
            continue
        if any(b["integration"] and b["integration"][0] == "PROVEN" for b in good):
            continue
        proven = False
        states = {b["integration"][0] for b in good if b["integration"]}
        why = "; ".join(sorted(b["integration"][1] for b in good if b["integration"]))
        # A merge that makes another tree than the tested one is stale evidence; an unprovable one is only unknown.
        reasons.append(("FAIL" if "DIFFERS" in states else "INCOMPLETE",
                        f"stage {name}: not tested on the tree a merge would make now ({why}); rerun it on the "
                        f"current head merged with the base"))
    rep["integration"] = "PROVEN" if proven else "NOT_PROVEN"

    rep["review"] = []
    for path in args.review:
        try:
            rep["review"].append(f"{path} (sha256 {sha256(path)}): supplied, unverified")
        except OSError as e:
            rep["review"].append(f"{path}: unreadable ({e})")
            reasons.append(("INCOMPLETE", f"review evidence {path} is unreadable"))

    levels = {level for level, _ in reasons}
    rep["evidence"] = next((v for v in ("FAIL", "ERROR", "INCOMPLETE") if v in levels), "CONSISTENT")
    rep["merge_approval_missing"] = [
        "review: " + ("none supplied" if not args.review else "supplied evidence is unverified by this tool"),
        "serial run and host exclusivity: no bundle records its run mode",
    ] + ([] if rep["integration"] == "PROVEN" else ["integration tree: not proven for every required stage"])
    return rep


def render(rep, check):
    out = [f"Evidence report for PR #{rep['pr']}: a report, not a merge approval",
           "Facts as read now; the head, base and checks can move after this report.", ""]
    g = rep["github"]
    if g:
        merge = (g.get("potentialMergeCommit") or {}).get("oid")
        out.append(f"PR: {g.get('state')}{' draft' if g.get('isDraft') else ''}, head {g['headRefOid']}, "
                   f"base {g.get('baseRefName')} {g['baseRefOid']}, GitHub merge commit {merge or 'none'}")
        out.append(f"GitHub reviewDecision: {g.get('reviewDecision') or 'none'}")
    out.append(f"CI {check}: {rep['ci'][0]} ({rep['ci'][1]})")
    for c in rep.get("checks", []):
        out.append(f"  {c}")
    for b in rep["bundles"]:
        out += ["", f"Bundle {b['path']}", f"  sha256 {b['sha256'] or 'unreadable'}, build {b['build'] or '?'}, "
                f"recorded OS {b['os'] or '?'}"]
        out.append(f"  tree: {b['tree']}: {b['tree_detail']}" + (f" (commit {b['commit']})" if b["commit"] else ""))
        base = {True: "contains", False: "does not contain", None: "unknown whether it contains"}[b["base_in_bundle"]]
        out.append(f"  base: the bundle's commit {base} the current base")
        if b["integration"]:
            out.append(f"  integration tree: {b['integration'][0]}: {b['integration'][1]}")
        out.append("  run mode and exclusivity: unknown (not recorded in the bundle)")
        out += [f"  {s['name']}: {s['status']}" for s in b["stages"]]
    out += ["", "Required stages"]
    out += [f"  {n}: {s}: {d}" for n, (s, d) in rep["stages"].items()]
    out += ["", f"Integration tree: {rep['integration']}"]
    out.append("Review: " + ("; ".join(rep["review"]) if rep["review"] else "none supplied"))
    out += ["", f"Evidence: {rep['evidence']}"]
    out += [f"  {level}: {text}" for level, text in rep["reasons"]]
    out.append("Merge approval: not given by this tool. Missing:")
    out += [f"  {m}" for m in rep["merge_approval_missing"]]
    return "\n".join(out)


def parse(argv):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--pr", type=int, required=True, help="pull request number")
    ap.add_argument("--stage", dest="stage_lists", action="append", required=True,
                    help="required stages, comma-separated; repeatable")
    ap.add_argument("--review", action="append", default=[], help="review evidence file, recorded as unverified")
    ap.add_argument("--check", default="ci/windows", help="required check as workflow/name (default ci/windows)")
    ap.add_argument("--repo", help="owner/name for gh (default: this checkout's)")
    ap.add_argument("--json", action="store_true", help="print the report as JSON")
    ap.add_argument("bundles", nargs="+", help="run bundle zips; only these are read")
    args = ap.parse_args(argv)
    args.stages = list(dict.fromkeys(s.strip() for l in args.stage_lists for s in l.split(",") if s.strip()))
    if not args.stages:
        ap.error("--stage names no stage")
    return args


def main(argv=None, gh=run_gh, git_run=run_git):
    args = parse(argv)
    rep = report(args, gh, git_run)
    print(json.dumps(rep, indent=2) if args.json else render(rep, args.check))
    return EXIT[rep["evidence"]]


if __name__ == "__main__":
    sys.exit(main())
