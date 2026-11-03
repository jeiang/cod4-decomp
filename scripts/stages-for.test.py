#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Behaviour of scripts/stages-for.py and scripts/stage-map.txt: `python3 scripts/stages-for.test.py` (or
`just script-tests`). The rule mechanics run in a throwaway repository with a small stage.rs and its own map; the
real map is checked against this checkout's HEAD and against the merges of #520 and #547, the two stage lists that
broke main (skipped when that history is absent)."""

import importlib.util
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS, "stages-for.py")
REPO = os.path.dirname(SCRIPTS)

_spec = importlib.util.spec_from_file_location("stages_for", SCRIPT)
sf = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sf)

DEFAULTS = ["asset-load", "headless-bots", "net-a", "client-x", "client-load", "client-match", "client-flythrough",
            "web-y"]
ALL_DEFAULTS = ",".join(DEFAULTS)
TIMING = ["client-load", "client-match", "client-flythrough", "headless-bots-32"]

STAGE_RS = """
pub static STAGES: &[StageDef] = &[
    StageDef {
        name: "asset-load",
        default: true,
        kind: Kind::Builtin(crate::stages::asset_load::run),
    },
    StageDef {
        name: "headless-bots",
        default: true,
        kind: Kind::Server(include_str!("../scenarios/headless-bots.cfg")),
    },
    StageDef {
        name: "net-a",
        default: true,
        kind: Kind::Builtin(crate::stages::net_a::run),
    },
    StageDef {
        name: "client-x",
        default: true,
        kind: Kind::Builtin(crate::stages::client_x::run),
    },
    StageDef {
        name: "client-load",
        default: true,
        kind: Kind::Builtin(crate::stages::client_load::run),
    },
    StageDef {
        name: "client-match",
        default: true,
        kind: Kind::Builtin(crate::stages::client_match::run),
    },
    StageDef {
        name: "client-flythrough",
        default: true,
        kind: Kind::Builtin(crate::stages::client_flythrough::run),
    },
    StageDef {
        name: "web-y",
        default: true,
        kind: Kind::Builtin(crate::stages::web_y::run),
    },
    StageDef {
        name: "headless-bots-32",
        default: false,
        kind: Kind::Server(include_str!("../scenarios/headless-bots-32.cfg")),
    },
    StageDef {
        name: "selftest-crash",
        default: false,
        kind: Kind::Builtin(crate::stages::selftest::crash),
    },
];
"""

MODULES = {
    "mod.rs": "pub mod asset_load;\n",
    "asset_load.rs": "use assets::Zone;\n",
    "net_a.rs": "use master::Master;\nuse std::net::UdpSocket;\n",
    "client_x.rs": "use crate::control::Driven;\n",
    "client_load.rs": "use super::helper::locate_client;\n",
    "client_match.rs": "use super::{helper, unrelated_fn};\n",
    "client_flythrough.rs": "use crate::stages::helper;\n",
    "helper.rs": "pub(crate) fn locate_client() {}\n",
    "web_y.rs": "use super::browser_cdp;\n",
    "browser_cdp.rs": "// CDP\n",
    "server.rs": "use server::server::Server;\n",
    "selftest.rs": "pub fn crash() {}\n",
}

MAP = """\
# test map
timing: client-load client-match client-flythrough headless-bots-32
no-client: asset-load net-a
no-client: headless-bots
shared.rs                        @full
crates/harness/src/stages/*.rs   @module
crates/harness/scenarios/*.cfg   @module
crates/*/tests/**                @none
**/*.md                          @none
crates/client/**                 @client @web +timing
crates/master/**                 @uses:master +timing
web/**                           @web
docs/**                          @none
named.txt                        net-a
globbed.txt                      client-*
"""


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout


def write(root, path, text):
    full = os.path.join(root, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w") as f:
        f.write(text)


class Mechanics(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.repo = tmp.name
        self.map = os.path.join(self.repo, "map.txt")
        with open(self.map, "w") as f:
            f.write(MAP)
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.email", "t@example.invalid")
        git(self.repo, "config", "user.name", "t")
        git(self.repo, "config", "commit.gpgsign", "false")
        write(self.repo, "crates/harness/src/stage.rs", STAGE_RS)
        for name, text in MODULES.items():
            write(self.repo, f"crates/harness/src/stages/{name}", text)
        for cfg in ("headless-bots.cfg", "headless-bots-32.cfg"):
            write(self.repo, f"crates/harness/scenarios/{cfg}", "map mp_crash\n")
        write(self.repo, "docs/a.md", "a\n")
        self.commit("base")
        self.base = git(self.repo, "rev-parse", "HEAD").strip()

    def commit(self, message):
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", message)

    def change(self, *paths):
        for p in paths:
            full = os.path.join(self.repo, p)
            old = open(full).read() if os.path.exists(full) else ""
            write(self.repo, p, old + "// changed\n")
        self.commit("change")

    def run_tool(self, base=None, head="HEAD", map_path=None):
        return subprocess.run([sys.executable, SCRIPT, base or self.base, head, "--map", map_path or self.map],
                              cwd=self.repo, capture_output=True, text=True)

    def stages(self, *paths):
        before = git(self.repo, "rev-parse", "HEAD").strip()
        self.change(*paths)
        p = self.run_tool(base=before)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout.strip()

    def test_shared_file_selects_the_full_default_suite(self):
        self.assertEqual(self.stages("shared.rs"), ALL_DEFAULTS)

    def test_unmapped_path_selects_the_full_default_suite(self):
        self.change("brand/new.file")
        p = self.run_tool()
        self.assertEqual(p.stdout.strip(), ALL_DEFAULTS)
        self.assertIn("brand/new.file: unmapped -> full default suite", p.stderr)

    def test_docs_only_select_nothing(self):
        self.assertEqual(self.stages("docs/a.md", "crates/client/README.md", "crates/client/tests/t.rs"), "")

    def test_a_stage_module_selects_its_stage(self):
        self.assertEqual(self.stages("crates/harness/src/stages/net_a.rs"), "net-a")

    def test_a_helper_selects_every_stage_that_names_it(self):
        # `super::helper::`, `super::{helper, ...}` and `crate::stages::helper` all include it.
        self.assertEqual(self.stages("crates/harness/src/stages/helper.rs"),
                         "client-load,client-match,client-flythrough")

    def test_scenarios_and_the_server_module_select_the_scenario_stages(self):
        self.assertEqual(self.stages("crates/harness/scenarios/headless-bots.cfg"), "headless-bots")
        self.assertEqual(self.stages("crates/harness/src/stages/server.rs"), "headless-bots,headless-bots-32")

    def test_a_module_no_stage_includes_selects_the_full_suite(self):
        self.assertEqual(self.stages("crates/harness/src/stages/orphan.rs"), ALL_DEFAULTS)

    def test_a_client_change_selects_every_stage_but_no_client_and_adds_the_timing_set(self):
        self.assertEqual(self.stages("crates/client/src/app.rs"),
                         "client-x,client-load,client-match,client-flythrough,web-y,headless-bots-32")

    def test_a_new_stage_is_client_until_listed_otherwise(self):
        write(self.repo, "crates/harness/src/stage.rs", STAGE_RS.replace(
            "];", '    StageDef {\n        name: "net-new",\n        default: true,\n'
                  '        kind: Kind::Builtin(crate::stages::net_new::run),\n    },\n];'))
        write(self.repo, "crates/harness/src/stages/net_new.rs", "\n")
        self.commit("new stage")
        base = git(self.repo, "rev-parse", "HEAD").strip()
        self.change("crates/client/src/app.rs")
        p = self.run_tool(base=base)
        self.assertIn("net-new", p.stdout.strip().split(","))

    def test_uses_selects_the_stages_calling_the_crate_and_the_timing_set(self):
        self.assertEqual(self.stages("crates/master/src/lib.rs"),
                         "net-a,client-load,client-match,client-flythrough,headless-bots-32")

    def test_web_and_named_and_globbed_stages(self):
        self.assertEqual(self.stages("web/main.js"), "web-y")
        self.assertEqual(self.stages("named.txt"), "net-a")
        self.assertEqual(self.stages("globbed.txt"), "client-x,client-load,client-match,client-flythrough")

    def test_a_rename_counts_the_old_and_the_new_path(self):
        os.makedirs(os.path.join(self.repo, "web"))
        git(self.repo, "mv", "docs/a.md", "web/a.js")
        self.commit("rename")
        p = self.run_tool()
        self.assertEqual(p.stdout.strip(), "web-y")
        self.assertIn("docs/a.md: ", p.stderr)
        self.assertIn("web/a.js: ", p.stderr)

    def test_only_the_heads_own_change_counts(self):
        git(self.repo, "checkout", "-q", "-b", "topic")
        self.change("docs/a.md")
        git(self.repo, "checkout", "-q", "main")
        self.change("shared.rs")
        p = self.run_tool(base="main", head="topic")
        self.assertEqual((p.returncode, p.stdout), (0, "\n"))

    def test_a_stale_map_is_an_error(self):
        for line in ("ghost.txt ghost-stage\n", "no-client: ghost-stage\n", "timing: ghost-stage\n"):
            with self.subTest(line=line):
                bad = os.path.join(self.repo, "bad.txt")
                with open(bad, "w") as f:
                    f.write(MAP + line)
                self.change("docs/a.md")
                p = self.run_tool(map_path=bad)
                self.assertEqual(p.returncode, 2)
                self.assertIn("ghost-stage", p.stderr)
                self.assertEqual(p.stdout, "")

    def test_a_bad_ref_is_an_error(self):
        p = self.run_tool(base="no-such-ref")
        self.assertEqual((p.returncode, p.stdout), (2, ""))


def have(commit):
    return subprocess.run(["git", "cat-file", "-e", f"{commit}^{{commit}}"], cwd=REPO,
                          capture_output=True).returncode == 0


class RealMap(unittest.TestCase):
    """The checked-in map against this checkout's HEAD."""

    @classmethod
    def setUpClass(cls):
        cls.map = sf.Map()
        cls.stages = sf.stage_facts("HEAD", cwd=REPO)

    def test_names_stages_that_exist(self):
        self.map.check(self.stages)

    def test_every_tracked_path_has_a_rule(self):
        paths = git(REPO, "ls-tree", "-r", "--name-only", "HEAD").split("\n")
        unmapped = [p for p in paths if p and not any(r.regex.match(p) for r in self.map.rules)]
        self.assertEqual(unmapped, [])

    def test_every_engine_crate_adds_the_timing_set(self):
        crates = git(REPO, "ls-tree", "--name-only", "HEAD:crates").split()
        for c in crates:
            if c == "harness":
                continue
            rule = next(r for r in self.map.rules if r.regex.match(f"crates/{c}/src/lib.rs"))
            self.assertIn("+timing", rule.selections, c)

    def test_no_client_stages_do_not_start_a_client(self):
        markers = ("locate_client", "crate::control", "COD4E_CLIENT", "browser_cdp")
        by_name = {s.name: s for s in self.stages}
        for name in self.map.no_client:
            s = by_name[name]
            self.assertTrue(s.default, name)
            self.assertFalse([m for m in markers if m in s.text], name)

    def run_real(self, merge):
        if not have(merge):
            self.skipTest(f"{merge} is not in this clone")
        p = subprocess.run([sys.executable, SCRIPT, f"{merge}^", merge], cwd=REPO, capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout.strip().split(",")

    def test_pr_520_selects_net_info_and_client_host_console(self):
        # #520's hand-picked list left both out, and its logcheck max then failed on main (#549).
        got = self.run_real("0c6a4a3c4c325101c750739be82d9282d7ae4a8e")
        self.assertIn("net-info", got)
        self.assertIn("client-host-console", got)

    def test_pr_547_selects_client_session(self):
        # #547's hand-picked list left it out, and client-session stayed red on main for about 6 h.
        self.assertIn("client-session", self.run_real("701a67a580a7127117cb05f0b45fe0040da59428"))

    def test_a_client_file_alone_selects_client_session(self):
        m, stages = self.map, self.stages
        chosen, _ = sf.select(["crates/client/src/app.rs"], m, stages)
        self.assertIn("client-session", chosen)
        self.assertTrue(set(m.timing) <= chosen)


if __name__ == "__main__":
    unittest.main()
