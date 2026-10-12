// SPDX-License-Identifier: GPL-3.0-only
// `node --test web/`: which files of an install the page reads, per language (run by crates/harness/tests/web_install.rs).
import assert from "node:assert/strict";
import test from "node:test";
import { choose, languageOf, required, wanted } from "./install.js";

const file = (path) => [path, {}];
const install = (language, others = []) => [
  file("localization.txt"),
  file("main/iw_00.iwd"),
  ...required(language).slice(1).map(file),
  ...others.map(file),
];

test("the language is line 1 of localization.txt, english when missing or unknown", () => {
  assert.equal(languageOf("german\r\nother"), "german");
  assert.equal(languageOf(" French \n"), "french");
  assert.equal(languageOf("klingon"), "english");
  assert.equal(languageOf(""), "english");
  assert.equal(languageOf(undefined), "english");
});

test("a non-english install is read from its own zone folder", () => {
  const found = install("german", ["zone/english/ui_mp.ff", "Zone/German/mp_crash.ff", "zone/german/mp_crash_load.ff"]);
  const { language, entries, missing } = choose(found, "german");
  assert.equal(language, "german");
  assert.deepEqual(missing, []);
  const paths = entries.map(([p]) => p);
  assert.ok(paths.includes("zone/german/ui_mp.ff") && paths.includes("Zone/German/mp_crash.ff"));
  assert.ok(!paths.some((p) => p.toLowerCase().startsWith("zone/english/")), "the other language is left out");
  assert.ok(!paths.includes("zone/german/mp_crash_load.ff"), "a loading screen zone is not read");
});

test("an install without its language's zones says what is missing, whatever else it holds", () => {
  const { missing } = choose(install("english"), "german");
  assert.deepEqual(missing, required("german").slice(1));
  assert.deepEqual(choose(install("english"), "").missing, []);
});

test("while walking any language's zones pass, and nothing else in zone/", () => {
  assert.ok(wanted("zone/czech/common_mp.ff") && wanted("zone/english/common_mp.ff"));
  assert.ok(!wanted("zone/english/mp_x_load.ff") && !wanted("zone/klingon/common_mp.ff") && !wanted("zone/english/xx.ff"));
  assert.ok(!wanted("zone/german/ui_mp.ff", "english") && wanted("zone/german/ui_mp.ff", "german"));
});
