// SPDX-License-Identifier: GPL-3.0-only
// Which files of the user's install the client reads. The install's language is line 1 of localization.txt (default
// english), as in crates/assets/src/paths.rs; the client reads that language's zones, so the page picks the same ones.

// The original engine's languages (crates/assets/src/vfs/mod.rs), one `zone/<language>` folder each.
export const LANGUAGES = [
  "english", "french", "german", "italian", "spanish", "british", "russian", "polish", "korean", "taiwanese",
  "japanese", "chinese", "thai", "leet", "czech",
];

const ci = (s) => s.toLowerCase();
const ZONES = /^(code_post_gfx|code_post_gfx_mp|localized_code_post_gfx_mp|ui_mp|common_mp|localized_common_mp)$/;
const MAP_ZONE = /^mp_[a-z0-9_]+$/;

/** The language localization.txt names; an unknown or missing one is english, as the client reads it. */
export function languageOf(localizationText) {
  const first = String(localizationText ?? "").split("\n")[0].trim().toLowerCase();
  return LANGUAGES.includes(first) ? first : "english";
}

/** Whether `name` is a `zone/<language>` folder the page should look into (the language is not known until
 * localization.txt is read, so every language's folder is walked). */
export const isLanguageDir = (name) => LANGUAGES.includes(ci(name));

/** The language of an install-relative `zone/<language>/<zone>.ff` path the client could read, else null. */
export function zoneLanguage(path) {
  const m = /^zone\/([^/]+)\/([^/]+)\.ff$/.exec(ci(path));
  if (!m || !LANGUAGES.includes(m[1])) return null;
  const zone = m[2];
  return ZONES.test(zone) || (MAP_ZONE.test(zone) && !zone.endsWith("_load")) ? m[1] : null;
}

/** Whether the client reads `path` (install-relative, `/`-separated) of an install in `language`. Pass no language to
 * accept any one's zones (while walking, before localization.txt is known). */
export function wanted(path, language) {
  const p = ci(path);
  if (p === "localization.txt" || /^main\/[^/]+\.iwd$/.test(p)) return true;
  const zl = zoneLanguage(p);
  return zl !== null && (language === undefined || zl === language);
}

/** The files without which the client cannot start, in `language`. */
export const required = (language) => [
  "main/iw_00.iwd",
  `zone/${language}/common_mp.ff`,
  `zone/${language}/code_post_gfx_mp.ff`,
  `zone/${language}/ui_mp.ff`,
];

/**
 * What to do with the files found in a picked folder: `found` is `[[path, File], ...]` and `localization` the text of
 * its localization.txt (or ""). Returns the language, the entries to read (that language's zones only) and the
 * required files that are missing.
 */
export function choose(found, localization) {
  const language = languageOf(localization);
  const entries = found.filter(([p]) => wanted(p, language));
  const have = new Set(entries.map(([p]) => ci(p)));
  return { language, entries, missing: required(language).filter((n) => !have.has(n)) };
}
