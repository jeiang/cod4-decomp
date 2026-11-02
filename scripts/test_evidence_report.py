# SPDX-License-Identifier: GPL-3.0-only
"""scripts/evidence-report.py checks; run with `python3 -m unittest discover -s scripts -p 'test_*.py'`.

GitHub is a recorded fake; git is the real one, in a throwaway repo.
"""
import contextlib
import importlib.util
import io
import json
import os
import pathlib
import subprocess
import tempfile
import unittest
import zipfile
from unittest import mock

_spec = importlib.util.spec_from_file_location("evidence_report", pathlib.Path(__file__).with_name("evidence-report.py"))
er = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(er)

ZERO = "0" * 40


def bundle(path, build, stages, *, ok=None, summary_build=None, extra=None, drop=(), raw=None):
    """A bundle zip as cod4e-harness writes it; `stages` maps name -> status."""
    reports = [{"name": n, "status": s} for n, s in stages.items()]
    skipped = [n for n, s in stages.items() if s == "skipped"]
    good = not any(s in er.BAD for s in stages.values()) and not skipped
    summary = {"schema": 1, "build": summary_build or build, "ok": good if ok is None else ok,
               "skipped": skipped, "allowed_skips": [], "unexpected_skips": skipped, "stages": reports}
    manifest = {"schema": 1, "build": {"hash": build}, "os": {"family": "linux", "arch": "x86_64"}}
    files = {"manifest.json": json.dumps(manifest), "summary.json": json.dumps(summary), **(extra or {})}
    with zipfile.ZipFile(path, "w") as z:
        for name, text in files.items():
            if name not in drop:
                z.writestr(name, (raw or {}).get(name, text))
    return str(path)


class EvidenceReportTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = pathlib.Path(tmp.name)
        env = {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", "GIT_AUTHOR_NAME": "t",
               "GIT_AUTHOR_EMAIL": "t@example.invalid", "GIT_COMMITTER_NAME": "t",
               "GIT_COMMITTER_EMAIL": "t@example.invalid"}
        patcher = mock.patch.dict(os.environ, env)
        patcher.start()
        self.addCleanup(patcher.stop)
        cwd = os.getcwd()
        self.addCleanup(os.chdir, cwd)
        repo = self.dir / "repo"
        repo.mkdir()
        os.chdir(repo)
        self.git("init", "-q")
        self.base = self.commit("a.txt", "base")
        self.head = self.commit("b.txt", "head")
        head_tree = self.git("rev-parse", self.head + "^{tree}")
        base_tree = self.git("rev-parse", self.base + "^{tree}")
        # Same tree as the head, another commit.
        self.same = self.git("commit-tree", head_tree, "-p", self.base, "-m", "same tree")
        # The base moved on: a commit the head does not contain.
        self.git("checkout", "-q", "-b", "side", self.base)
        self.new_base = self.commit("c.txt", "new base")
        self.merge = self.git("commit-tree", head_tree, "-p", self.new_base, "-p", self.head, "-m", "merge")
        self.other_merge = self.git("commit-tree", base_tree, "-p", self.new_base, "-p", self.head, "-m", "merge")
        self.gh_calls, self.git_calls = [], []
        self.pr = {"number": 7, "state": "OPEN", "isDraft": False, "headRefOid": self.head,
                   "baseRefOid": self.base, "baseRefName": "main", "potentialMergeCommit": None,
                   "reviewDecision": "", "statusCheckRollup": [self.check()]}
        self.gh_result = None

    def git(self, *args):
        # On the command line, so no config file can sign: a NixOS host's git carries `commit.gpgsign` in a config
        # GIT_CONFIG_NOSYSTEM does not turn off.
        return subprocess.run(["git", "-c", "commit.gpgsign=false", *args], check=True, capture_output=True,
                              text=True).stdout.strip()

    def commit(self, name, text):
        pathlib.Path(name).write_text(text)
        self.git("add", name)
        self.git("commit", "-q", "--no-verify", "-m", text)
        return self.git("rev-parse", "HEAD")

    @staticmethod
    def check(status="COMPLETED", conclusion="SUCCESS", name="windows", workflow="ci"):
        return {"__typename": "CheckRun", "name": name, "workflowName": workflow, "status": status,
                "conclusion": conclusion}

    def gh(self, args):
        self.gh_calls.append(args)
        return self.gh_result or (0, json.dumps(self.pr), "")

    def git_run(self, args):
        self.git_calls.append(args)
        return er.run_git(args)

    def run_report(self, *bundles, stages="s1,s2", extra=()):
        argv = ["--pr", "7", "--stage", stages, "--json", *extra, *bundles]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = er.main(argv, gh=self.gh, git_run=self.git_run)
        return code, json.loads(out.getvalue())

    def good(self, name="good.zip", build=None, **kw):
        return bundle(self.dir / name, build or self.head[:12], {"s1": "passed", "s2": "passed"}, **kw)

    def reasons(self, rep):
        return " | ".join(f"{level}: {text}" for level, text in rep["reasons"])

    def test_exact_head_is_consistent_but_not_a_merge_approval(self):
        code, rep = self.run_report(self.good())
        self.assertEqual((code, rep["evidence"]), (0, "CONSISTENT"), self.reasons(rep))
        self.assertEqual(rep["bundles"][0]["tree"], "EXACT")
        self.assertEqual(rep["integration"], "PROVEN")
        self.assertTrue(rep["merge_approval_missing"])
        self.assertIn("review: none supplied", rep["merge_approval_missing"])
        self.assertTrue(any("serial" in m for m in rep["merge_approval_missing"]))
        # A `just slot` bundle looks the same as a `slot-full` one: the Linux checks stay listed as missing.
        self.assertTrue(any(m.startswith("Linux test/clippy/fmt (slot-full): not recorded")
                            for m in rep["merge_approval_missing"]), rep["merge_approval_missing"])
        text = io.StringIO()
        with contextlib.redirect_stdout(text):
            er.main(["--pr", "7", "--stage", "s1", self.good()], gh=self.gh, git_run=self.git_run)
        self.assertIn("not a merge approval", text.getvalue())
        self.assertIn("Merge approval: not given by this tool", text.getvalue())
        self.assertIn("run mode and exclusivity: unknown", text.getvalue())

    def test_supplied_review_stays_unverified(self):
        review = self.dir / "review.md"
        review.write_text("approved")
        _, rep = self.run_report(self.good(), extra=["--review", str(review)])
        self.assertIn("supplied, unverified", rep["review"][0])
        self.assertIn("review: supplied evidence is unverified by this tool", rep["merge_approval_missing"])
        _, rep = self.run_report(self.good(), extra=["--review", str(self.dir / "absent.md")])
        self.assertEqual(rep["evidence"], "INCOMPLETE")

    def test_same_tree_and_changed_tree(self):
        _, rep = self.run_report(self.good(build=self.same[:12]))
        self.assertEqual((rep["evidence"], rep["bundles"][0]["tree"]), ("CONSISTENT", "SAME_TREE"), self.reasons(rep))
        code, rep = self.run_report(self.good(build=self.base[:12]))
        self.assertEqual((code, rep["evidence"], rep["bundles"][0]["tree"]), (1, "FAIL", "STALE"))
        self.assertEqual(rep["stages"]["s1"][0], "NOT_FOR_HEAD")

    def test_integration_tree_follows_the_current_base(self):
        self.pr["baseRefOid"] = self.new_base
        code, rep = self.run_report(self.good())
        self.assertEqual(rep["bundles"][0]["integration"][0], "NOT_PROVEN")
        self.assertEqual(rep["integration"], "NOT_PROVEN")
        self.assertFalse(rep["bundles"][0]["base_in_bundle"])
        # Every stage passed on the head, yet the head lacks the current base: unknown, not consistent.
        self.assertEqual((code, rep["evidence"]), (3, "INCOMPLETE"), self.reasons(rep))
        self.assertIn("update the branch so its head contains the current base", self.reasons(rep))
        self.pr["potentialMergeCommit"] = {"oid": self.merge}
        code, rep = self.run_report(self.good())
        self.assertEqual((code, rep["integration"], rep["evidence"]), (0, "PROVEN", "CONSISTENT"), self.reasons(rep))
        # A merge that makes another tree than the tested one is stale evidence.
        self.pr["potentialMergeCommit"] = {"oid": self.other_merge}
        code, rep = self.run_report(self.good())
        self.assertEqual(rep["bundles"][0]["integration"][0], "DIFFERS")
        self.assertEqual((code, rep["evidence"]), (1, "FAIL"), self.reasons(rep))
        # A merge commit of another base proves nothing about this one.
        self.pr["baseRefOid"] = self.base
        code, rep = self.run_report(self.good())
        self.assertEqual(rep["bundles"][0]["integration"][0], "NOT_PROVEN")
        self.assertEqual((code, rep["evidence"]), (3, "INCOMPLETE"), self.reasons(rep))

    def test_the_advised_rerun_is_evidence_and_a_local_merge_of_the_old_head_is_not(self):
        # The base moved on: a bundle of the head is incomplete, and says to update the branch and rerun.
        self.pr["baseRefOid"] = self.new_base
        code, rep = self.run_report(self.good())
        self.assertEqual((code, rep["evidence"]), (3, "INCOMPLETE"), self.reasons(rep))
        self.assertIn("then rerun it on the new head", self.reasons(rep))
        # Updating the branch merges the base into the head: a new head with a new tree.
        self.git("checkout", "-q", "-b", "updated", self.head)
        self.git("merge", "-q", "--no-edit", "--no-verify", "side")
        updated = self.git("rev-parse", "HEAD")
        # A bundle of that merge while the PR head is still the old one is another tree: not evidence for the head.
        code, rep = self.run_report(self.good("merge.zip", build=updated[:12]))
        self.assertEqual((code, rep["bundles"][0]["tree"]), (1, "STALE"), self.reasons(rep))
        # Once the PR head is the updated commit, the rerun on it is consistent, and the old head's bundle is stale.
        self.pr["headRefOid"] = updated
        code, rep = self.run_report(self.good("rerun.zip", build=updated[:12]))
        self.assertEqual((code, rep["evidence"], rep["integration"]), (0, "CONSISTENT", "PROVEN"), self.reasons(rep))
        code, rep = self.run_report(self.good("old.zip"))
        self.assertEqual((code, rep["bundles"][0]["tree"]), (1, "STALE"), self.reasons(rep))

    def test_failed_missing_and_skipped_stages(self):
        failed = bundle(self.dir / "f.zip", self.head[:12], {"s1": "failed", "s2": "passed"})
        code, rep = self.run_report(failed)
        self.assertEqual((code, rep["stages"]["s1"][0]), (1, "FAIL"))
        hung = bundle(self.dir / "h.zip", self.head[:12], {"s1": "passed", "s2": "passed", "s3": "hung"})
        _, rep = self.run_report(hung)
        self.assertEqual(rep["evidence"], "FAIL", "a bad unrequired stage still fails the run")
        _, rep = self.run_report(bundle(self.dir / "m.zip", self.head[:12], {"s1": "passed"}))
        self.assertEqual((rep["evidence"], rep["stages"]["s2"][0]), ("FAIL", "MISSING"))
        skipped = bundle(self.dir / "s.zip", self.head[:12], {"s1": "passed", "s2": "skipped"})
        _, rep = self.run_report(skipped)
        self.assertEqual((rep["evidence"], rep["stages"]["s2"][0]), ("FAIL", "SKIPPED"))
        # Even a skip the run allowed is not a pass.
        allowed = bundle(self.dir / "a.zip", self.head[:12], {"s1": "passed", "s2": "skipped"}, ok=True, raw={
            "summary.json": json.dumps({"schema": 1, "build": self.head[:12], "ok": True, "skipped": ["s2"],
                                        "allowed_skips": ["s2"], "unexpected_skips": [],
                                        "stages": [{"name": "s1", "status": "passed"},
                                                   {"name": "s2", "status": "skipped"}]})})
        _, rep = self.run_report(allowed)
        self.assertEqual((rep["evidence"], rep["stages"]["s2"][0]), ("FAIL", "SKIPPED"))

    def test_stages_from_two_bundles(self):
        a = bundle(self.dir / "1.zip", self.head[:12], {"s1": "passed"})
        b = bundle(self.dir / "2.zip", self.same[:12], {"s2": "passed"})
        _, rep = self.run_report(a, b)
        self.assertEqual(rep["evidence"], "CONSISTENT", self.reasons(rep))

    def test_dirty_unknown_and_ambiguous_hashes(self):
        _, rep = self.run_report(self.good(build=self.head[:12] + "-dirty"))
        self.assertEqual((rep["evidence"], rep["bundles"][0]["tree"]), ("FAIL", "DIRTY"))
        _, rep = self.run_report(self.good(build="unknown"))
        self.assertEqual((rep["evidence"], rep["bundles"][0]["tree"]), ("INCOMPLETE", "UNKNOWN"))
        _, rep = self.run_report(self.good(build="f" * 12))
        self.assertEqual((rep["evidence"], rep["bundles"][0]["tree"]), ("INCOMPLETE", "UNKNOWN"))
        self.assertEqual(rep["stages"]["s1"][0], "UNKNOWN")
        real = self.git_run

        def two_commits(args):
            if args[0] == "rev-parse" and args[1].startswith("--disambiguate="):
                return 0, f"{self.head}\n{self.same}\n", ""
            return real(args)

        self.git_run = two_commits
        _, rep = self.run_report(self.good())
        self.assertEqual((rep["evidence"], rep["bundles"][0]["tree"]), ("FAIL", "AMBIGUOUS"))

    def test_head_not_in_the_local_repo(self):
        self.pr["headRefOid"] = "1" * 40
        _, rep = self.run_report(self.good(build=self.same[:12]))
        self.assertEqual((rep["evidence"], rep["bundles"][0]["tree"]), ("INCOMPLETE", "UNKNOWN"))

    def test_malformed_bundles(self):
        cases = {
            "notzip.zip": None,
            "nosummary.zip": dict(drop=("summary.json",)),
            "badjson.zip": dict(raw={"summary.json": "{"}),
            "mismatch.zip": dict(summary_build="0" * 12),
            "liar.zip": dict(ok=True, build=self.head[:12]),
        }
        for name, kw in cases.items():
            path = self.dir / name
            if kw is None:
                path.write_bytes(b"not a zip")
            elif name == "liar.zip":
                bundle(path, self.head[:12], {"s1": "passed", "s2": "failed"}, ok=True)
            else:
                self.good(name, **kw)
            code, rep = self.run_report(str(path))
            self.assertEqual((code, rep["evidence"]), (1, "FAIL"), name)
            self.assertTrue(rep["bundles"][0]["problems"], name)
            self.assertNotEqual(rep["stages"]["s1"][0], "PASS", name)
        code, rep = self.run_report(str(self.dir / "absent.zip"))
        self.assertEqual((code, rep["bundles"][0]["tree"]), (1, "MALFORMED"))

    def test_ci_states(self):
        for rollup, want in [
            ([], "MISSING"),
            ([self.check(name="linux")], "MISSING"),
            ([self.check(status="IN_PROGRESS", conclusion="")], "PENDING"),
            ([self.check(conclusion="FAILURE")], "FAIL"),
            ([self.check(conclusion="SKIPPED")], "FAIL"),
            ([self.check(), self.check(conclusion="CANCELLED")], "FAIL"),
        ]:
            self.pr["statusCheckRollup"] = rollup
            code, rep = self.run_report(self.good())
            self.assertEqual(rep["ci"][0], want, rollup)
            self.assertEqual(rep["evidence"], "FAIL" if want == "FAIL" else "INCOMPLETE", rollup)
            self.assertNotEqual(code, 0)

    def test_gh_failure_is_never_a_pass(self):
        for result in [(1, "", "HTTP 502"), (0, "not json", ""), (0, json.dumps({"number": 7}), ""),
                       (0, json.dumps({**self.pr, "number": 8}), "")]:
            self.gh_result = result
            code, rep = self.run_report(self.good())
            self.assertEqual((code, rep["evidence"]), (4, "ERROR"), result)
            self.assertEqual(rep["bundles"][0]["tree"], "UNKNOWN")
            self.assertEqual(rep["integration"], "NOT_PROVEN")

    def test_only_read_only_commands_run(self):
        self.pr["potentialMergeCommit"] = {"oid": self.merge}
        self.run_report(self.good(), self.good("stale.zip", build=self.base[:12]))
        self.assertTrue(self.gh_calls and self.git_calls)
        self.assertTrue(all(tuple(c[:2]) == ("pr", "view") for c in self.gh_calls))
        self.assertTrue(all(c[0] in er.GIT_READS for c in self.git_calls))
        for refused in (["pr", "merge", "7"], ["api", "-X", "POST", "x"], ["pr", "comment", "7"]):
            with self.assertRaises(er.Refused):
                er.run_gh(refused)
        for refused in (["fetch"], ["push"], ["merge", self.head], ["update-ref", "HEAD", self.base], []):
            with self.assertRaises(er.Refused):
                er.run_git(refused)


if __name__ == "__main__":
    unittest.main()
