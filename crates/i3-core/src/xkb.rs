//! The real libxkbcommon-backed keymap (C: the xkbcommon half of
//! src/bindings.c).
//!
//! C: src/bindings.c:16-18
//!     static struct xkb_context *xkb_context;
//!     static struct xkb_keymap *xkb_keymap;
//! and src/main.c:840-887 (xcb-xkb registration).
//!
//! src/bindings.rs keeps the 1:1 port of the C control flow and talks to the
//! keymap through the XkbKeymap / XkbKeymapFactory traits, whose test double
//! is FakeKeymap. This module is the production implementation of those traits:
//! RealKeymap wraps libxkbcommon and RealKeymapFactory wraps the
//! libxkbcommon-x11 constructors (which need the raw xcb_connection_t *).
//!
//! One difference to C is structural: C passes four separate struct xkb_state
//! objects (xkb_state, _no_shift, _numlock, _numlock_no_shift) into
//! add_keycode_if_matches. The trait hands a symbol lookup an XkbState value
//! (base_mods + group) instead, and RealKeymap re-applies that mask to its one
//! xkb_state before each query. The queries are interleaved exactly like C, so
//! the answers are identical; only the state objects are shared.

use std::cell::RefCell;
use std::ffi::{c_char, CString};

use libi3::xkbcommon_ffi as ffi;

use crate::backend::XConn;
use crate::bindings::{XkbKeymap, XkbKeymapFactory, XkbRuleNames, XkbState as BindingState};
// The DLOG/ELOG macros of C: libi3's log module is the one the rest of the
// bindings port (src/bindings.c) already uses.
use libi3::log::{debuglog, errorlog};

/// C: xcb/xkb.h XCB_XKB_MAJOR_VERSION
pub const XCB_XKB_MAJOR_VERSION: u16 = 1;
/// C: xcb/xkb.h XCB_XKB_MINOR_VERSION
pub const XCB_XKB_MINOR_VERSION: u16 = 0;
/// C: xcb/xkb.h XCB_XKB_ID_USE_CORE_KBD
pub const XCB_XKB_ID_USE_CORE_KBD: u16 = 0x0100;
/// C: the keysym constant C passes to aio_get_mod_mask_for() (xkbcommon
/// spells it XKB_KEY_Num_Lock; xcb_keysyms spells it XCB_NUM_LOCK).
pub const XCB_NUM_LOCK: u32 = 0xff7f;

/// C: struct xkb_keymap *xkb_keymap + the four dummy states, merged into one
/// keymap plus one reusable state.
pub struct RealKeymap {
    /// The single struct xkb_state. It is declared before the keymap because
    /// Rust drops struct fields in declaration order and xkbcommon requires
    /// the state to be destroyed before the keymap it was created from
    /// (C: xkb_state_unref() before xkb_keymap_unref() in load_keymap()).
    state: RefCell<ffi::XkbState>,
    keymap: ffi::XkbKeymap,
    /// C: the device id xkb_x11_get_core_keyboard_device_id() returned.
    pub device_id: i32,
}

impl RealKeymap {
    /// Builds the keymap wrapper, creating the xkb_state like
    /// translate_keysyms() calls xkb_state_new(xkb_keymap).
    pub fn new(keymap: ffi::XkbKeymap, device_id: i32) -> Option<RealKeymap> {
        let state = ffi::XkbState::new(&keymap)?;
        Some(RealKeymap {
            state: RefCell::new(state),
            keymap,
            device_id,
        })
    }

    /// C: xkb_state_update_mask(state, base_mods, 0, 0, 0, 0, group)
    fn update_mask(&self, state: &BindingState) {
        self.state
            .borrow_mut()
            .update_mask(state.base_mods, 0, 0, 0, 0, state.group);
    }

    /// C: xkb_keymap_key_get_syms_by_level(keymap, key, layout, level, &syms)
    pub fn key_get_syms_by_level(&self, key: u8, layout: u32, level: u32) -> Vec<u32> {
        self.keymap
            .key_get_syms_by_level(key as u32, layout, level)
            .to_vec()
    }
}

impl XkbKeymap for RealKeymap {
    fn keymap_key_for_each(&self, f: &mut dyn FnMut(u8)) {
        // C: xkb_keymap_key_for_each(xkb_keymap, add_keycode_if_matches, &resolving)
        self.keymap.key_for_each(f);
    }

    fn state_key_get_one_sym(&self, state: &BindingState, key: u8) -> u32 {
        // C: xkb_state_key_get_one_sym(resolving->xkb_state, key)
        self.update_mask(state);
        self.state.borrow().key_get_one_sym(key as u32)
    }

    fn state_key_get_layout(&self, state: &BindingState, key: u8) -> u32 {
        // C: xkb_state_key_get_layout(resolving->xkb_state, key)
        self.update_mask(state);
        self.state.borrow().key_get_layout(key as u32)
    }

    fn state_key_get_level(&self, state: &BindingState, key: u8, layout: u32) -> u32 {
        // C: xkb_state_key_get_level(resolving->xkb_state, key, layout)
        self.update_mask(state);
        self.state.borrow().key_get_level(key as u32, layout)
    }

    fn keysym_from_name(&self, name: &str) -> u32 {
        // C: xkb_keysym_from_name(bind->symbol, XKB_KEYSYM_NO_FLAGS)
        ffi::keysym_from_name(name)
    }

    fn keymap_key_get_syms_by_level(&self, key: u8, layout: u32, level: u32) -> Vec<u32> {
        // C: xkb_keymap_key_get_syms_by_level(keymap, key, layout, level, &syms)
        self.key_get_syms_by_level(key, layout, level)
    }
}

/// C: the xkb_rule_names of load_keymap() — NULL for the components
/// _XKB_RULES_NAMES did not provide.
fn rule_names_to_c(names: &XkbRuleNames, storage: &mut Vec<CString>) -> ffi::xkb_rule_names {
    fn component(value: &Option<String>, storage: &mut Vec<CString>) -> *const c_char {
        match value {
            Some(value) => {
                let c_string = match CString::new(value.as_bytes()) {
                    Ok(c_string) => c_string,
                    Err(_) => return std::ptr::null(),
                };
                let ptr = c_string.as_ptr();
                // The heap buffer of a CString does not move when the Vec of
                // CStrings grows, so the pointer stays valid until the call.
                storage.push(c_string);
                ptr
            }
            None => std::ptr::null(),
        }
    }

    ffi::xkb_rule_names {
        rules: component(&names.rules, storage),
        model: component(&names.model, storage),
        layout: component(&names.layout, storage),
        variant: component(&names.variant, storage),
        options: component(&names.options, storage),
    }
}

/// C: struct xkb_context *xkb_context (src/bindings.c:16) plus the
/// xkb_x11_* / xkb_keymap_new_from_names constructors of load_keymap().
pub struct RealKeymapFactory {
    context: ffi::XkbContext,
}

impl RealKeymapFactory {
    /// C: xkb_context_new(0) — the context only needs to exist once.
    pub fn new() -> Option<RealKeymapFactory> {
        Some(RealKeymapFactory {
            context: ffi::XkbContext::new()?,
        })
    }

    /// C: src/main.c:840-887 — the xcb-xkb registration:
    ///   xcb_xkb_use_extension(conn, 1, 0)
    ///   xcb_xkb_select_events(conn, XCB_XKB_ID_USE_CORE_KBD,
    ///       STATE_NOTIFY|MAP_NOTIFY|NEW_KEYBOARD_NOTIFY, 0, same, 0xff, 0xff, NULL)
    ///   xcb_xkb_per_client_flags(conn, XCB_XKB_ID_USE_CORE_KBD,
    ///       GRABS_USE_XKB_STATE|LOOKUP_STATE_WHEN_GRABBED|DETECTABLE_AUTO_REPEAT,
    ///       same, 0, 0, 0)
    ///
    /// xkb_x11_setup_xkb_extension() performs the use_extension handshake and
    /// reports the event/error bases; the other two requests are the libxcb-xkb
    /// functions, declared in libi3::xcb_xkb_ffi.
    /// Returns the event base (C: xkb_base) on success.
    pub fn setup_extension(&self, conn: &crate::xcb_util::XConnection) -> Option<i32> {
        let raw = conn.get_raw_xcb_connection();
        // C: xcb_xkb_use_extension(conn, XCB_XKB_MAJOR_VERSION, XCB_XKB_MINOR_VERSION)
        // is done by xkb_x11_setup_xkb_extension(), which also reports the event
        // base (C: extreply->first_event) and the supported version.
        let (major, minor, base_event, _base_error) =
            self.context
                .setup_x11_extension(raw, XCB_XKB_MAJOR_VERSION, XCB_XKB_MINOR_VERSION)?;
        debuglog(&format!("initializing xcb-xkb {} {}\n", major, minor));

        // C: xcb_xkb_select_events(conn, XCB_XKB_ID_USE_CORE_KBD,
        //     XCB_XKB_EVENT_TYPE_STATE_NOTIFY | MAP_NOTIFY | NEW_KEYBOARD_NOTIFY,
        //     0, same, 0xff, 0xff, NULL);
        let which = libi3::xcb_xkb_ffi::XCB_XKB_EVENT_TYPE_STATE_NOTIFY
            | libi3::xcb_xkb_ffi::XCB_XKB_EVENT_TYPE_MAP_NOTIFY
            | libi3::xcb_xkb_ffi::XCB_XKB_EVENT_TYPE_NEW_KEYBOARD_NOTIFY;
        if let Some(error_code) = libi3::xcb_xkb_ffi::select_events(
            raw,
            XCB_XKB_ID_USE_CORE_KBD,
            which,
            0,
            which,
            0xff,
            0xff,
        ) {
            // C ignores the request error; without the event selection the
            // group changes of handle_xkb_state_notify() never arrive, so the
            // port reports it and carries on with the keysym translation only.
            errorlog(&format!(
                "Could not select xkb events (X error {})\n",
                error_code
            ));
        }

        // C: const uint32_t mask = XCB_XKB_PER_CLIENT_FLAG_GRABS_USE_XKB_STATE |
        //     XCB_XKB_PER_CLIENT_FLAG_LOOKUP_STATE_WHEN_GRABBED |
        //     XCB_XKB_PER_CLIENT_FLAG_DETECTABLE_AUTO_REPEAT;
        // Setting both the grab flags makes the X server send the full XKB state
        // (including the keyboard group in bits 13/14) with KeyPress and
        // KeyRelease, which get_binding_from_xcb_event() decodes; detectable
        // auto repeat is what makes --release bindings fire once.
        let mask = libi3::xcb_xkb_ffi::XCB_XKB_PER_CLIENT_FLAG_GRABS_USE_XKB_STATE
            | libi3::xcb_xkb_ffi::XCB_XKB_PER_CLIENT_FLAG_LOOKUP_STATE_WHEN_GRABBED
            | libi3::xcb_xkb_ffi::XCB_XKB_PER_CLIENT_FLAG_DETECTABLE_AUTO_REPEAT;
        match libi3::xcb_xkb_ffi::per_client_flags(raw, XCB_XKB_ID_USE_CORE_KBD, mask, mask) {
            Some((_supported, value)) => {
                for (flag, name) in [
                    (
                        libi3::xcb_xkb_ffi::XCB_XKB_PER_CLIENT_FLAG_GRABS_USE_XKB_STATE,
                        "XCB_XKB_PER_CLIENT_FLAG_GRABS_USE_XKB_STATE",
                    ),
                    (
                        libi3::xcb_xkb_ffi::XCB_XKB_PER_CLIENT_FLAG_LOOKUP_STATE_WHEN_GRABBED,
                        "XCB_XKB_PER_CLIENT_FLAG_LOOKUP_STATE_WHEN_GRABBED",
                    ),
                    (
                        libi3::xcb_xkb_ffi::XCB_XKB_PER_CLIENT_FLAG_DETECTABLE_AUTO_REPEAT,
                        "XCB_XKB_PER_CLIENT_FLAG_DETECTABLE_AUTO_REPEAT",
                    ),
                ] {
                    if value & flag == 0 {
                        // C: PCF_REPLY_ERROR(_value)
                        errorlog(&format!("Could not set {}\n", name));
                    }
                }
            }
            None => {
                // C: pcf_reply == NULL falls into PCF_REPLY_ERROR for each
                // flag; the request itself failed.
                errorlog("Could not set the xkb per-client flags\n");
            }
        }

        Some(i32::from(base_event))
    }
}

impl XkbKeymapFactory for RealKeymapFactory {
    fn core_keyboard_device_id(&mut self, x: &mut dyn XConn) -> i32 {
        // C: xkb_x11_get_core_keyboard_device_id(conn)
        match x.raw_xcb_connection() {
            Some(raw) => self.context.core_keyboard_device_id(raw),
            // No X server behind the connection (test doubles): fall through to
            // the RMLVO path of load_keymap().
            None => -1,
        }
    }

    fn keymap_new_from_device(
        &mut self,
        x: &mut dyn XConn,
        device_id: i32,
    ) -> Option<Box<dyn XkbKeymap>> {
        // C: xkb_x11_keymap_new_from_device(xkb_context, conn, device_id, 0)
        let raw = x.raw_xcb_connection()?;
        let keymap = self.context.keymap_from_device(raw, device_id)?;
        RealKeymap::new(keymap, device_id).map(|keymap| Box::new(keymap) as Box<dyn XkbKeymap>)
    }

    fn keymap_new_from_names(&mut self, names: &XkbRuleNames) -> Option<Box<dyn XkbKeymap>> {
        // C: xkb_keymap_new_from_names(xkb_context, &names, 0) with NULL for
        // the components _XKB_RULES_NAMES did not provide.
        let mut storage: Vec<CString> = Vec::new();
        let c_names = rule_names_to_c(names, &mut storage);
        let keymap = self.context.keymap_from_names(&c_names)?;
        RealKeymap::new(keymap, -1).map(|keymap| Box::new(keymap) as Box<dyn XkbKeymap>)
    }
}

/******************************************************************************
 * get_mod_mask_for (C: libi3/get_mod_mask.c)
 ******************************************************************************/

/// The xcb_key_symbols_t of libi3/get_mod_mask.c, answered by the xkb keymap:
/// a keycode matches when the keysym appears on any of its groups/levels.
struct KeymapSymbols<'a>(&'a dyn XkbKeymap);

impl libi3::get_mod_mask::KeySymbols for KeymapSymbols<'_> {
    /// C: libi3/get_mod_mask.c:57 xcb_key_symbols_get_keycode(symbols, keysym)
    fn keycodes_for_keysym(&self, keysym: u32) -> Vec<u8> {
        keycodes_for_keysym(self.0, keysym)
    }
}

/// C: libi3/get_mod_mask.c:48 get_mod_mask_for — returns the modifier mask
/// (XCB_MOD_MASK_*) for the given keysym, for example for XCB_NUM_LOCK
/// (usually configured to mod2).
///
/// C resolves the keycodes of the keysym with xcb_key_symbols_get_keycode();
/// the port walks the xkb keymap instead. The mask computation itself is
/// libi3's (one implementation, not a second copy here).
pub fn get_mod_mask_for(keymap: &dyn XkbKeymap, keysym: u32, modmap: &[u8]) -> u32 {
    libi3::get_mod_mask::get_mod_mask_for(&KeymapSymbols(keymap), keysym, modmap)
}

/// C: libi3/get_mod_mask.c:24 aio_get_mod_mask_for — the all-in-one version:
/// one GetModifierMapping round-trip, then get_mod_mask_for().
///
/// C calls this before load_keymap() and resolves the keysym through
/// xcb_key_symbols; the port needs the xkb keymap for that, so the caller
/// passes it (main.c order becomes: load_keymap, numlock mask, translate).
pub fn aio_get_mod_mask_for(x: &mut dyn XConn, keymap: &dyn XkbKeymap, keysym: u32) -> u32 {
    /* C: libi3/get_mod_mask.c:24 — xcb_flush + the modifier mapping round trip
     * live in libi3; the XConn serves them through Libi3Conn. */
    libi3::get_mod_mask::aio_get_mod_mask_for(
        &mut crate::xcb_util::Libi3Conn(x),
        &KeymapSymbols(keymap),
        keysym,
    )
}

/// C: the xcb_key_symbols_get_keycode(symbols, keysym) scan — every keycode
/// that produces keysym on some group/level.
fn keycodes_for_keysym(keymap: &dyn XkbKeymap, keysym: u32) -> Vec<u8> {
    let mut codes = Vec::new();
    keymap.keymap_key_for_each(&mut |key| {
        'groups: for group in 0..XKB_MAX_GROUPS {
            for level in 0..XKB_MAX_LEVELS {
                for sym in keymap.keymap_key_get_syms_by_level(key, group, level) {
                    if sym == keysym {
                        if !codes.contains(&key) {
                            codes.push(key);
                        }
                        break 'groups;
                    }
                }
            }
        }
    });
    codes
}

/// C: xkbcommon.h XKB_MAX_GROUPS
const XKB_MAX_GROUPS: u32 = 4;
/// C: xkbcommon.h XKB_MAX_LEVELS
const XKB_MAX_LEVELS: u32 = 8;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{
        configure_binding, translate_keysyms, Bindings, NullHooks, XkbKeymap,
        XkbState as BindingState,
    };
    use crate::data::InputType;
    use crate::tree::Tree;
    use crate::xcb_util::RecordingConn;

    /// A tiny keymap test double for the modifier-mask helpers (the keycode ->
    /// keysym mapping xcb_key_symbols_get_keycode() would answer).
    #[derive(Default)]
    struct TestKeymap {
        keys: Vec<(u8, u32)>,
    }

    impl TestKeymap {
        fn add(&mut self, keycode: u8, keysym: u32) {
            self.keys.push((keycode, keysym));
        }
    }

    impl XkbKeymap for TestKeymap {
        fn keymap_key_for_each(&self, f: &mut dyn FnMut(u8)) {
            for (keycode, _) in &self.keys {
                f(*keycode);
            }
        }

        fn state_key_get_one_sym(&self, _state: &BindingState, key: u8) -> u32 {
            self.keys
                .iter()
                .find(|(keycode, _)| *keycode == key)
                .map(|(_, keysym)| *keysym)
                .unwrap_or(0)
        }

        fn state_key_get_layout(&self, _state: &BindingState, _key: u8) -> u32 {
            0
        }

        fn state_key_get_level(&self, _state: &BindingState, _key: u8, _layout: u32) -> u32 {
            0
        }

        fn keysym_from_name(&self, _name: &str) -> u32 {
            0
        }

        fn keymap_key_get_syms_by_level(&self, key: u8, layout: u32, level: u32) -> Vec<u32> {
            if layout != 0 || level != 0 {
                return Vec::new();
            }
            self.keys
                .iter()
                .find(|(keycode, _)| *keycode == key)
                .map(|(_, keysym)| vec![*keysym])
                .unwrap_or_default()
        }
    }

    /// C: libi3/get_mod_mask.c get_mod_mask_for — Num_Lock lives on Mod2, so
    /// the mask is 1 << 4.
    #[test]
    fn get_mod_mask_for_finds_numlock_on_mod2() {
        let mut keymap = TestKeymap::default();
        keymap.add(77, XCB_NUM_LOCK);
        // 8 modifiers x 4 keycodes, like the reply of a normal X server.
        let mut modmap = vec![0u8; 32];
        modmap[4 * 4] = 77;
        assert_eq!(get_mod_mask_for(&keymap, XCB_NUM_LOCK, &modmap), 1 << 4);
        // A different keysym has no modifier.
        assert_eq!(get_mod_mask_for(&keymap, 0x61, &modmap), 0);
        // A short/absent modifier mapping answers 0 (the C NULL reply path).
        assert_eq!(get_mod_mask_for(&keymap, XCB_NUM_LOCK, &[]), 0);
    }

    /// C: libi3/get_mod_mask.c aio_get_mod_mask_for — one GetModifierMapping
    /// round-trip through the X connection.
    #[test]
    fn aio_get_mod_mask_for_reads_the_modifier_mapping() {
        let mut keymap = TestKeymap::default();
        keymap.add(77, XCB_NUM_LOCK);
        let mut conn = RecordingConn::new(0x100);
        let mut modmap = vec![0u8; 32];
        modmap[4 * 4] = 77;
        conn.modifier_mapping = modmap;
        assert_eq!(
            aio_get_mod_mask_for(&mut conn, &keymap, XCB_NUM_LOCK),
            1 << 4
        );
        assert!(conn.log.iter().any(|line| line == "get_modifier_mapping"));
        assert!(conn.log.iter().any(|line| line == "flush"));
    }

    /// C: xkb_keymap_new_from_names("us") + translate_keysyms() — the real
    /// libxkbcommon keymap resolves the keysym names of the config to the
    /// evdev keycodes (no X server involved).
    #[test]
    fn real_keymap_translates_keysyms_to_keycodes() {
        let mut factory = match RealKeymapFactory::new() {
            Some(factory) => factory,
            None => panic!("xkb_context_new failed"),
        };
        let names = XkbRuleNames {
            layout: Some("us".to_string()),
            ..Default::default()
        };
        let keymap = factory
            .keymap_new_from_names(&names)
            .expect("xkb_keymap_new_from_names(us)");
        assert!(!keymap.keymap_key_get_syms_by_level(38, 0, 0).is_empty());

        let mut b = Bindings::new();
        b.keymap = Some(keymap);
        let mut t = Tree::new();
        let bind = configure_binding(
            &mut b,
            "bindsym",
            Some("Mod4"),
            "Return",
            None,
            None,
            None,
            None,
            "mode resize",
            crate::config_directives::DEFAULT_BINDING_MODE,
            false,
        )
        .expect("configure_binding");
        t.bindings.push_back(bind);

        let mut hooks = NullHooks;
        translate_keysyms(&b, &mut t, &mut hooks);

        let bind = &t.bindings[0];
        // xkbcommon 1.13 spells the modifier bits of the X core events; bit 6
        // is Mod4, so the translated keycode has to carry it.
        assert!(bind.keycodes_head.iter().any(|keycode| {
            keycode.keycode == 36 && keycode.modifiers & crate::ipc::XCB_KEY_BUT_MASK_MOD_4 != 0
        }));
        // "Return" is keycode 36 on the evdev us layout.
        assert!(bind
            .keycodes_head
            .iter()
            .all(|keycode| keycode.keycode == 36));
    }

    /// C: the key is grabbed with the translated modifier mask — the same
    /// translate path is what grab_all_keys() then uses.
    #[test]
    fn real_keymap_binding_is_a_keyboard_binding() {
        let mut b = Bindings::new();
        let bind = configure_binding(
            &mut b,
            "bindsym",
            Some("Mod1"),
            "d",
            None,
            None,
            None,
            None,
            "nop",
            crate::config_directives::DEFAULT_BINDING_MODE,
            false,
        )
        .expect("configure_binding");
        assert_eq!(bind.input_type, InputType::B_KEYBOARD);
        assert_eq!(bind.symbol.as_deref(), Some("d"));
    }
}
