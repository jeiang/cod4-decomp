// SPDX-License-Identifier: GPL-3.0-or-later
//! Menu expression evaluator.
//!
//! The stock menus store each expression (`visible`, `text`, `material`, `rect x/y/w/h`, `forecolor a`)
//! as a flat token list in infix order. The original evaluates that stream with an operator stack and an
//! operand stack (`EvaluateExpression`); the shape of the operand stack depends only on the token order,
//! never on values, so [`Compiled::new`] replays the original's parser symbolically, once, and keeps the
//! resulting expression tree. Per-frame evaluation is then a plain tree walk that only allocates for
//! string results. Quirks of the original parser (right-leaning grouping of equal-precedence associative
//! operators, `a - b + c` evaluating as `a - (b + c)`, a function applying only at its closing `)`) are
//! reproduced deliberately because the shipped menus were authored against them.
//!
//! Anything the original reports as a structural error and then fails on (stray operands, missing
//! operands, wrong arity for single-argument functions, empty statement, over-deep nesting) is an
//! [`ExprError`] here; the convenience evaluators then yield [`Value::fallback`] (`""`, which reads as
//! `false`, `0`, `0.0` and an empty string at once), exactly what `IsExpressionTrue`,
//! `GetExpressionFloat` and `GetExpressionResultString` return for a failed statement.
//!
//! Everything an expression can ask of the world goes through [`ExprEnv`]; there are no globals.
//! Operations have no short-circuit: both sides of `&&`/`||` are always evaluated, as in the original.
//!
//! # Operator codes (`operationEnum`) and the [`ExprEnv`] method each consults
//!
//! | code | token | env method |
//! |---|---|---|
//! | 0x00 | NOOP | none; rejected by the compiler |
//! | 0x01 | `)` | none (closes a `(` or a function) |
//! | 0x02..=0x06 | `* / % + -` | none; `-` is unary when only one operand list is on the stack |
//! | 0x07 | `!` | none |
//! | 0x08..=0x0D | `< <= > >= == !=` | none; `==`/`!=` on two strings is case-insensitive |
//! | 0x0E, 0x0F | `&& \|\|` | none |
//! | 0x10, 0x11 | `(` `,` | none |
//! | 0x12..=0x16 | `& \| ~ << >>` | none |
//! | 0x17, 0x18 | sin, cos | none (radians) |
//! | 0x19, 0x1A | min, max | none (float result) |
//! | 0x1B | milliseconds | [`ExprEnv::milliseconds`] |
//! | 0x1C..=0x1F | dvarint, dvarbool, dvarfloat, dvarstring | [`ExprEnv::dvar_string`] |
//! | 0x20 | stat | [`ExprEnv::stat`] |
//! | 0x21 | ui_active | [`ExprEnv::ui_active`] |
//! | 0x22 | flashbanged | [`ExprEnv::flashbanged`] |
//! | 0x23 | scoped | [`ExprEnv::scoped`] |
//! | 0x24 | scoreboard_visible | [`ExprEnv::scoreboard_visible`] |
//! | 0x25 | inkillcam | [`ExprEnv::in_killcam`] |
//! | 0x26 | player | [`ExprEnv::player_field`] |
//! | 0x27 | selecting_location | [`ExprEnv::selecting_location`] |
//! | 0x28..=0x2B | team, otherteam, marinesfield, opforfield | [`ExprEnv::team_field`] |
//! | 0x2C | menuisopen | [`ExprEnv::menu_is_open`] |
//! | 0x2D | writingdata | [`ExprEnv::writing_data`] |
//! | 0x2E | inlobby | [`ExprEnv::in_lobby`] |
//! | 0x2F | inprivateparty | [`ExprEnv::in_private_party`] |
//! | 0x30 | privatepartyhost | [`ExprEnv::private_party_host`] |
//! | 0x31 | privatepartyhostinlobby | [`ExprEnv::private_party_host_in_lobby`] |
//! | 0x32 | aloneinparty | [`ExprEnv::alone_in_party`] |
//! | 0x33 | adsjavelin | [`ExprEnv::ads_javelin`] |
//! | 0x34 | weaplockblink | [`ExprEnv::weap_lock_blink`] |
//! | 0x35 | weapattacktop | [`ExprEnv::weap_attack_top`] |
//! | 0x36 | weapattackdirect | [`ExprEnv::weap_attack_direct`] |
//! | 0x37 | secondsastime | none (pure) |
//! | 0x38 | tablelookup | [`ExprEnv::table_lookup`] |
//! | 0x39 | locstring | [`ExprEnv::localize`] |
//! | 0x3A..=0x3D | localvarint, localvarbool, localvarfloat, localvarstring | [`ExprEnv::local_var`] |
//! | 0x3E | timeleft | [`ExprEnv::time_left`] |
//! | 0x3F | secondsascountdown | none (pure) |
//! | 0x40 | gamemsgwndactive | [`ExprEnv::game_message_window_active`] |
//! | 0x41..=0x43 | int, string, float | none |
//! | 0x44 | gametypename | [`ExprEnv::gametype_name`] |
//! | 0x45 | gametype | [`ExprEnv::gametype`] |
//! | 0x46 | gametypedescription | [`ExprEnv::gametype_description`] |
//! | 0x47 | scoreatrank | [`ExprEnv::score_at_rank`] |
//! | 0x48 | friendsonline | [`ExprEnv::friends_online`] |
//! | 0x49 | spectatingclient | [`ExprEnv::following`] |
//! | 0x4A | statrangeanybitsset | [`ExprEnv::stat`] (looped here) |
//! | 0x4B | keybinding | [`ExprEnv::key_binding`] |
//! | 0x4C | actionslotusable | [`ExprEnv::action_slot_usable`] |
//! | 0x4D | hudfade | [`ExprEnv::hud_fade`] |
//! | 0x4E | maxrecommendedplayers | [`ExprEnv::max_players`] |
//! | 0x4F | acceptinginvite | [`ExprEnv::accepting_invite`] |
//! | 0x50 | isintermission | [`ExprEnv::is_intermission`] |

use assets::zone::menu::{ExpressionEntry, Operand, Statement};
use std::borrow::Cow;
use std::fmt;
use std::rc::Rc;

/// The original's fixed capacity of both stacks; exceeding it fails the statement.
const STACK_LIMIT: usize = 60;
/// Longest comma-separated argument list; a longer one evaluates to `0`.
const MAX_ARGS: usize = 10;
/// Strings produced by the original live in 256-byte buffers.
const MAX_STRING: usize = 255;

macro_rules! ops {
    ($($name:ident = $code:literal,)*) => {
        /// The original's `operationEnum`, numbered identically.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u8)]
        pub enum Op { $($name = $code),* }
        const ALL_OPS: [Op; 81] = [$(Op::$name),*];
    };
}

ops! {
    Noop = 0, RightParen = 1, Multiply = 2, Divide = 3, Modulus = 4, Add = 5, Subtract = 6, Not = 7,
    LessThan = 8, LessThanEqualTo = 9, GreaterThan = 10, GreaterThanEqualTo = 11, Equals = 12,
    NotEqual = 13, And = 14, Or = 15, LeftParen = 16, Comma = 17, BitwiseAnd = 18, BitwiseOr = 19,
    BitwiseNot = 20, BitShiftLeft = 21, BitShiftRight = 22, Sin = 23, Cos = 24, Min = 25, Max = 26,
    Milliseconds = 27, DvarInt = 28, DvarBool = 29, DvarFloat = 30, DvarString = 31, Stat = 32,
    UiActive = 33, Flashbanged = 34, Scoped = 35, ScoreboardVisible = 36, InKillcam = 37,
    PlayerField = 38, SelectingLocation = 39, TeamField = 40, OtherTeamField = 41, MarinesField = 42,
    OpforField = 43, MenuIsOpen = 44, WritingData = 45, InLobby = 46, InPrivateParty = 47,
    PrivatePartyHost = 48, PrivatePartyHostInLobby = 49, AloneInParty = 50, AdsJavelin = 51,
    WeapLockBlink = 52, WeapAttackTop = 53, WeapAttackDirect = 54, SecondsAsTime = 55,
    TableLookup = 56, LocalizeString = 57, LocalVarInt = 58, LocalVarBool = 59, LocalVarFloat = 60,
    LocalVarString = 61, TimeLeft = 62, SecondsAsCountdown = 63, GameMsgWndActive = 64, ToInt = 65,
    ToString = 66, ToFloat = 67, GametypeName = 68, Gametype = 69, GametypeDescription = 70,
    Score = 71, FriendsOnline = 72, Following = 73, StatRangeBitsSet = 74, KeyBinding = 75,
    ActionSlotUsable = 76, HudFade = 77, MaxPlayers = 78, AcceptingInvite = 79, IsIntermission = 80,
}

/// `s_operatorPrecedence`: a *lower* number binds tighter. Functions are all 5.
const PRECEDENCE: [i32; 81] = {
    let mut p = [5; 81];
    p[Op::Noop as usize] = i32::MAX;
    p[Op::RightParen as usize] = 0;
    p[Op::Multiply as usize] = 11;
    p[Op::Divide as usize] = 11;
    p[Op::Modulus as usize] = 11;
    p[Op::Add as usize] = 13;
    p[Op::Subtract as usize] = 13;
    p[Op::Not as usize] = 9;
    p[Op::LessThan as usize] = 15;
    p[Op::LessThanEqualTo as usize] = 15;
    p[Op::GreaterThan as usize] = 15;
    p[Op::GreaterThanEqualTo as usize] = 15;
    p[Op::Equals as usize] = 16;
    p[Op::NotEqual as usize] = 16;
    p[Op::And as usize] = 25;
    p[Op::Or as usize] = 25;
    p[Op::LeftParen as usize] = 99;
    p[Op::Comma as usize] = 80;
    p[Op::BitwiseAnd as usize] = 17;
    p[Op::BitwiseOr as usize] = 18;
    p[Op::BitwiseNot as usize] = 9;
    p[Op::BitShiftLeft as usize] = 14;
    p[Op::BitShiftRight as usize] = 14;
    p
};

impl Op {
    pub fn from_code(code: i32) -> Option<Op> {
        usize::try_from(code)
            .ok()
            .and_then(|c| ALL_OPS.get(c))
            .copied()
    }

    fn precedence(self) -> i32 {
        PRECEDENCE[self as usize]
    }

    /// Functions (`sin` ... `isintermission`) double as their own opening parenthesis.
    fn is_function(self) -> bool {
        self as u8 >= Op::Sin as u8
    }

    /// `OpPairsWithRightParen`: what a `)` stops at.
    fn pairs_with_right_paren(self) -> bool {
        self.is_function() || self == Op::LeftParen
    }

    /// `IsOpAssociative`: false for `/`, `%`, `-`, which group left when they meet themselves.
    fn is_associative(self) -> bool {
        let c = self as u8;
        c < Op::Divide as u8 || (c > Op::Modulus as u8 && self != Op::Subtract)
    }

    fn arity(self) -> Arity {
        match self {
            Op::Min | Op::Max | Op::TableLookup | Op::LocalizeString | Op::StatRangeBitsSet => {
                Arity::List
            }
            Op::Sin
            | Op::Cos
            | Op::DvarInt
            | Op::DvarBool
            | Op::DvarFloat
            | Op::DvarString
            | Op::Stat
            | Op::PlayerField
            | Op::TeamField
            | Op::OtherTeamField
            | Op::MarinesField
            | Op::OpforField
            | Op::MenuIsOpen
            | Op::WeapLockBlink
            | Op::SecondsAsTime
            | Op::LocalVarInt
            | Op::LocalVarBool
            | Op::LocalVarFloat
            | Op::LocalVarString
            | Op::SecondsAsCountdown
            | Op::GameMsgWndActive
            | Op::ToInt
            | Op::ToString
            | Op::ToFloat
            | Op::Score
            | Op::KeyBinding
            | Op::ActionSlotUsable
            | Op::HudFade => Arity::One,
            _ => Arity::Zero,
        }
    }

    fn is_binary(self) -> bool {
        matches!(
            self,
            Op::Multiply
                | Op::Divide
                | Op::Modulus
                | Op::Add
                | Op::LessThan
                | Op::LessThanEqualTo
                | Op::GreaterThan
                | Op::GreaterThanEqualTo
                | Op::Equals
                | Op::NotEqual
                | Op::And
                | Op::Or
                | Op::BitwiseAnd
                | Op::BitwiseOr
                | Op::BitShiftLeft
                | Op::BitShiftRight
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arity {
    /// Takes nothing off the operand stack.
    Zero,
    /// `GetOperand`: exactly one operand.
    One,
    /// `GetOperandList`: whatever the commas gathered.
    List,
}

/// An expression value: the original's `Operand` (`VAL_INT` / `VAL_FLOAT` / `VAL_STRING`).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i32),
    Float(f32),
    Str(Rc<str>),
}

impl Value {
    fn str(s: &str) -> Value {
        Value::Str(truncate(s).into())
    }

    /// What a failed statement yields; reads as false / 0 / 0.0 / "".
    #[cfg(test)]
    pub fn fallback() -> Value {
        Value::Str("".into())
    }

    /// `GetSourceInt`: floats truncate, strings go through `atoi`.
    pub fn as_int(&self) -> i32 {
        match self {
            Value::Int(i) => *i,
            Value::Float(f) => *f as i32,
            Value::Str(s) => atoi(s),
        }
    }

    /// `GetSourceFloat`.
    pub fn as_float(&self) -> f32 {
        match self {
            Value::Int(i) => *i as f32,
            Value::Float(f) => *f,
            Value::Str(s) => atof(s) as f32,
        }
    }

    /// `IsExpressionTrue`'s test: the integer reading is non-zero (so `0.5` is false).
    pub fn as_bool(&self) -> bool {
        self.as_int() != 0
    }

    /// `GetSourceString`: `%i`, `%f` or the string itself.
    pub fn text(&self) -> Cow<'_, str> {
        match self {
            Value::Int(i) => Cow::Owned(i.to_string()),
            Value::Float(f) => Cow::Owned(format!("{f:.6}")),
            Value::Str(s) => Cow::Borrowed(s),
        }
    }

    /// The original's numeric reading of a string operand when no string overload exists: an int if
    /// `atoi` and `atof` agree, else a float.
    fn coerce_numeric(self) -> Value {
        match self {
            Value::Str(s) => {
                let (i, f) = (atoi(&s), atof(&s));
                if f64::from(i) == f {
                    Value::Int(i)
                } else {
                    Value::Float(f as f32)
                }
            }
            v => v,
        }
    }

    fn truth(&self) -> bool {
        match self {
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
        }
    }

    fn f64(&self) -> f64 {
        match self {
            Value::Int(i) => f64::from(*i),
            Value::Float(f) => f64::from(*f),
            Value::Str(s) => atof(s),
        }
    }
}

/// A local UI variable (`localvarint` etc.): a table separate from dvars, one typed slot per name.
#[derive(Clone, Debug, PartialEq)]
pub enum LocalVar {
    Int(i32),
    Float(f32),
    Str(String),
}

impl LocalVar {
    fn to_bool(&self) -> bool {
        match self {
            LocalVar::Int(i) => *i != 0,
            LocalVar::Float(f) => *f != 0.0,
            LocalVar::Str(s) => atoi(s) != 0,
        }
    }

    fn to_int(&self) -> i32 {
        match self {
            LocalVar::Int(i) => *i,
            LocalVar::Float(f) => *f as i32,
            LocalVar::Str(s) => atoi(s),
        }
    }

    fn to_float(&self) -> f32 {
        match self {
            LocalVar::Int(i) => *i as f32,
            LocalVar::Float(f) => *f,
            LocalVar::Str(s) => atof(s) as f32,
        }
    }

    /// Floats print with `%g`.
    fn to_text(&self) -> String {
        match self {
            LocalVar::Int(i) => i.to_string(),
            LocalVar::Float(f) => format_g(f64::from(*f)),
            LocalVar::Str(s) => s.clone(),
        }
    }
}

/// Field of `player("...")`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerField {
    /// `"teamname"`: string, the local player's team name.
    TeamName,
    /// `"otherteamname"`: string, the opposing team's name.
    OtherTeamName,
    /// `"dead"`: int 0/1.
    Dead,
    /// `"clipAmmo"`: int, rounds in the current clip.
    ClipAmmo,
    /// `"nightvision"`: int 0/1, looking through night vision.
    NightVision,
    /// `"score"`: int, the local player's scoreboard row (0 when there is none).
    Score,
    /// `"deaths"`: int.
    Deaths,
    /// `"kills"`: int.
    Kills,
    /// `"ping"`: int.
    Ping,
}

/// Which team `team(..)`-family operators address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeamSel {
    /// `team`: the local player's team (free when its client info is not valid yet).
    Own,
    /// `otherteam`: allies for axis, axis for allies, spectator and free map to themselves.
    Other,
    /// `marinesfield`: always allies.
    Marines,
    /// `opforfield`: always axis.
    Opfor,
}

/// Field of `team("...")`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeamField {
    /// `"score"`: int, the team's score.
    Score,
    /// `"name"`: string, the team's display name.
    Name,
}

/// Argument of `hudfade("...")`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HudFade {
    Dpad,
    Weapon,
    Compass,
    /// The original answers a constant 1.0 once cgame is initialised.
    Scoreboard,
}

/// One argument of `locstring(...)`, as the original classifies them: a string argument is a localized
/// string reference (its leading `@` is dropped; arguments of length 0 or 1 are skipped), any other
/// argument is a literal number rendered as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocPart {
    pub is_ref: bool,
    pub text: String,
}

/// Everything an expression can ask of the world. The client implements it over its live state; methods
/// without a default are needed by the MP menus, defaulted ones are lobby/party/online-friends features
/// that are constant in the PC build (and SP-only weapon-lock state).
pub trait ExprEnv {
    /// `Dvar_GetVariantString`: the dvar's value rendered as text, `""` if it does not exist. Backs
    /// `dvarint`/`dvarbool`/`dvarfloat` (the evaluator applies `atoi`/`atof`) and `dvarstring`.
    fn dvar_string(&self, name: &str) -> String;
    /// Local UI variable table lookup (`localvar*`), separate from dvars; `None` if undefined.
    fn local_var(&self, name: &str) -> Option<LocalVar>;
    /// `milliseconds()`: `Sys_Milliseconds`.
    fn milliseconds(&self) -> i32;
    /// `ui_active()`: a menu is capturing input.
    fn ui_active(&self) -> bool;
    /// `flashbanged()`.
    fn flashbanged(&self) -> bool;
    /// `scoped()`: a scope overlay is displayed.
    fn scoped(&self) -> bool;
    /// `scoreboard_visible()`.
    fn scoreboard_visible(&self) -> bool;
    /// `inkillcam()`.
    fn in_killcam(&self) -> bool;
    /// `player("...")`; see [`PlayerField`] for the type each field must return.
    fn player_field(&self, field: PlayerField) -> Value;
    /// `selecting_location()`: airstrike-style map targeting.
    fn selecting_location(&self) -> bool;
    /// `team(..)`, `otherteam(..)`, `marinesfield(..)`, `opforfield(..)`; see [`TeamField`].
    fn team_field(&self, team: TeamSel, field: TeamField) -> Value;
    /// `menuisopen("name")`: the menu is open and visible.
    fn menu_is_open(&self, name: &str) -> bool;
    /// `stat(n)`: persistent player stat.
    fn stat(&self, index: i32) -> i32;
    /// `locstring(...)`: assemble the localized string (`SEH_LocalizeTextMessage`): the first part is the
    /// base reference and later parts substitute its `&1`.. parameters.
    fn localize(&self, parts: &[LocPart]) -> String;
    /// `tablelookup(file, searchColumn, searchValue, returnColumn)`: string-table lookup, `""` if absent.
    fn table_lookup(
        &self,
        file: &str,
        search_column: i32,
        search_value: &str,
        return_column: i32,
    ) -> String;
    /// `timeleft()`: whole seconds until the game ends (0 without a snapshot).
    fn time_left(&self) -> i32;
    /// `gamemsgwndactive(n)`: game message window `n` currently shows text (0 if `n` is invalid).
    fn game_message_window_active(&self, window: i32) -> bool;
    /// `gametypename()`: the localized display name.
    fn gametype_name(&self) -> String;
    /// `gametype()`: the internal gametype id such as `"war"`.
    fn gametype(&self) -> String;
    /// `gametypedescription()`.
    fn gametype_description(&self) -> String;
    /// `scoreatrank(rank)`: score of the player at 1-based `rank`, 0 if none.
    fn score_at_rank(&self, rank: i32) -> i32;
    /// `spectatingclient()`: the local player is following someone.
    fn following(&self) -> bool;
    /// `keybinding("command")`: localized key name(s), single binding form; `KEY_UNBOUND` text if none.
    fn key_binding(&self, command: &str) -> String;
    /// `actionslotusable(slot)`, `slot` already validated to `1..=4`.
    fn action_slot_usable(&self, slot: i32) -> bool;
    /// `hudfade("...")`: alpha 0..=1 (0 before cgame is initialised).
    fn hud_fade(&self, element: HudFade) -> f32;
    /// `isintermission()`.
    fn is_intermission(&self) -> bool;
    /// `adsjavelin()`.
    fn ads_javelin(&self) -> bool;

    /// `writingdata()`: constant 0 in the PC build.
    fn writing_data(&self) -> bool {
        false
    }
    /// `inlobby()`: constant 0 in the PC build.
    fn in_lobby(&self) -> bool {
        false
    }
    /// `inprivateparty()`: constant 0 in the PC build.
    fn in_private_party(&self) -> bool {
        false
    }
    /// `privatepartyhost()`: constant 0 in the PC build.
    fn private_party_host(&self) -> bool {
        false
    }
    /// `privatepartyhostinlobby()`: constant 0 in the PC build.
    fn private_party_host_in_lobby(&self) -> bool {
        false
    }
    /// `aloneinparty()`: constant 0 in the PC build.
    fn alone_in_party(&self) -> bool {
        false
    }
    /// `friendsonline()`: constant 0 in the PC build.
    fn friends_online(&self) -> i32 {
        0
    }
    /// `maxrecommendedplayers()`: constant 0 in the PC build (the original routes it to the friends stub).
    fn max_players(&self) -> i32 {
        0
    }
    /// `acceptinginvite()`: constant 0 in the PC build (same stub).
    fn accepting_invite(&self) -> i32 {
        0
    }
    /// `weaplockblink(blinksPerSecond)`. No stock MP menu uses it; the reconstruction ties it to a
    /// non-PC feature, so it is false by default.
    fn weap_lock_blink(&self, _blinks_per_second: f32) -> bool {
        false
    }
    /// `weapattacktop()`: see [`ExprEnv::weap_lock_blink`].
    fn weap_attack_top(&self) -> bool {
        false
    }
    /// `weapattackdirect()`: see [`ExprEnv::weap_lock_blink`].
    fn weap_attack_direct(&self) -> bool {
        false
    }
}

/// Why a statement cannot be evaluated (the original prints the matching error and fails it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprError {
    /// No tokens (nothing to evaluate).
    Empty,
    /// Operator code outside `1..=0x50` (or the invalid `NOOP`).
    BadOperator(i32),
    /// More than 60 operand lists pending.
    TooManyOperands,
    /// More than 60 operators pending.
    TooDeeplyNested,
    /// An operator found no operand where it needs one.
    MissingOperand,
    /// An operator needing one operand found a comma list of this many instead.
    BadOperandCount(usize),
    /// Operands remain that no operator consumed, or the result is a comma list.
    StrayOperands,
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExprError::Empty => write!(f, "empty expression"),
            ExprError::BadOperator(c) => write!(f, "invalid operator code {c}"),
            ExprError::TooManyOperands => write!(f, "too many operands"),
            ExprError::TooDeeplyNested => write!(f, "operators nested too deeply"),
            ExprError::MissingOperand => write!(f, "missing operand"),
            ExprError::BadOperandCount(n) => write!(f, "expected one operand, found {n}"),
            ExprError::StrayOperands => write!(f, "stray operands"),
        }
    }
}

impl std::error::Error for ExprError {}

#[derive(Debug)]
enum Node {
    Const(Value),
    /// `!`, `~`, unary `-`.
    Unary(Op, Box<Node>),
    Binary(Op, Box<Node>, Box<Node>),
    Call(Op, Box<[Node]>),
}

/// A statement parsed once into an expression tree.
#[derive(Debug)]
pub struct Compiled {
    root: Node,
}

/// Compile-time replay of the original's operator/operand stacks.
struct Parser {
    ops: Vec<Op>,
    /// Each entry is a comma list; a plain operand is a list of one.
    data: Vec<Vec<Node>>,
}

impl Parser {
    fn pop_one(&mut self) -> Result<Node, ExprError> {
        match self.data.last() {
            None => Err(ExprError::MissingOperand),
            Some(l) if l.len() != 1 => Err(ExprError::BadOperandCount(l.len())),
            Some(_) => Ok(self.data.pop().and_then(|mut l| l.pop()).expect("checked")),
        }
    }

    fn pop_list(&mut self) -> Result<Vec<Node>, ExprError> {
        self.data.pop().ok_or(ExprError::MissingOperand)
    }

    fn push(&mut self, n: Node) {
        self.data.push(vec![n]);
    }

    /// `RunHigherPriorityOperators`: reduce while the stack top binds tighter than `op`.
    fn run_higher(&mut self, op: Op) -> Result<(), ExprError> {
        while let Some(&top) = self.ops.last() {
            let blocks = top.precedence() >= op.precedence()
                || (top.precedence() == 5 && op != Op::RightParen);
            if blocks && (op.is_associative() || top != op) {
                break;
            }
            self.run_op()?;
        }
        Ok(())
    }

    /// `RunOp`: pop one operator and apply it to the operand stack.
    fn run_op(&mut self) -> Result<(), ExprError> {
        let Some(op) = self.ops.pop() else {
            return Ok(());
        };
        match op {
            Op::Noop => Err(ExprError::BadOperator(0)),
            Op::LeftParen => Ok(()),
            Op::RightParen => {
                while let Some(&top) = self.ops.last() {
                    self.run_op()?;
                    if top.pairs_with_right_paren() {
                        break;
                    }
                }
                Ok(())
            }
            Op::Subtract => {
                let rhs = self.pop_one()?;
                if self.data.is_empty() {
                    self.push(Node::Unary(Op::Subtract, Box::new(rhs)));
                } else {
                    let lhs = self.pop_one()?;
                    self.push(Node::Binary(op, Box::new(lhs), Box::new(rhs)));
                }
                Ok(())
            }
            Op::Not | Op::BitwiseNot => {
                let x = self.pop_one()?;
                self.push(Node::Unary(op, Box::new(x)));
                Ok(())
            }
            Op::Comma => {
                let mut right = self.pop_list()?;
                let mut list = self.pop_list()?;
                if list.len() + right.len() <= MAX_ARGS {
                    list.append(&mut right);
                    self.data.push(list);
                } else {
                    self.push(Node::Const(Value::Int(0)));
                }
                Ok(())
            }
            _ if op.is_binary() => {
                let rhs = self.pop_one()?;
                let lhs = self.pop_one()?;
                self.push(Node::Binary(op, Box::new(lhs), Box::new(rhs)));
                Ok(())
            }
            _ => {
                let args: Vec<Node> = match op.arity() {
                    Arity::Zero => Vec::new(),
                    Arity::One => vec![self.pop_one()?],
                    Arity::List => self.pop_list()?,
                };
                self.push(Node::Call(op, args.into()));
                Ok(())
            }
        }
    }
}

impl Compiled {
    pub fn new(stmt: &Statement) -> Result<Compiled, ExprError> {
        let mut p = Parser {
            ops: Vec::new(),
            data: Vec::new(),
        };
        for entry in &stmt.entries {
            match &**entry {
                ExpressionEntry::Operand(o) => {
                    if p.data.len() == STACK_LIMIT {
                        return Err(ExprError::TooManyOperands);
                    }
                    p.push(Node::Const(match o {
                        Operand::Int(i) => Value::Int(*i),
                        Operand::Float(f) => Value::Float(*f),
                        Operand::String(s) => Value::str(s.as_deref().unwrap_or("")),
                    }));
                }
                ExpressionEntry::Operator(code) => {
                    let op = Op::from_code(*code)
                        .filter(|&o| o != Op::Noop)
                        .ok_or(ExprError::BadOperator(*code))?;
                    if op != Op::LeftParen {
                        p.run_higher(op)?;
                    }
                    if p.ops.len() == STACK_LIMIT {
                        return Err(ExprError::TooDeeplyNested);
                    }
                    p.ops.push(op);
                }
            }
        }
        while !p.ops.is_empty() {
            p.run_op()?;
        }
        match p.data.len() {
            0 => Err(ExprError::Empty),
            1 => {
                let mut list = p.data.pop().expect("one list");
                if list.len() == 1 {
                    Ok(Compiled {
                        root: list.pop().expect("one operand"),
                    })
                } else {
                    Err(ExprError::StrayOperands)
                }
            }
            _ => Err(ExprError::StrayOperands),
        }
    }

    pub fn eval(&self, env: &dyn ExprEnv) -> Value {
        eval_node(&self.root, env)
    }

    /// `IsExpressionTrue`.
    pub fn eval_bool(&self, env: &dyn ExprEnv) -> bool {
        self.eval(env).as_bool()
    }

    /// `GetExpressionFloat`.
    pub fn eval_float(&self, env: &dyn ExprEnv) -> f32 {
        self.eval(env).as_float()
    }

    /// `GetExpressionResultString`.
    pub fn eval_string(&self, env: &dyn ExprEnv) -> String {
        truncate(&self.eval(env).text()).to_owned()
    }
}

/// Compile and evaluate in one go (tests); a statement that fails to compile yields [`Value::fallback`].
#[cfg(test)]
pub fn eval(stmt: &Statement, env: &dyn ExprEnv) -> Value {
    Compiled::new(stmt).map_or_else(|_| Value::fallback(), |c| c.eval(env))
}

#[cfg(test)]
pub fn eval_bool(stmt: &Statement, env: &dyn ExprEnv) -> bool {
    eval(stmt, env).as_bool()
}

#[cfg(test)]
pub fn eval_float(stmt: &Statement, env: &dyn ExprEnv) -> f32 {
    eval(stmt, env).as_float()
}

#[cfg(test)]
pub fn eval_string(stmt: &Statement, env: &dyn ExprEnv) -> String {
    truncate(&eval(stmt, env).text()).to_owned()
}

fn eval_node(n: &Node, env: &dyn ExprEnv) -> Value {
    match n {
        Node::Const(v) => v.clone(),
        Node::Unary(op, x) => unary(*op, eval_node(x, env)),
        Node::Binary(op, l, r) => binary(*op, eval_node(l, env), eval_node(r, env)),
        Node::Call(op, args) => call(*op, args, env),
    }
}

fn bool_value(b: bool) -> Value {
    Value::Int(i32::from(b))
}

fn unary(op: Op, v: Value) -> Value {
    match (op, v) {
        // The original errors on strings (`!`, `~`) and yields 0.
        (Op::Not | Op::BitwiseNot, Value::Str(_)) => Value::Int(0),
        (Op::Not, Value::Int(i)) => bool_value(i == 0),
        (Op::Not, Value::Float(f)) => bool_value(f == 0.0),
        (Op::BitwiseNot, v) => Value::Int(!v.as_int()),
        // Negating a string fails the whole statement in the original; read it as 0 here.
        (Op::Subtract, Value::Int(i)) => Value::Int(i.wrapping_neg()),
        (Op::Subtract, Value::Float(f)) => Value::Float(-f),
        (Op::Subtract, Value::Str(_)) => Value::Int(0),
        _ => unreachable!("compiler only builds ! ~ - unary nodes"),
    }
}

fn snap(f: f32) -> i32 {
    f.round_ties_even() as i32
}

fn int_rem(l: i32, r: i32) -> i32 {
    if r == 0 { l } else { l.wrapping_rem(r) }
}

/// The original's `validOperations` dispatch: exact string overloads first, then string operands are
/// read as numbers and the numeric overload runs.
fn binary(op: Op, l: Value, r: Value) -> Value {
    use Value::{Float, Int, Str};
    let (l_str, r_str) = (matches!(l, Str(_)), matches!(r, Str(_)));
    match op {
        Op::BitShiftLeft => {
            return Int(l.as_int().wrapping_shl(r.as_int() as u32));
        }
        Op::BitShiftRight => {
            return Int(l.as_int().wrapping_shr(r.as_int() as u32));
        }
        Op::Equals | Op::NotEqual if l_str && r_str => {
            let (Str(a), Str(b)) = (&l, &r) else {
                unreachable!()
            };
            return bool_value(a.eq_ignore_ascii_case(b) == (op == Op::Equals));
        }
        Op::Add if l_str || r_str => {
            let joined = format!("{}{}", truncate(&l.text()), truncate(&r.text()));
            return Value::str(&joined);
        }
        // A string beside a number: the string counts as true when non-empty.
        Op::And if l_str != r_str => return bool_value(l.truth() && r.truth()),
        Op::Or if l_str != r_str => return bool_value(l.truth() || r.truth()),
        Op::BitwiseAnd | Op::BitwiseOr if l_str != r_str => {
            let (a, b) = (l.as_int(), r.as_int());
            return Int(if op == Op::BitwiseAnd { a & b } else { a | b });
        }
        _ => {}
    }
    let (l, r) = (l.coerce_numeric(), r.coerce_numeric());
    let both_int = matches!((&l, &r), (Int(_), Int(_)));
    let (a, b) = (l.f64(), r.f64());
    match op {
        Op::Add | Op::Subtract | Op::Multiply if both_int => {
            let (Int(x), Int(y)) = (&l, &r) else {
                unreachable!()
            };
            Int(match op {
                Op::Add => x.wrapping_add(*y),
                Op::Subtract => x.wrapping_sub(*y),
                _ => x.wrapping_mul(*y),
            })
        }
        Op::Add => Float((a + b) as f32),
        Op::Subtract => Float((a - b) as f32),
        Op::Multiply => Float((a * b) as f32),
        Op::Divide => Float(if b == 0.0 { 0.0 } else { (a / b) as f32 }),
        Op::Modulus => Int(match (&l, &r) {
            (Int(x), Int(y)) => int_rem(*x, *y),
            (Int(x), Float(y)) => int_rem(*x, snap(*y)),
            (Float(x), Int(y)) => int_rem(snap(*x), *y),
            (Float(x), Float(y)) => int_rem(snap(*x), snap(*y)),
            _ => unreachable!("strings were coerced"),
        }),
        Op::LessThan => bool_value(a < b),
        Op::LessThanEqualTo => bool_value(a <= b),
        Op::GreaterThan => bool_value(a > b),
        Op::GreaterThanEqualTo => bool_value(a >= b),
        Op::Equals => bool_value(a == b),
        Op::NotEqual => bool_value(a != b),
        Op::And => bool_value(l.truth() && r.truth()),
        Op::Or => bool_value(l.truth() || r.truth()),
        Op::BitwiseAnd => Int(l.as_int() & r.as_int()),
        Op::BitwiseOr => Int(l.as_int() | r.as_int()),
        _ => unreachable!("compiler only builds binary operator nodes"),
    }
}

fn name_of(v: &Value) -> Option<&str> {
    match v {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

/// Original-style accessor: the argument must be a string, else the operator reports an error.
fn with_name(v: &Value, f: impl FnOnce(&str) -> Value, on_error: Value) -> Value {
    name_of(v).map_or(on_error, f)
}

fn call(op: Op, args: &[Node], env: &dyn ExprEnv) -> Value {
    use Value::{Float, Int};
    // Every single-argument operator has exactly one argument (checked when compiling); nullary ones none.
    let a = || eval_node(&args[0], env);
    match op {
        Op::Sin => Float(f64::from(a().as_float()).sin() as f32),
        Op::Cos => Float(f64::from(a().as_float()).cos() as f32),
        Op::Min | Op::Max => {
            let mut vals = args.iter().map(|n| eval_node(n, env).as_float());
            let Some(first) = vals.next() else {
                return Float(0.0);
            };
            Float(vals.fold(first, |acc, v| match op {
                Op::Min if acc > v => v,
                Op::Max if acc < v => v,
                _ => acc,
            }))
        }
        Op::Milliseconds => Int(env.milliseconds()),
        Op::DvarInt | Op::DvarBool => with_name(&a(), |n| Int(atoi(&env.dvar_string(n))), Int(0)),
        Op::DvarFloat => with_name(
            &a(),
            |n| Float(atof(&env.dvar_string(n)) as f32),
            Float(0.0),
        ),
        Op::DvarString => with_name(&a(), |n| Value::str(&env.dvar_string(n)), Value::str("")),
        Op::Stat => Int(env.stat(a().as_int())),
        Op::UiActive => bool_value(env.ui_active()),
        Op::Flashbanged => bool_value(env.flashbanged()),
        Op::Scoped => bool_value(env.scoped()),
        Op::ScoreboardVisible => bool_value(env.scoreboard_visible()),
        Op::InKillcam => bool_value(env.in_killcam()),
        Op::PlayerField => with_name(
            &a(),
            |n| match PLAYER_FIELDS
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(n))
            {
                Some((_, f)) => env.player_field(*f),
                None => Int(0),
            },
            Value::str(""),
        ),
        Op::SelectingLocation => bool_value(env.selecting_location()),
        Op::TeamField | Op::OtherTeamField | Op::MarinesField | Op::OpforField => {
            let sel = match op {
                Op::TeamField => TeamSel::Own,
                Op::OtherTeamField => TeamSel::Other,
                Op::MarinesField => TeamSel::Marines,
                _ => TeamSel::Opfor,
            };
            with_name(
                &a(),
                |n| {
                    if n.eq_ignore_ascii_case("score") {
                        env.team_field(sel, TeamField::Score)
                    } else if n.eq_ignore_ascii_case("name") {
                        env.team_field(sel, TeamField::Name)
                    } else {
                        Int(0)
                    }
                },
                Value::str(""),
            )
        }
        Op::MenuIsOpen => with_name(&a(), |n| bool_value(env.menu_is_open(n)), Int(0)),
        Op::WritingData => bool_value(env.writing_data()),
        Op::InLobby => bool_value(env.in_lobby()),
        Op::InPrivateParty => bool_value(env.in_private_party()),
        Op::PrivatePartyHost => bool_value(env.private_party_host()),
        Op::PrivatePartyHostInLobby => bool_value(env.private_party_host_in_lobby()),
        Op::AloneInParty => bool_value(env.alone_in_party()),
        Op::AdsJavelin => bool_value(env.ads_javelin()),
        Op::WeapLockBlink => bool_value(env.weap_lock_blink(a().as_float())),
        Op::WeapAttackTop => bool_value(env.weap_attack_top()),
        Op::WeapAttackDirect => bool_value(env.weap_attack_direct()),
        Op::SecondsAsTime => {
            let minutes = snap(a().as_int() as f32 / 60.0);
            Value::str(&format!(
                "{}d {}h {}m",
                minutes / 1440,
                minutes % 1440 / 60,
                minutes % 60
            ))
        }
        Op::TableLookup => {
            if args.len() != 4 {
                return Value::str("");
            }
            let v: Vec<Value> = args.iter().map(|n| eval_node(n, env)).collect();
            Value::str(&env.table_lookup(&v[0].text(), v[1].as_int(), &v[2].text(), v[3].as_int()))
        }
        Op::LocalizeString => localize(args, env),
        Op::LocalVarInt | Op::LocalVarBool | Op::LocalVarFloat | Op::LocalVarString => {
            let var = name_of(&a()).and_then(|n| env.local_var(n));
            match (op, var) {
                (Op::LocalVarInt, v) => Int(v.map_or(0, |v| v.to_int())),
                (Op::LocalVarBool, v) => bool_value(v.is_some_and(|v| v.to_bool())),
                (Op::LocalVarFloat, v) => Float(v.map_or(0.0, |v| v.to_float())),
                (_, v) => Value::str(&v.map_or_else(String::new, |v| v.to_text())),
            }
        }
        Op::TimeLeft => Int(env.time_left()),
        Op::SecondsAsCountdown => {
            let s = a().as_int();
            if s >= 0 {
                Value::str(&format!("{:>2}:{:02}", s / 60, s % 60))
            } else {
                Value::str("")
            }
        }
        Op::GameMsgWndActive => bool_value(env.game_message_window_active(a().as_int())),
        Op::ToInt => Int(a().as_int()),
        Op::ToString => Value::str(&a().text()),
        Op::ToFloat => Float(a().as_float()),
        Op::GametypeName => Value::str(&env.gametype_name()),
        Op::Gametype => Value::str(&env.gametype()),
        Op::GametypeDescription => Value::str(&env.gametype_description()),
        Op::Score => match a() {
            Int(rank) if rank > 0 => Int(env.score_at_rank(rank)),
            _ => Int(0),
        },
        Op::FriendsOnline => Int(env.friends_online()),
        Op::Following => bool_value(env.following()),
        Op::StatRangeBitsSet => {
            let v: Vec<i32> = args.iter().map(|n| eval_node(n, env).as_int()).collect();
            let [min, max, mask] = v[..] else {
                return Int(0);
            };
            bool_value((min..=max).any(|i| env.stat(i) & mask != 0))
        }
        Op::KeyBinding => with_name(&a(), |n| Value::str(&env.key_binding(n)), Value::str("")),
        Op::ActionSlotUsable => {
            let slot = a().as_int();
            bool_value((1..=4).contains(&slot) && env.action_slot_usable(slot))
        }
        Op::HudFade => {
            let element = name_of(&a()).and_then(|n| {
                [
                    ("dpad", HudFade::Dpad),
                    ("weapon", HudFade::Weapon),
                    ("compass", HudFade::Compass),
                    ("scoreboard", HudFade::Scoreboard),
                ]
                .into_iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(n))
                .map(|(_, e)| e)
            });
            Float(element.map_or(0.0, |e| env.hud_fade(e)))
        }
        Op::MaxPlayers => Int(env.max_players()),
        Op::AcceptingInvite => Int(env.accepting_invite()),
        Op::IsIntermission => bool_value(env.is_intermission()),
        _ => unreachable!("compiler only builds function call nodes for functions"),
    }
}

const PLAYER_FIELDS: [(&str, PlayerField); 9] = [
    ("teamname", PlayerField::TeamName),
    ("otherteamname", PlayerField::OtherTeamName),
    ("dead", PlayerField::Dead),
    ("clipAmmo", PlayerField::ClipAmmo),
    ("nightvision", PlayerField::NightVision),
    ("score", PlayerField::Score),
    ("deaths", PlayerField::Deaths),
    ("kills", PlayerField::Kills),
    ("ping", PlayerField::Ping),
];

/// `LocalizeString`: classify and collect the arguments for [`ExprEnv::localize`].
fn localize(args: &[Node], env: &dyn ExprEnv) -> Value {
    let mut parts = Vec::with_capacity(args.len());
    for n in args {
        match eval_node(n, env) {
            Value::Str(s) => {
                // Skips empty and one-character strings; drops the leading character (the `@`).
                if let Some((_, rest)) = s.char_indices().nth(1).map(|(i, _)| s.split_at(i)) {
                    parts.push(LocPart {
                        is_ref: true,
                        text: rest.to_owned(),
                    });
                }
            }
            v => {
                let text = v.text().into_owned();
                if !text.is_empty() {
                    parts.push(LocPart {
                        is_ref: false,
                        text,
                    });
                }
            }
        }
    }
    Value::str(&env.localize(&parts))
}

/// `Com_sprintf` into a 256-byte buffer.
fn truncate(s: &str) -> &str {
    if s.len() <= MAX_STRING {
        return s;
    }
    let mut n = MAX_STRING;
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s[..n]
}

/// C `atoi`: leading whitespace, sign, digits; saturating.
fn atoi(s: &str) -> i32 {
    let s = s.trim_start();
    let (neg, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut n: i64 = 0;
    for b in digits.bytes().take_while(u8::is_ascii_digit) {
        n = (n * 10 + i64::from(b - b'0')).min(i64::from(i32::MAX) + 1);
    }
    let n = if neg { -n } else { n };
    n.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// C `atof` for decimal input: the longest numeric prefix, else 0.
fn atof(s: &str) -> f64 {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut i = usize::from(matches!(b.first(), Some(b'-' | b'+')));
    let digits = |mut j: usize| {
        while b.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        j
    };
    let int_end = digits(i);
    let mut end = int_end;
    let mut any = int_end > i;
    if b.get(end) == Some(&b'.') {
        let frac_end = digits(end + 1);
        any |= frac_end > end + 1;
        end = frac_end;
    }
    if !any {
        return 0.0;
    }
    if matches!(b.get(end), Some(b'e' | b'E')) {
        i = end + 1;
        if matches!(b.get(i), Some(b'-' | b'+')) {
            i += 1;
        }
        let exp_end = digits(i);
        if exp_end > i {
            end = exp_end;
        }
    }
    s[..end].parse().unwrap_or(0.0)
}

/// C `%g` (6 significant digits).
fn format_g(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return if v.is_nan() {
            "nan".into()
        } else if v.is_infinite() {
            if v < 0.0 { "-inf" } else { "inf" }.into()
        } else if v.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let sci = format!("{v:.5e}");
    let (mantissa, exp) = sci.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("exponent");
    let trim = |s: &str| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s.to_owned()
        }
    };
    if (-4..6).contains(&exp) {
        trim(&format!("{v:.*}", (5 - exp) as usize))
    } else {
        format!(
            "{}e{}{:02}",
            trim(mantissa),
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assets::zone::menu::{ExpressionEntry as E, Operand as O};
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    #[derive(Default)]
    struct Mock {
        dvars: HashMap<String, String>,
        locals: HashMap<String, LocalVar>,
        stats: HashMap<i32, i32>,
        tables: HashMap<(String, i32, String, i32), String>,
        open_menus: Vec<String>,
    }

    impl ExprEnv for Mock {
        fn dvar_string(&self, name: &str) -> String {
            self.dvars.get(name).cloned().unwrap_or_default()
        }
        fn local_var(&self, name: &str) -> Option<LocalVar> {
            self.locals.get(name).cloned()
        }
        fn milliseconds(&self) -> i32 {
            1234
        }
        fn ui_active(&self) -> bool {
            true
        }
        fn flashbanged(&self) -> bool {
            false
        }
        fn scoped(&self) -> bool {
            true
        }
        fn scoreboard_visible(&self) -> bool {
            false
        }
        fn in_killcam(&self) -> bool {
            true
        }
        fn player_field(&self, f: PlayerField) -> Value {
            match f {
                PlayerField::TeamName => Value::Str("Marines".into()),
                PlayerField::OtherTeamName => Value::Str("OpFor".into()),
                PlayerField::Dead => Value::Int(0),
                PlayerField::ClipAmmo => Value::Int(7),
                PlayerField::NightVision => Value::Int(1),
                PlayerField::Score => Value::Int(100),
                PlayerField::Deaths => Value::Int(2),
                PlayerField::Kills => Value::Int(9),
                PlayerField::Ping => Value::Int(33),
            }
        }
        fn selecting_location(&self) -> bool {
            false
        }
        fn team_field(&self, team: TeamSel, f: TeamField) -> Value {
            match (team, f) {
                (TeamSel::Marines, TeamField::Score) => Value::Int(5),
                (TeamSel::Opfor, TeamField::Score) => Value::Int(3),
                (TeamSel::Own, TeamField::Name) => Value::Str("MARINES".into()),
                _ => Value::Int(-1),
            }
        }
        fn menu_is_open(&self, name: &str) -> bool {
            self.open_menus.iter().any(|m| m == name)
        }
        fn stat(&self, i: i32) -> i32 {
            self.stats.get(&i).copied().unwrap_or(0)
        }
        fn localize(&self, parts: &[LocPart]) -> String {
            parts
                .iter()
                .map(|p| format!("{}{}", if p.is_ref { "R:" } else { "L:" }, p.text))
                .collect::<Vec<_>>()
                .join("|")
        }
        fn table_lookup(&self, f: &str, sc: i32, sv: &str, rc: i32) -> String {
            self.tables
                .get(&(f.into(), sc, sv.into(), rc))
                .cloned()
                .unwrap_or_default()
        }
        fn time_left(&self) -> i32 {
            125
        }
        fn game_message_window_active(&self, w: i32) -> bool {
            w == 1
        }
        fn gametype_name(&self) -> String {
            "Team Deathmatch".into()
        }
        fn gametype(&self) -> String {
            "war".into()
        }
        fn gametype_description(&self) -> String {
            "Kill".into()
        }
        fn score_at_rank(&self, rank: i32) -> i32 {
            rank * 10
        }
        fn following(&self) -> bool {
            false
        }
        fn key_binding(&self, c: &str) -> String {
            format!("key:{c}")
        }
        fn action_slot_usable(&self, s: i32) -> bool {
            s == 2
        }
        fn hud_fade(&self, e: HudFade) -> f32 {
            match e {
                HudFade::Dpad => 0.25,
                HudFade::Weapon => 0.5,
                HudFade::Compass => 0.75,
                HudFade::Scoreboard => 1.0,
            }
        }
        fn is_intermission(&self) -> bool {
            true
        }
        fn ads_javelin(&self) -> bool {
            true
        }
    }

    /// Tokenizer for tests: whitespace separated, `"..."` strings, numbers, operator symbols or names.
    fn stmt(src: &str) -> Statement {
        let mut entries = Vec::new();
        let mut chars = src.chars().peekable();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
                continue;
            }
            let mut tok = String::new();
            // Functions are their own opening parenthesis in the stored stream; keep test sources readable.
            let after_function = matches!(
                entries.last().map(|e: &E| e),
                Some(E::Operator(c)) if Op::from_code(*c).is_some_and(Op::is_function)
            );
            if c == '(' && after_function {
                chars.next();
                continue;
            }
            if c == '"' {
                chars.next();
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                    tok.push(c);
                }
                entries.push(E::Operand(O::String(Some(tok.into()))));
                continue;
            }
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                tok.push(c);
                chars.next();
            }
            let entry = if let Some(op) = op_by_name(&tok) {
                E::Operator(op as i32)
            } else if let Ok(i) = tok.parse::<i32>() {
                E::Operand(O::Int(i))
            } else {
                E::Operand(O::Float(
                    tok.parse().unwrap_or_else(|_| panic!("token {tok}")),
                ))
            };
            entries.push(entry);
        }
        Statement {
            entries: entries.into_iter().map(Arc::new).collect(),
        }
    }

    fn op_by_name(name: &str) -> Option<Op> {
        Some(match name {
            ")" => Op::RightParen,
            "*" => Op::Multiply,
            "/" => Op::Divide,
            "%" => Op::Modulus,
            "+" => Op::Add,
            "-" => Op::Subtract,
            "!" => Op::Not,
            "<" => Op::LessThan,
            "<=" => Op::LessThanEqualTo,
            ">" => Op::GreaterThan,
            ">=" => Op::GreaterThanEqualTo,
            "==" => Op::Equals,
            "!=" => Op::NotEqual,
            "&&" => Op::And,
            "||" => Op::Or,
            "(" => Op::LeftParen,
            "," => Op::Comma,
            "&" => Op::BitwiseAnd,
            "|" => Op::BitwiseOr,
            "~" => Op::BitwiseNot,
            "<<" => Op::BitShiftLeft,
            ">>" => Op::BitShiftRight,
            n => *ALL_OPS
                .iter()
                .find(|o| o.is_function() && FUNCTION_NAMES[**o as usize - 23] == n)?,
        })
    }

    /// The script tokens of the functions, `g_expOperatorNames[0x17..]`.
    const FUNCTION_NAMES: [&str; 58] = [
        "sin",
        "cos",
        "min",
        "max",
        "milliseconds",
        "dvarint",
        "dvarbool",
        "dvarfloat",
        "dvarstring",
        "stat",
        "ui_active",
        "flashbanged",
        "scoped",
        "scoreboard_visible",
        "inkillcam",
        "player",
        "selecting_location",
        "team",
        "otherteam",
        "marinesfield",
        "opforfield",
        "menuisopen",
        "writingdata",
        "inlobby",
        "inprivateparty",
        "privatepartyhost",
        "privatepartyhostinlobby",
        "aloneinparty",
        "adsjavelin",
        "weaplockblink",
        "weapattacktop",
        "weapattackdirect",
        "secondsastime",
        "tablelookup",
        "locstring",
        "localvarint",
        "localvarbool",
        "localvarfloat",
        "localvarstring",
        "timeleft",
        "secondsascountdown",
        "gamemsgwndactive",
        "int",
        "string",
        "float",
        "gametypename",
        "gametype",
        "gametypedescription",
        "scoreatrank",
        "friendsonline",
        "spectatingclient",
        "statrangeanybitsset",
        "keybinding",
        "actionslotusable",
        "hudfade",
        "maxrecommendedplayers",
        "acceptinginvite",
        "isintermission",
    ];

    fn run(src: &str) -> Value {
        eval(&stmt(src), &Mock::default())
    }

    fn run_with(src: &str, env: &Mock) -> Value {
        eval(&stmt(src), env)
    }

    fn i(v: i32) -> Value {
        Value::Int(v)
    }
    fn f(v: f32) -> Value {
        Value::Float(v)
    }
    fn s(v: &str) -> Value {
        Value::Str(v.into())
    }

    #[test]
    fn op_table_is_dense() {
        for (idx, op) in ALL_OPS.iter().enumerate() {
            assert_eq!(*op as usize, idx);
            assert_eq!(Op::from_code(idx as i32), Some(*op));
        }
        assert_eq!(Op::from_code(81), None);
        assert_eq!(Op::from_code(-1), None);
    }

    #[test]
    fn arithmetic_and_types() {
        assert_eq!(run("1 + 2"), i(3));
        assert_eq!(run("1 + 2.5"), f(3.5));
        assert_eq!(run("7 / 2"), f(3.5));
        assert_eq!(run("7 / 0"), f(0.0));
        assert_eq!(run("7 % 4"), i(3));
        assert_eq!(run("7 % 0"), i(7));
        assert_eq!(run("7.6 % 4"), i(0));
        assert_eq!(run("2 * 3.5"), f(7.0));
        assert_eq!(run("6 - 2"), i(4));
        assert_eq!(run("- 5"), i(-5));
        assert_eq!(run("- 2.5"), f(-2.5));
    }

    #[test]
    fn precedence_and_grouping() {
        assert_eq!(run("1 + 2 * 3"), i(7));
        assert_eq!(run("2 * 3 + 1"), i(7));
        assert_eq!(run("( 1 + 2 ) * 3"), i(9));
        assert_eq!(run("( 2 + 3 ) * ( 4 + 1 )"), i(25));
        // `!` binds tighter than && which has the same level as ||; equal associative operators group right.
        assert_eq!(run("! 0 && 1"), i(1));
        assert_eq!(
            run("! 1 && 0 || 1"),
            i(0),
            "(!1) && (0 || 1): && and || group right"
        );
        assert_eq!(
            run("0 && 0 || 1"),
            i(0),
            "&& and || share a level and group right"
        );
        assert_eq!(run("1 + 1 == 2"), i(1));
        assert_eq!(run("1 < 2 == 1"), i(1));
        assert_eq!(run("8 - 3 - 2"), i(3), "subtract groups left");
        assert_eq!(run("16 / 4 / 2"), f(2.0), "divide groups left");
        // Original quirk: mixed same-level operators group right.
        assert_eq!(run("10 - 3 + 2"), i(5), "10 - (3 + 2)");
        assert_eq!(run("6 << 1 + 1"), i(24), "+ binds tighter than <<");
    }

    #[test]
    fn comparisons() {
        assert_eq!(run("1 < 2"), i(1));
        assert_eq!(run("2 <= 2"), i(1));
        assert_eq!(run("3 > 4"), i(0));
        assert_eq!(run("4 >= 4.5"), i(0));
        assert_eq!(run("1 == 1.0"), i(1));
        assert_eq!(run("1 != 2"), i(1));
    }

    #[test]
    fn strings() {
        assert_eq!(run("\"Ab\" == \"aB\""), i(1), "case-insensitive");
        assert_eq!(run("\"a\" != \"b\""), i(1));
        assert_eq!(run("\"a\" + \"b\""), s("ab"));
        assert_eq!(run("\"n\" + 5"), s("n5"));
        assert_eq!(run("5 + \"n\""), s("5n"));
        assert_eq!(run("\"x\" + 1.5"), s("x1.500000"));
        // string vs number comparison reads the string as a number
        assert_eq!(run("\"5\" == 5"), i(1));
        assert_eq!(run("\"5.5\" > 5"), i(1));
        assert_eq!(run("\"abc\" == 0"), i(1));
        assert_eq!(run("\"\" == \"\""), i(1));
        let long = "x".repeat(300);
        let src = format!("\"{long}\" + \"y\"");
        assert_eq!(eval_string(&stmt(&src), &Mock::default()).len(), 255);
    }

    #[test]
    fn logic_and_bitwise() {
        assert_eq!(run("1 && 1"), i(1));
        assert_eq!(run("1 && 0"), i(0));
        assert_eq!(run("0 || 0.5"), i(1));
        assert_eq!(run("\"x\" && 1"), i(1));
        assert_eq!(run("\"\" || 0"), i(0));
        assert_eq!(run("\"1\" && \"0\""), i(0), "two strings read as numbers");
        assert_eq!(run("! 0"), i(1));
        assert_eq!(run("! 2.5"), i(0));
        assert_eq!(run("! \"x\""), i(0));
        assert_eq!(run("12 & 10"), i(8));
        assert_eq!(run("12 | 3"), i(15));
        assert_eq!(run("~ 0"), i(-1));
        assert_eq!(run("1 << 4"), i(16));
        assert_eq!(run("256 >> 4"), i(16));
        assert_eq!(run("3.9 & 7"), i(3));
    }

    #[test]
    fn functions_and_commas() {
        assert_eq!(run("min ( 3 , 1 , 2 )"), f(1.0));
        assert_eq!(run("max ( 3 , 1 , 2 )"), f(3.0));
        assert_eq!(run("max ( 1 + 1 , min ( 9 , 4 ) ) + 1"), f(5.0));
        assert_eq!(run("sin ( 0 )"), f(0.0));
        assert_eq!(run("cos ( 0 )"), f(1.0));
        assert_eq!(run("int ( 3.9 )"), i(3));
        assert_eq!(run("float ( \"2.5\" )"), f(2.5));
        assert_eq!(run("string ( 4 ) + string ( 2 )"), s("42"));
        assert_eq!(run("milliseconds ( )"), i(1234));
        // more than ten arguments collapse to the single int 0
        assert_eq!(
            run("min ( 1 , 2 , 3 , 4 , 5 , 6 , 7 , 8 , 9 , 10 , 11 )"),
            f(0.0),
            "the oversized list collapses to the single int 0, which min reads as 0.0"
        );
        // `- 1` after an operand parses as binary minus (original quirk)
        assert_eq!(run("5 * - 1"), Value::fallback());
    }

    #[test]
    fn pure_time_formatting() {
        assert_eq!(run("secondsascountdown ( 125 )"), s(" 2:05"));
        assert_eq!(run("secondsascountdown ( 5 )"), s(" 0:05"));
        assert_eq!(run("secondsascountdown ( 0 - 1 )"), s(""));
        assert_eq!(run("secondsastime ( 90061 )"), s("1d 1h 1m"));
    }

    #[test]
    fn dvars() {
        let mut env = Mock::default();
        env.dvars.insert("fs_game".into(), String::new());
        env.dvars.insert("n".into(), "7".into());
        env.dvars.insert("x".into(), "2.5".into());
        env.dvars.insert("name".into(), "Soap".into());
        assert_eq!(run_with("dvarint ( \"n\" ) + 1", &env), i(8));
        assert_eq!(run_with("dvarbool ( \"n\" )", &env), i(7));
        assert_eq!(run_with("dvarfloat ( \"x\" ) * 2", &env), f(5.0));
        assert_eq!(run_with("dvarstring ( \"name\" )", &env), s("Soap"));
        assert_eq!(run_with("dvarint ( \"missing\" )", &env), i(0));
        assert_eq!(run_with("dvarint ( 5 )", &env), i(0), "non-string name");
        // the decoded main-menu item `( dvarString("fs_game") == "" )`
        let main = stmt("( dvarstring ( \"fs_game\" ) == \"\" )");
        assert!(eval_bool(&main, &env));
        env.dvars.insert("fs_game".into(), "mods/x".into());
        assert!(!eval_bool(&main, &env));
        // `( dvarInt("developer") && !dvarInt("ui_hide") )` style
        env.dvars.insert("developer".into(), "1".into());
        let st = stmt("dvarint ( \"developer\" ) && ! dvarint ( \"ui_hide\" )");
        assert!(eval_bool(&st, &env));
    }

    #[test]
    fn locals_and_env_families() {
        let mut env = Mock::default();
        env.locals.insert("a".into(), LocalVar::Int(4));
        env.locals.insert("b".into(), LocalVar::Float(2.5));
        env.locals.insert("c".into(), LocalVar::Str("12abc".into()));
        env.stats.insert(252, 0b101);
        env.open_menus.push("quickmessage".into());
        env.tables.insert(
            ("mp/statstable.csv".into(), 0, "7".into(), 3),
            "ak47".into(),
        );
        assert_eq!(run_with("localvarint ( \"a\" )", &env), i(4));
        assert_eq!(run_with("localvarfloat ( \"a\" )", &env), f(4.0));
        assert_eq!(run_with("localvarint ( \"b\" )", &env), i(2));
        assert_eq!(run_with("localvarstring ( \"b\" )", &env), s("2.5"));
        assert_eq!(run_with("localvarstring ( \"a\" )", &env), s("4"));
        assert_eq!(run_with("localvarint ( \"c\" )", &env), i(12));
        assert_eq!(run_with("localvarbool ( \"c\" )", &env), i(1));
        assert_eq!(run_with("localvarint ( \"nope\" )", &env), i(0));
        assert_eq!(run_with("localvarstring ( \"nope\" )", &env), s(""));
        assert_eq!(run_with("ui_active ( )", &env), i(1));
        assert_eq!(run_with("flashbanged ( )", &env), i(0));
        assert_eq!(run_with("scoped ( )", &env), i(1));
        assert_eq!(run_with("scoreboard_visible ( )", &env), i(0));
        assert_eq!(run_with("inkillcam ( )", &env), i(1));
        assert_eq!(run_with("selecting_location ( )", &env), i(0));
        assert_eq!(run_with("adsjavelin ( )", &env), i(1));
        assert_eq!(run_with("isintermission ( )", &env), i(1));
        assert_eq!(run_with("spectatingclient ( )", &env), i(0));
        assert_eq!(run_with("timeleft ( )", &env), i(125));
        assert_eq!(run_with("stat ( 252 )", &env), i(5));
        assert_eq!(
            run_with("statrangeanybitsset ( 250 , 253 , 4 )", &env),
            i(1)
        );
        assert_eq!(
            run_with("statrangeanybitsset ( 250 , 253 , 2 )", &env),
            i(0)
        );
        assert_eq!(
            run_with("statrangeanybitsset ( 253 , 250 , 4 )", &env),
            i(0)
        );
        assert_eq!(run_with("statrangeanybitsset ( 1 , 2 )", &env), i(0));
        assert_eq!(run_with("menuisopen ( \"quickmessage\" )", &env), i(1));
        assert_eq!(run_with("menuisopen ( \"x\" )", &env), i(0));
        assert_eq!(
            run_with(
                "tablelookup ( \"mp/statstable.csv\" , 0 , \"7\" , 3 )",
                &env
            ),
            s("ak47")
        );
        assert_eq!(run_with("tablelookup ( \"f\" , 0 , \"7\" )", &env), s(""));
        assert_eq!(
            run_with("tablelookup ( \"mp/statstable.csv\" , 0 , 7 , 3 )", &env),
            s("ak47"),
            "numeric search value is stringified"
        );
        assert_eq!(run_with("gametype ( )", &env), s("war"));
        assert_eq!(run_with("gametypename ( )", &env), s("Team Deathmatch"));
        assert_eq!(run_with("gametypedescription ( )", &env), s("Kill"));
        assert_eq!(run_with("scoreatrank ( 3 )", &env), i(30));
        assert_eq!(run_with("scoreatrank ( 0 )", &env), i(0));
        assert_eq!(run_with("scoreatrank ( 1.5 )", &env), i(0));
        assert_eq!(run_with("gamemsgwndactive ( 1 )", &env), i(1));
        assert_eq!(
            run_with("keybinding ( \"+attack\" )", &env),
            s("key:+attack")
        );
        assert_eq!(run_with("actionslotusable ( 2 )", &env), i(1));
        assert_eq!(run_with("actionslotusable ( 1 )", &env), i(0));
        assert_eq!(run_with("actionslotusable ( 5 )", &env), i(0));
        assert_eq!(run_with("hudfade ( \"compass\" )", &env), f(0.75));
        assert_eq!(run_with("hudfade ( \"WEAPON\" )", &env), f(0.5));
        assert_eq!(run_with("hudfade ( \"bogus\" )", &env), f(0.0));
        assert_eq!(run_with("player ( \"teamname\" )", &env), s("Marines"));
        assert_eq!(run_with("player ( \"clipAmmo\" )", &env), i(7));
        assert_eq!(run_with("player ( \"kills\" )", &env), i(9));
        assert_eq!(run_with("player ( \"bogus\" )", &env), i(0));
        assert_eq!(run_with("player ( 3 )", &env), s(""));
        assert_eq!(run_with("team ( \"name\" )", &env), s("MARINES"));
        assert_eq!(run_with("marinesfield ( \"score\" )", &env), i(5));
        assert_eq!(run_with("opforfield ( \"score\" )", &env), i(3));
        assert_eq!(run_with("otherteam ( \"score\" )", &env), i(-1));
        assert_eq!(run_with("weaplockblink ( 2 )", &env), i(0));
        assert_eq!(run_with("weapattacktop ( )", &env), i(0));
        assert_eq!(run_with("weapattackdirect ( )", &env), i(0));
        for lobby in [
            "writingdata",
            "inlobby",
            "inprivateparty",
            "privatepartyhost",
            "privatepartyhostinlobby",
            "aloneinparty",
            "friendsonline",
            "maxrecommendedplayers",
            "acceptinginvite",
        ] {
            assert_eq!(run_with(&format!("{lobby} ( )"), &env), i(0), "{lobby}");
        }
    }

    #[test]
    fn localize_collects_parts() {
        let env = Mock::default();
        assert_eq!(
            run_with("locstring ( \"@MENU_A\" , \"@B\" , 5 , \"x\" )", &env),
            s("R:MENU_A|R:B|L:5")
        );
        assert_eq!(run_with("locstring ( \"@MENU_A\" )", &env), s("R:MENU_A"));
    }

    #[test]
    fn failed_statements_fall_back() {
        let env = Mock::default();
        for src in [
            "",
            "1 2",
            "1 +",
            "min ( 1 , 2 )  )  5",
            "( 1 , 2 )",
            "sin ( 1 , 2 )",
        ] {
            assert_eq!(eval(&stmt(src), &env), Value::fallback(), "{src:?}");
            assert!(!eval_bool(&stmt(src), &env), "{src:?}");
            assert_eq!(eval_float(&stmt(src), &env), 0.0);
            assert_eq!(eval_string(&stmt(src), &env), "");
        }
        assert_eq!(
            Compiled::new(&Statement::default()).unwrap_err(),
            ExprError::Empty
        );
        let bad = Statement {
            entries: vec![Arc::new(E::Operator(200))],
        };
        assert_eq!(
            Compiled::new(&bad).unwrap_err(),
            ExprError::BadOperator(200)
        );
        // 61 operands in a row
        let many = Statement {
            entries: (0..61).map(|n| Arc::new(E::Operand(O::Int(n)))).collect(),
        };
        assert_eq!(
            Compiled::new(&many).unwrap_err(),
            ExprError::TooManyOperands
        );
    }

    #[test]
    fn result_conversions() {
        let env = Mock::default();
        assert_eq!(eval_string(&stmt("1 + 1"), &env), "2");
        assert_eq!(eval_string(&stmt("1 / 4"), &env), "0.250000");
        assert_eq!(eval_float(&stmt("\"1.5\""), &env), 1.5);
        // the integer reading decides truth: 0.5 is false
        assert!(!eval_bool(&stmt("0.5"), &env));
        assert!(eval_bool(&stmt("\"2\""), &env));
    }

    #[test]
    fn compiled_is_reusable() {
        let mut env = Mock::default();
        let c = Compiled::new(&stmt("dvarint ( \"n\" ) * 2")).unwrap();
        env.dvars.insert("n".into(), "3".into());
        assert_eq!(c.eval(&env), i(6));
        env.dvars.insert("n".into(), "4".into());
        assert_eq!(c.eval(&env), i(8));
        assert!(c.eval_bool(&env));
        assert_eq!(c.eval_float(&env), 8.0);
        assert_eq!(c.eval_string(&env), "8");
    }

    #[test]
    fn c_style_parsers() {
        assert_eq!(atoi("  -12x"), -12);
        assert_eq!(atoi("+3"), 3);
        assert_eq!(atoi("x"), 0);
        assert_eq!(atoi("99999999999"), i32::MAX);
        assert_eq!(atof("2.5abc"), 2.5);
        assert_eq!(atof(".5"), 0.5);
        assert_eq!(atof("1e3x"), 1000.0);
        assert_eq!(atof("1e"), 1.0);
        assert_eq!(atof("-"), 0.0);
        assert_eq!(format_g(2.5), "2.5");
        assert_eq!(format_g(100000.0), "100000");
        assert_eq!(format_g(1234567.0), "1.23457e+06");
        assert_eq!(format_g(0.0001), "0.0001");
        assert_eq!(format_g(0.00001), "1e-05");
    }

    /// Decodes every statement of the installed `ui_mp` and `common_mp` menus; skipped without an install.
    #[test]
    fn installed_menus_compile_and_evaluate() {
        use assets::zone::{Asset, KeepAll, Zone};
        let Some(root) = std::env::var_os("COD4_PATH") else {
            eprintln!("COD4_PATH not set; skipping");
            return;
        };
        let mut total = 0usize;
        let mut empty = 0usize;
        let mut codes: BTreeMap<i32, usize> = BTreeMap::new();
        let mut failures = Vec::new();
        let env = Permissive;
        for zone_name in ["ui_mp", "common_mp"] {
            let path = std::path::Path::new(&root)
                .join("zone/english")
                .join(format!("{zone_name}.ff"));
            let Ok(file) = std::fs::File::open(&path) else {
                eprintln!("{} missing; skipping", path.display());
                continue;
            };
            let zone = Zone::open(std::io::BufReader::new(file)).expect("open zone");
            zone.decode(&KeepAll, |a| {
                let Asset::MenuList(list) = &a else { return };
                for m in list.menus.iter().flatten() {
                    let mut stmts: Vec<&Statement> =
                        vec![&m.visible_exp, &m.rect_x_exp, &m.rect_y_exp];
                    for it in &m.items {
                        stmts.extend([
                            &it.visible_exp,
                            &it.text_exp,
                            &it.material_exp,
                            &it.rect_x_exp,
                            &it.rect_y_exp,
                            &it.rect_w_exp,
                            &it.rect_h_exp,
                            &it.forecolor_a_exp,
                        ]);
                    }
                    for st in stmts {
                        if st.entries.is_empty() {
                            empty += 1;
                            continue;
                        }
                        total += 1;
                        for e in &st.entries {
                            if let E::Operator(c) = &**e {
                                *codes.entry(*c).or_default() += 1;
                            }
                        }
                        match Compiled::new(st) {
                            Ok(c) => {
                                c.eval(&env);
                                c.eval_bool(&env);
                                c.eval_float(&env);
                                c.eval_string(&env);
                            }
                            Err(e) => {
                                failures.push(format!("{zone_name} {:?}: {e}", m.window.name))
                            }
                        }
                    }
                }
            })
            .expect("decode");
        }
        eprintln!("statements: {total} non-empty, {empty} empty");
        eprintln!("operator codes: {codes:?}");
        assert!(
            failures.is_empty(),
            "{} failed, first: {:?}",
            failures.len(),
            &failures[..failures.len().min(5)]
        );
    }

    /// Env for the install sweep: every question has a defined non-trivial answer.
    struct Permissive;
    impl ExprEnv for Permissive {
        fn dvar_string(&self, _: &str) -> String {
            "1".into()
        }
        fn local_var(&self, _: &str) -> Option<LocalVar> {
            Some(LocalVar::Int(1))
        }
        fn milliseconds(&self) -> i32 {
            1000
        }
        fn ui_active(&self) -> bool {
            true
        }
        fn flashbanged(&self) -> bool {
            true
        }
        fn scoped(&self) -> bool {
            true
        }
        fn scoreboard_visible(&self) -> bool {
            true
        }
        fn in_killcam(&self) -> bool {
            true
        }
        fn player_field(&self, _: PlayerField) -> Value {
            Value::Int(1)
        }
        fn selecting_location(&self) -> bool {
            true
        }
        fn team_field(&self, _: TeamSel, _: TeamField) -> Value {
            Value::Int(1)
        }
        fn menu_is_open(&self, _: &str) -> bool {
            true
        }
        fn stat(&self, i: i32) -> i32 {
            i
        }
        fn localize(&self, _: &[LocPart]) -> String {
            "loc".into()
        }
        fn table_lookup(&self, _: &str, _: i32, _: &str, _: i32) -> String {
            "1".into()
        }
        fn time_left(&self) -> i32 {
            60
        }
        fn game_message_window_active(&self, _: i32) -> bool {
            true
        }
        fn gametype_name(&self) -> String {
            "gt".into()
        }
        fn gametype(&self) -> String {
            "war".into()
        }
        fn gametype_description(&self) -> String {
            "d".into()
        }
        fn score_at_rank(&self, r: i32) -> i32 {
            r
        }
        fn following(&self) -> bool {
            true
        }
        fn key_binding(&self, _: &str) -> String {
            "k".into()
        }
        fn action_slot_usable(&self, _: i32) -> bool {
            true
        }
        fn hud_fade(&self, _: HudFade) -> f32 {
            1.0
        }
        fn is_intermission(&self) -> bool {
            true
        }
        fn ads_javelin(&self) -> bool {
            true
        }
    }
}
