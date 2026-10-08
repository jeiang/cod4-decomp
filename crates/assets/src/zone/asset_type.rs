// SPDX-License-Identifier: GPL-3.0-only
//! The original engine's `XAssetType` numbering (IW3 1.7).

/// Asset type tag stored in each entry of the zone's asset list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u32)]
pub enum XAssetType {
    XModelPieces = 0,
    PhysPreset,
    XAnimParts,
    XModel,
    Material,
    TechniqueSet,
    Image,
    Sound,
    SoundCurve,
    LoadedSound,
    Clipmap,
    ClipmapPvs,
    ComWorld,
    GameWorldSp,
    GameWorldMp,
    MapEnts,
    GfxWorld,
    LightDef,
    UiMap,
    Font,
    MenuList,
    Menu,
    Localize,
    Weapon,
    SndDriverGlobals,
    Fx,
    ImpactFx,
    AiType,
    MpType,
    Character,
    XModelAlias,
    RawFile,
    StringTable,
}

impl XAssetType {
    pub const COUNT: usize = 33;

    const ALL: [XAssetType; Self::COUNT] = {
        use XAssetType::*;
        [
            XModelPieces,
            PhysPreset,
            XAnimParts,
            XModel,
            Material,
            TechniqueSet,
            Image,
            Sound,
            SoundCurve,
            LoadedSound,
            Clipmap,
            ClipmapPvs,
            ComWorld,
            GameWorldSp,
            GameWorldMp,
            MapEnts,
            GfxWorld,
            LightDef,
            UiMap,
            Font,
            MenuList,
            Menu,
            Localize,
            Weapon,
            SndDriverGlobals,
            Fx,
            ImpactFx,
            AiType,
            MpType,
            Character,
            XModelAlias,
            RawFile,
            StringTable,
        ]
    };

    pub fn from_u32(v: u32) -> Option<Self> {
        Self::ALL.get(v as usize).copied()
    }

    pub fn all() -> impl Iterator<Item = XAssetType> {
        Self::ALL.into_iter()
    }

    /// The original engine's lowercase asset-type name.
    pub fn name(self) -> &'static str {
        const NAMES: [&str; XAssetType::COUNT] = [
            "xmodelpieces",
            "physpreset",
            "xanimparts",
            "xmodel",
            "material",
            "techset",
            "image",
            "sound",
            "sndcurve",
            "loaded_sound",
            "col_map_sp",
            "col_map_mp",
            "com_map",
            "game_map_sp",
            "game_map_mp",
            "map_ents",
            "gfx_map",
            "lightdef",
            "ui_map",
            "font",
            "menufile",
            "menu",
            "localize",
            "weapon",
            "snddriverglobals",
            "fx",
            "impactfx",
            "aitype",
            "mptype",
            "character",
            "xmodelalias",
            "rawfile",
            "stringtable",
        ];
        NAMES[self as usize]
    }
}
