//! C: <X11/keysym.h> and xcb-util-keysyms (xcb_keysyms.c) — the key symbol
//! constants i3-input uses and the xcb_is_*_key() classifiers it prints.
//!
//! The numeric values are the ones of X11/keysymdef.h; the classifiers are the
//! exact range checks of xcb-util-keysyms so that the skip rule for modifier
//! and cursor keys behaves like C.

#![allow(non_upper_case_globals, dead_code)]

/// C: X11/keysymdef.h — NoSymbol.
pub const XK_NoSymbol: u32 = 0x0000_0000;
/// C: X11/keysymdef.h
pub const XK_BackSpace: u32 = 0xff08;
/// C: X11/keysymdef.h
pub const XK_Return: u32 = 0xff0d;
/// C: X11/keysymdef.h
pub const XK_Escape: u32 = 0xff1b;
/// C: X11/keysymdef.h
pub const XK_Mode_switch: u32 = 0xff7e;

/// C: X11/keysymdef.h — XK_Home .. XK_Down is the cursor key block.
pub const XK_Home: u32 = 0xff50;
/// C: X11/keysymdef.h
pub const XK_Down: u32 = 0xff54;

/// C: X11/keysymdef.h
pub const XK_Select: u32 = 0xff60;
/// C: X11/keysymdef.h
pub const XK_Break: u32 = 0xff6b;

/// C: X11/keysymdef.h
pub const XK_KP_Space: u32 = 0xff80;
/// C: X11/keysymdef.h — XK_KP_F1 .. XK_KP_F4
pub const XK_KP_F1: u32 = 0xff91;
/// C: X11/keysymdef.h
pub const XK_KP_F4: u32 = 0xff94;
/// C: X11/keysymdef.h
pub const XK_KP_Equal: u32 = 0xffbd;

/// C: X11/keysymdef.h
pub const XK_F1: u32 = 0xffbe;

/// C: X11/keysymdef.h — XK_Shift_L .. XK_Hyper_R is the modifier block.
pub const XK_Shift_L: u32 = 0xffe1;
/// C: X11/keysymdef.h
pub const XK_Hyper_R: u32 = 0xffee;
/// C: X11/keysymdef.h — XK_F1 .. XK_F35
pub const XK_F35: u32 = 0xffe0;

/// C: xcb/xproto.h — XCB_MOD_MASK_SHIFT.
pub const XCB_MOD_MASK_SHIFT: u16 = 1 << 0;

/// C: xcb-util-keysyms xcb_keysyms.c — xcb_is_keypad_key()
pub fn xcb_is_keypad_key(keysym: u32) -> bool {
    (XK_KP_Space..=XK_KP_Equal).contains(&keysym)
}

/// C: xcb-util-keysyms xcb_keysyms.c — xcb_is_private_keypad_key()
pub fn xcb_is_private_keypad_key(keysym: u32) -> bool {
    (XK_KP_F1..=XK_KP_F4).contains(&keysym)
}

/// C: xcb-util-keysyms xcb_keysyms.c — xcb_is_cursor_key()
pub fn xcb_is_cursor_key(keysym: u32) -> bool {
    (XK_Home..=XK_Down).contains(&keysym)
}

/// C: xcb-util-keysyms xcb_keysyms.c — xcb_is_pf_key()
pub fn xcb_is_pf_key(keysym: u32) -> bool {
    (XK_KP_F1..=XK_KP_F4).contains(&keysym)
}

/// C: xcb-util-keysyms xcb_keysyms.c — xcb_is_function_key()
pub fn xcb_is_function_key(keysym: u32) -> bool {
    (XK_F1..=XK_F35).contains(&keysym)
}

/// C: xcb-util-keysyms xcb_keysyms.c — xcb_is_misc_function_key()
pub fn xcb_is_misc_function_key(keysym: u32) -> bool {
    (XK_Select..=XK_Break).contains(&keysym)
}

/// C: xcb-util-keysyms xcb_keysyms.c — xcb_is_modifier_key()
///
/// The modifier block plus the XKB private modifier keys 0xfe01..0xfe13.
pub fn xcb_is_modifier_key(keysym: u32) -> bool {
    if (XK_Shift_L..=XK_Hyper_R).contains(&keysym) {
        return true;
    }
    if (0xfe01..=0xfe13).contains(&keysym) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_ranges_match_xcb_keysyms() {
        assert!(xcb_is_modifier_key(0xffe1));
        assert!(xcb_is_modifier_key(0xffee));
        assert!(!xcb_is_modifier_key(0xffdf));
        assert!(xcb_is_modifier_key(0xfe01));
        assert!(xcb_is_modifier_key(0xfe13));
        assert!(!xcb_is_modifier_key(0xfe14));

        assert!(xcb_is_cursor_key(0xff50));
        assert!(xcb_is_cursor_key(0xff54));
        assert!(!xcb_is_cursor_key(0xff55));

        assert!(xcb_is_function_key(0xffbe));
        assert!(xcb_is_function_key(0xffe0));
        assert!(!xcb_is_function_key(0xffe1));

        assert!(xcb_is_keypad_key(0xff80));
        assert!(xcb_is_keypad_key(0xffbd));
        assert!(!xcb_is_keypad_key(0xffbe));

        assert!(xcb_is_pf_key(0xff91));
        assert!(!xcb_is_pf_key(0xff95));

        assert!(xcb_is_misc_function_key(0xff60));
        assert!(xcb_is_misc_function_key(0xff6b));
        assert!(!xcb_is_misc_function_key(0xff6c));
    }
}
