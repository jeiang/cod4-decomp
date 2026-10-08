// SPDX-License-Identifier: GPL-3.0-or-later
//! When the window holds the mouse in a match. Mouse look only works while the pointer is locked, so every way into
//! the game must end with the lock taken, with no click needed (the original does the same when a menu closes):
//! the first frame of a match, a menu closing, the window regaining focus. The one exception is the player letting go
//! on purpose (Escape with no menu to open): a click takes the pointer back.

#[derive(Debug, Default)]
pub struct PointerGate {
    /// The player let go on purpose and has not clicked since.
    loose: bool,
}

impl PointerGate {
    /// Escape ran `togglemenu` and there was no menu to open.
    pub fn let_go(&mut self) {
        self.loose = true;
    }

    /// A click in the game window.
    pub fn click(&mut self) {
        self.loose = false;
    }

    /// Whether the pointer should be locked this frame. A menu open at all ends a deliberate release: closing it is
    /// the way back into the game.
    pub fn wants_lock(&mut self, in_match: bool, menu_open: bool, focused: bool) -> bool {
        if menu_open {
            self.loose = false;
        }
        in_match && focused && !menu_open && !self.loose
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_match_takes_the_pointer_without_a_click() {
        let mut g = PointerGate::default();
        assert!(
            !g.wants_lock(false, false, true),
            "the front end has no look"
        );
        assert!(g.wants_lock(true, false, true));
    }

    #[test]
    fn closing_any_menu_takes_the_pointer_back() {
        let mut g = PointerGate::default();
        // The server's team and class menus, then the player's own Escape menu.
        for _ in 0..3 {
            assert!(g.wants_lock(true, false, true));
            assert!(!g.wants_lock(true, true, true), "a menu needs the cursor");
            assert!(g.wants_lock(true, false, true), "the menu closed");
        }
    }

    #[test]
    fn letting_go_holds_until_a_click_or_a_menu() {
        let mut g = PointerGate::default();
        g.let_go();
        for _ in 0..5 {
            assert!(!g.wants_lock(true, false, true));
        }
        g.click();
        assert!(g.wants_lock(true, false, true));
        g.let_go();
        assert!(!g.wants_lock(true, false, true));
        // A menu opened by the server in the meantime ends the release when it closes.
        assert!(!g.wants_lock(true, true, true));
        assert!(g.wants_lock(true, false, true));
    }

    #[test]
    fn an_unfocused_window_keeps_its_hands_off_and_regains_it_on_focus() {
        let mut g = PointerGate::default();
        assert!(!g.wants_lock(true, false, false));
        assert!(g.wants_lock(true, false, true));
    }
}
