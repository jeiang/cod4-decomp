// SPDX-License-Identifier: GPL-3.0-only
//! What the menus draw from: the boot zones' menu lists, fonts, localized strings, string tables and materials.
//!
//! The original loads `code_post_gfx_mp`, `localized_code_post_gfx_mp`, `ui_mp`, `common_mp` and
//! `localized_common_mp` at boot, then the running mod's `mod.ff` (`fs_game`); the UI reads its assets from that set,
//! by lower-case name, a later zone's replacing an earlier one's.

use assets::zone::gfx::Material;
use assets::zone::menu::MenuDef;
use assets::zone::text::{Font, StringTable};
use assets::zone::{Asset, XAssetType, Zone};
use server::content::Install;
use std::collections::HashMap;
use std::sync::Arc;

/// Zones the menus come from, in boot order.
pub const UI_ZONES: [&str; 5] = [
    "code_post_gfx_mp",
    "localized_code_post_gfx_mp",
    "ui_mp",
    "common_mp",
    "localized_common_mp",
];

#[derive(Default)]
pub struct UiAssets {
    /// Lower-case menu name to definition; a later zone's menu replaces an earlier one.
    pub menus: HashMap<String, Arc<MenuDef>>,
    /// Menu names in load order.
    pub menu_order: Vec<String>,
    /// The always-on in-game HUD menus (`ui_mp/hud.txt`), in draw order.
    pub hud_order: Vec<String>,
    /// Lower-case font name (`fonts/normalfont`).
    pub fonts: HashMap<String, Arc<Font>>,
    /// Localize key (upper case) to text.
    pub localize: HashMap<String, Arc<str>>,
    /// Lower-case table name (`mp/mapstable.csv`).
    pub tables: HashMap<String, Arc<StringTable>>,
    /// Lower-case material name, without a leading `,`.
    pub materials: HashMap<String, Arc<Material>>,
    /// Lower-case map name to the game modes its `.arena` file allows (lower case); a map without an entry or with
    /// none listed allows every mode.
    pub arenas: HashMap<String, Vec<String>>,
}

/// The maps of an `.arena` file (blocks of `key value` lines) and the game modes each names.
fn parse_arenas(text: &str) -> Vec<(String, Vec<String>)> {
    text.split('}')
        .filter_map(|block| {
            let get = |key: &str| {
                block.lines().find_map(|l| {
                    let (k, v) = l.trim().split_once(char::is_whitespace)?;
                    k.eq_ignore_ascii_case(key)
                        .then(|| v.trim().trim_matches('"').to_ascii_lowercase())
                })
            };
            let modes = get("gametype").unwrap_or_default();
            Some((
                get("map")?,
                modes.split_whitespace().map(str::to_owned).collect(),
            ))
        })
        .collect()
}

/// The asset kinds the UI keeps from a zone.
struct UiFilter;

impl assets::zone::DecodeFilter for UiFilter {
    fn keep(&self, ty: XAssetType) -> bool {
        use XAssetType::*;
        matches!(
            ty,
            MenuList | Font | Localize | StringTable | Material | RawFile
        )
    }
}

impl UiAssets {
    pub fn load(install: &Install) -> Result<Self, String> {
        let mut a = Self::default();
        for zone in UI_ZONES {
            a.load_zone(install, zone)?;
        }
        if let Some(path) = ::assets::paths::mod_zone(&install.root, &install.game) {
            a.load_file(&path, "mod")?;
        }
        Ok(a)
    }

    /// Adds the icon materials the weapons carry (HUD, ammunition counter, kill feed, d-pad): most are defined
    /// by the weapon that first names them and no UI zone lists them.
    pub fn add_weapon_icons(&mut self, weapons: &[Arc<assets::zone::weapon::WeaponDef>]) {
        for w in weapons {
            for m in [
                &w.hud_icon,
                &w.ammo_counter_icon,
                &w.kill_icon,
                &w.dpad_icon,
            ]
            .into_iter()
            .flatten()
            {
                self.add_material(m.clone());
            }
        }
    }

    /// Adds the menus, fonts, strings and materials of `zone`.
    pub fn load_zone(&mut self, install: &Install, zone: &str) -> Result<(), String> {
        let path = install
            .zone_path(zone)
            .ok_or_else(|| format!("Could not find zone '{zone}'"))?;
        self.load_file(&path, zone)
    }

    fn load_file(&mut self, path: &std::path::Path, zone: &str) -> Result<(), String> {
        let file = ::assets::fs::buffered(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let z = Zone::open(file).map_err(|e| format!("{zone}: {e}"))?;
        z.decode(&UiFilter, |a| self.add(a))
            .map_err(|e| format!("{zone}: {e}"))?;
        tracing::debug!(
            zone,
            menus = self.menu_order.len(),
            fonts = self.fonts.len(),
            strings = self.localize.len(),
            materials = self.materials.len(),
            "ui zone loaded"
        );
        Ok(())
    }

    fn add(&mut self, asset: Asset) {
        match asset {
            Asset::MenuList(l) => {
                let is_hud = l
                    .name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case("ui_mp/hud.txt"));
                for m in l.menus.iter().flatten() {
                    if let Some(n) = &m.window.name {
                        let k = n.to_ascii_lowercase();
                        if is_hud && !self.hud_order.contains(&k) {
                            self.hud_order.push(k.clone());
                        }
                        if self.menus.insert(k.clone(), m.clone()).is_none() {
                            self.menu_order.push(k);
                        }
                    }
                }
            }
            Asset::Font(f) => {
                // The font carries its own (and its glow) material; no zone lists them as assets.
                for m in [&f.material, &f.glow_material].into_iter().flatten() {
                    self.add_material(m.clone());
                }
                if let Some(n) = &f.name {
                    self.fonts.insert(n.to_ascii_lowercase(), f);
                }
            }
            Asset::Localize(l) => {
                if let (Some(n), Some(v)) = (&l.name, &l.value) {
                    self.localize.insert(n.to_ascii_uppercase(), v.clone());
                }
            }
            Asset::StringTable(t) => {
                if let Some(n) = &t.name {
                    self.tables.insert(n.to_ascii_lowercase(), t);
                }
            }
            Asset::Material(m) => self.add_material(m),
            Asset::RawFile(r) if r.name.as_deref().is_some_and(|n| n.ends_with(".arena")) => {
                self.arenas
                    .extend(parse_arenas(&String::from_utf8_lossy(&r.data)));
            }
            _ => {}
        }
    }

    fn add_material(&mut self, m: Arc<Material>) {
        let Some(n) = &m.name else { return };
        let k = n.trim_start_matches(',').to_ascii_lowercase();
        // A name with a leading comma marks a copy of a material another zone owns; keep the owner's.
        let copy = n.starts_with(',');
        if !copy || !self.materials.contains_key(&k) {
            self.materials.insert(k, m);
        }
    }

    /// Keeps the materials the menus reference inline (items and windows carry their own copy).
    pub fn note_menu_materials(&mut self) {
        let menus: Vec<Arc<MenuDef>> = self.menus.values().cloned().collect();
        for m in menus {
            if let Some(b) = &m.window.background {
                self.add_material(b.clone());
            }
            for it in &m.items {
                if let Some(b) = &it.window.background {
                    self.add_material(b.clone());
                }
            }
        }
    }

    pub fn material(&self, name: &str) -> Option<&Arc<Material>> {
        self.materials
            .get(&name.trim_start_matches(',').to_ascii_lowercase())
    }

    pub fn table(&self, name: &str) -> Option<&Arc<StringTable>> {
        self.tables.get(&name.to_ascii_lowercase())
    }

    /// The text of a localize reference: `@KEY` or `KEY` (the original's `SEH_StringEd_GetString`); `None` if unknown.
    pub fn translate(&self, key: &str) -> Option<&Arc<str>> {
        self.localize
            .get(&key.trim_start_matches('@').to_ascii_uppercase())
    }

    /// [`UiAssets::translate`] for a reference the data means to resolve (`@KEY`): a miss shows the key and is
    /// logged once per key.
    pub fn translate_ref(&self, key: &str) -> Option<&Arc<str>> {
        let t = self.translate(key);
        if t.is_none() {
            logging::dedup!(
                WARN,
                key,
                key,
                "localized string is missing, showing its key"
            );
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install() -> Option<Install> {
        let root =
            std::env::var_os("COD4_PATH").map_or_else(|| "COD4".into(), std::path::PathBuf::from);
        Install::open(&root)
            .ok()
            .filter(|i| i.zone_path("ui_mp").is_some())
    }

    #[test]
    fn the_boot_zones_give_the_stock_menus_fonts_and_strings() {
        let Some(i) = install() else { return };
        let mut a = UiAssets::load(&i).unwrap();
        a.note_menu_materials();
        for m in [
            "main",
            "class",
            "team_marinesopfor",
            "scoreboard",
            "killcam",
            "popup_leavegame",
            "pc_join_unranked",
            "createserver",
        ] {
            assert!(a.menus.contains_key(m), "menu {m}");
        }
        assert!(a.fonts.contains_key("fonts/normalfont"));
        assert!(a.fonts.contains_key("fonts/bigfont"));
        assert!(a.translate("@MENU_JOIN_GAME").is_some());
        assert!(a.table("mp/mapstable.csv").is_some());
        assert!(a.material("white").is_some(), "white");
    }

    /// The d-pad and grenade icons are materials only the weapons define; the UI must be able to draw them.
    #[test]
    fn weapon_icons_resolve_after_registering_the_weapons() {
        let Some(i) = install() else { return };
        let mut content = server::content::Content::for_client();
        content.load_zone(&i, "code_post_gfx_mp", 2).unwrap();
        content.load_zone(&i, "common_mp", 4).unwrap();
        let mut a = UiAssets::load(&i).unwrap();
        assert!(
            a.material("hud_icon_40mm_grenade_mp")
                .is_none_or(|m| m.textures.is_empty()),
            "the UI zones do not carry it"
        );
        a.add_weapon_icons(&content.weapons());
        for name in ["hud_icon_40mm_grenade_mp", "hud_icon_rpg_dpad"] {
            let m = a.material(name).unwrap_or_else(|| panic!("{name}"));
            assert!(!m.textures.is_empty(), "{name} has its image");
        }
    }

    #[test]
    fn a_localized_reference_that_is_missing_warns_with_its_key() {
        let (none, events) = logging::capture(|| {
            UiAssets::default()
                .translate_ref("@NO_SUCH_KEY_TEST")
                .is_none()
        });
        assert!(none);
        let e = events
            .iter()
            .find(|e| e.level == tracing::Level::WARN)
            .expect("a warning");
        assert_eq!(e.field("key"), Some("@NO_SUCH_KEY_TEST"));
    }
}
