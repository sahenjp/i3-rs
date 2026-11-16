//! Hand-declared FFI for libxkbcommon and libxkbcommon-x11.
//!
//! C: src/bindings.c:13-14
//!     #include <xkbcommon/xkbcommon-x11.h>
//!     #include <xkbcommon/xkbcommon.h>
//!
//! The system has the shared objects but no xkbcommon-x11 development files
//! (no xkbcommon-x11.h, no xkbcommon-x11.pc), so the declarations are written
//! out by hand like crates/libi3/src/cairo_ffi.rs does for cairo/pango. The
//! link flags live in build.rs (the .so files have no plain-name symlink).
//!
//! The wrappers below are the thin safe layer: one Rust type per opaque C type,
//! with Drop calling the matching xkb_*_unref(). They carry no i3 semantics;
//! the port of src/bindings.c (i3-core/src/xkb.rs) builds the xkb_state dance
//! of translate_keysyms() on top of them.

// The C names (xkb_keysym_t, XKB_KEY_NoSymbol, …) are kept 1:1, like the rest
// of the port.
#![allow(non_camel_case_types, non_upper_case_globals)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};

/* Opaque C types (xkbcommon.h). They are never dereferenced from Rust. */
#[repr(C)]
pub struct xkb_context {
    _private: [u8; 0],
}
#[repr(C)]
pub struct xkb_keymap {
    _private: [u8; 0],
}
#[repr(C)]
pub struct xkb_state {
    _private: [u8; 0],
}

/// C: xkbcommon.h xkb_keysym_t
pub type xkb_keysym_t = u32;
/// C: xkbcommon.h xkb_keycode_t
pub type xkb_keycode_t = u32;
/// C: xkbcommon.h xkb_layout_index_t
pub type xkb_layout_index_t = u32;
/// C: xkbcommon.h xkb_level_index_t
pub type xkb_level_index_t = u32;
/// C: xkbcommon.h xkb_mod_mask_t
pub type xkb_mod_mask_t = u32;
/// C: xkbcommon.h enum xkb_state_component
pub type xkb_state_component = u32;
/// C: xkbcommon.h enum xkb_context_flags
pub type xkb_context_flags = u32;
/// C: xkbcommon.h enum xkb_keymap_compile_flags
pub type xkb_keymap_compile_flags = u32;
/// C: xkbcommon.h enum xkb_keysym_flags
pub type xkb_keysym_flags = u32;

/// C: xkbcommon.h XKB_CONTEXT_NO_FLAGS
pub const XKB_CONTEXT_NO_FLAGS: xkb_context_flags = 0;
/// C: xkbcommon.h XKB_KEYMAP_COMPILE_NO_FLAGS
pub const XKB_KEYMAP_COMPILE_NO_FLAGS: xkb_keymap_compile_flags = 0;
/// C: xkbcommon.h XKB_KEYSYM_NO_FLAGS
pub const XKB_KEYSYM_NO_FLAGS: xkb_keysym_flags = 0;
/// C: xkbcommon.h XKB_KEY_NoSymbol
pub const XKB_KEY_NoSymbol: xkb_keysym_t = 0;
/// C: xkbcommon.h XKB_LAYOUT_INVALID
pub const XKB_LAYOUT_INVALID: xkb_layout_index_t = u32::MAX;
/// C: xkbcommon-x11.h XKB_X11_SETUP_XKB_EXTENSION_NO_FLAGS
pub const XKB_X11_SETUP_XKB_EXTENSION_NO_FLAGS: u32 = 0;

/// C: xkbcommon.h struct xkb_rule_names
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct xkb_rule_names {
    pub rules: *const c_char,
    pub model: *const c_char,
    pub layout: *const c_char,
    pub variant: *const c_char,
    pub options: *const c_char,
}

/// C: xkbcommon.h xkb_keymap_key_iter_t
/// typedef void (*xkb_keymap_key_iter_t)(struct xkb_keymap *keymap,
///                                       xkb_keycode_t key, void *data);
pub type xkb_keymap_key_iter_t = extern "C" fn(*mut xkb_keymap, xkb_keycode_t, *mut c_void);
extern "C" {
    /* C: xkbcommon.h */
    fn xkb_context_new(flags: xkb_context_flags) -> *mut xkb_context;
    fn xkb_context_unref(context: *mut xkb_context);

    fn xkb_keymap_new_from_names(
        context: *mut xkb_context,
        names: *const xkb_rule_names,
        flags: xkb_keymap_compile_flags,
    ) -> *mut xkb_keymap;
    fn xkb_keymap_unref(keymap: *mut xkb_keymap);
    fn xkb_keymap_key_for_each(
        keymap: *mut xkb_keymap,
        iter: xkb_keymap_key_iter_t,
        data: *mut c_void,
    );
    fn xkb_keymap_key_get_syms_by_level(
        keymap: *mut xkb_keymap,
        key: xkb_keycode_t,
        layout: xkb_layout_index_t,
        level: xkb_level_index_t,
        syms_out: *mut *const xkb_keysym_t,
    ) -> c_int;

    fn xkb_state_new(keymap: *mut xkb_keymap) -> *mut xkb_state;
    fn xkb_state_unref(state: *mut xkb_state);
    fn xkb_state_update_mask(
        state: *mut xkb_state,
        depressed_mods: xkb_mod_mask_t,
        latched_mods: xkb_mod_mask_t,
        locked_mods: xkb_mod_mask_t,
        depressed_layout: xkb_layout_index_t,
        latched_layout: xkb_layout_index_t,
        locked_layout: xkb_layout_index_t,
    ) -> xkb_state_component;
    fn xkb_state_key_get_syms(
        state: *mut xkb_state,
        key: xkb_keycode_t,
        syms_out: *mut *const xkb_keysym_t,
    ) -> c_int;
    fn xkb_state_key_get_one_sym(state: *mut xkb_state, key: xkb_keycode_t) -> xkb_keysym_t;
    fn xkb_state_key_get_layout(state: *mut xkb_state, key: xkb_keycode_t) -> xkb_layout_index_t;
    fn xkb_state_key_get_level(
        state: *mut xkb_state,
        key: xkb_keycode_t,
        layout: xkb_layout_index_t,
    ) -> xkb_level_index_t;
    fn xkb_state_serialize_mods(
        state: *mut xkb_state,
        components: xkb_state_component,
    ) -> xkb_mod_mask_t;

    fn xkb_keysym_from_name(name: *const c_char, flags: xkb_keysym_flags) -> xkb_keysym_t;
    fn xkb_keysym_get_name(keysym: xkb_keysym_t, buffer: *mut c_char, size: usize) -> c_int;
    fn xkb_keysym_to_utf8(keysym: xkb_keysym_t, buffer: *mut c_char, size: usize) -> c_int;
    fn xkb_keysym_to_utf32(keysym: xkb_keysym_t) -> u32;

    /* C: xkbcommon-x11.h — all of these take the raw xcb_connection_t *. */
    fn xkb_x11_setup_xkb_extension(
        connection: *mut c_void,
        major_xkb_version: u16,
        minor_xkb_version: u16,
        flags: u32,
        major_xkb_version_out: *mut u16,
        minor_xkb_version_out: *mut u16,
        base_event_out: *mut u8,
        base_error_out: *mut u8,
    ) -> c_int;
    fn xkb_x11_get_core_keyboard_device_id(connection: *mut c_void) -> i32;
    fn xkb_x11_keymap_new_from_device(
        context: *mut xkb_context,
        connection: *mut c_void,
        device_id: i32,
        flags: xkb_keymap_compile_flags,
    ) -> *mut xkb_keymap;
    fn xkb_x11_state_new_from_device(
        keymap: *mut xkb_keymap,
        connection: *mut c_void,
        device_id: i32,
    ) -> *mut xkb_state;
}
/******************************************************************************
 * xkb_context
 ******************************************************************************/

/// C: struct xkb_context *xkb_context (src/bindings.c:16).
pub struct XkbContext(*mut xkb_context);

impl XkbContext {
    /// C: xkb_context_new(0)
    pub fn new() -> Option<XkbContext> {
        let context = unsafe { xkb_context_new(XKB_CONTEXT_NO_FLAGS) };
        if context.is_null() {
            None
        } else {
            Some(XkbContext(context))
        }
    }

    /// C: src/main.c:840-887 — xkb_x11_setup_xkb_extension().
    ///
    /// Returns (major, minor, base_event, base_error) on success.
    pub fn setup_x11_extension(
        &self,
        conn: *mut c_void,
        major: u16,
        minor: u16,
    ) -> Option<(u16, u16, u8, u8)> {
        let mut major_out: u16 = 0;
        let mut minor_out: u16 = 0;
        let mut base_event: u8 = 0;
        let mut base_error: u8 = 0;
        let ok = unsafe {
            xkb_x11_setup_xkb_extension(
                conn,
                major,
                minor,
                XKB_X11_SETUP_XKB_EXTENSION_NO_FLAGS,
                &mut major_out,
                &mut minor_out,
                &mut base_event,
                &mut base_error,
            )
        };
        if ok == 0 {
            None
        } else {
            Some((major_out, minor_out, base_event, base_error))
        }
    }

    /// C: xkb_x11_get_core_keyboard_device_id(conn)
    pub fn core_keyboard_device_id(&self, conn: *mut c_void) -> i32 {
        unsafe { xkb_x11_get_core_keyboard_device_id(conn) }
    }

    /// C: xkb_x11_keymap_new_from_device(context, conn, device_id, 0)
    pub fn keymap_from_device(&self, conn: *mut c_void, device_id: i32) -> Option<XkbKeymap> {
        let keymap = unsafe {
            xkb_x11_keymap_new_from_device(self.0, conn, device_id, XKB_KEYMAP_COMPILE_NO_FLAGS)
        };
        if keymap.is_null() {
            None
        } else {
            Some(XkbKeymap(keymap))
        }
    }

    /// C: xkb_keymap_new_from_names(context, &names, 0)
    pub fn keymap_from_names(&self, names: &xkb_rule_names) -> Option<XkbKeymap> {
        let keymap =
            unsafe { xkb_keymap_new_from_names(self.0, names, XKB_KEYMAP_COMPILE_NO_FLAGS) };
        if keymap.is_null() {
            None
        } else {
            Some(XkbKeymap(keymap))
        }
    }
}

impl Drop for XkbContext {
    fn drop(&mut self) {
        // C: xkb_context_unref(xkb_context)
        unsafe { xkb_context_unref(self.0) };
    }
}
/******************************************************************************
 * xkb_keymap
 ******************************************************************************/

/// C: struct xkb_keymap *xkb_keymap (src/bindings.c:17).
pub struct XkbKeymap(*mut xkb_keymap);

impl XkbKeymap {
    pub fn as_ptr(&self) -> *mut xkb_keymap {
        self.0
    }

    /// C: xkb_keymap_key_for_each(xkb_keymap, add_keycode_if_matches, &resolving)
    ///
    /// The C callback receives the keymap, the keycode and the opaque data
    /// pointer; the trampoline below unwraps the Rust closure from the data
    /// pointer and hands it only the keycode. The keycode is narrowed to
    /// xcb_keycode_t (8 bit) here because that is the type of
    /// struct Binding_Keycode (include/data.h), exactly like the implicit
    /// conversion in the C macro ADD_TRANSLATED_KEY.
    pub fn key_for_each(&self, f: &mut dyn FnMut(u8)) {
        struct TrampolineCtx<'a> {
            f: &'a mut dyn FnMut(u8),
        }

        extern "C" fn trampoline(_keymap: *mut xkb_keymap, key: xkb_keycode_t, data: *mut c_void) {
            // SAFETY: data is the &mut TrampolineCtx that key_for_each put on
            // the stack for the duration of the C call.
            let ctx = unsafe { &mut *(data as *mut TrampolineCtx<'_>) };
            (ctx.f)(key as u8);
        }

        let mut ctx = TrampolineCtx { f };
        unsafe {
            xkb_keymap_key_for_each(
                self.0,
                trampoline,
                &mut ctx as *mut TrampolineCtx<'_> as *mut c_void,
            )
        };
    }

    /// C: xkb_keymap_key_get_syms_by_level(keymap, key, layout, level, &syms)
    ///
    /// Returns the symbols of the level as a slice; empty when the key has no
    /// symbols there.
    pub fn key_get_syms_by_level(
        &self,
        key: xkb_keycode_t,
        layout: xkb_layout_index_t,
        level: xkb_level_index_t,
    ) -> &[xkb_keysym_t] {
        let mut syms: *const xkb_keysym_t = std::ptr::null();
        let count =
            unsafe { xkb_keymap_key_get_syms_by_level(self.0, key, layout, level, &mut syms) };
        if count <= 0 || syms.is_null() {
            &[]
        } else {
            // SAFETY: xkbcommon returns a pointer to count keysyms owned by
            // the keymap, which outlives this borrow.
            unsafe { std::slice::from_raw_parts(syms, count as usize) }
        }
    }
}

impl Drop for XkbKeymap {
    fn drop(&mut self) {
        // C: xkb_keymap_unref(xkb_keymap)
        unsafe { xkb_keymap_unref(self.0) };
    }
}
/******************************************************************************
 * xkb_state
 ******************************************************************************/

/// C: struct xkb_state *dummy_state (src/bindings.c:450-453).
pub struct XkbState(*mut xkb_state);

impl XkbState {
    /// C: xkb_state_new(xkb_keymap)
    pub fn new(keymap: &XkbKeymap) -> Option<XkbState> {
        let state = unsafe { xkb_state_new(keymap.0) };
        if state.is_null() {
            None
        } else {
            Some(XkbState(state))
        }
    }

    /// C: xkb_x11_state_new_from_device(keymap, conn, device_id) — the X11 state
    /// constructor, kept next to xkb_state_new (the one bindings.c uses).
    pub fn new_from_device(
        keymap: &XkbKeymap,
        conn: *mut c_void,
        device_id: i32,
    ) -> Option<XkbState> {
        let state = unsafe { xkb_x11_state_new_from_device(keymap.0, conn, device_id) };
        if state.is_null() {
            None
        } else {
            Some(XkbState(state))
        }
    }

    /// C: xkb_state_update_mask(state, base_mods, 0, 0, 0, 0, group)
    pub fn update_mask(
        &mut self,
        base_mods: xkb_mod_mask_t,
        latched_mods: xkb_mod_mask_t,
        locked_mods: xkb_mod_mask_t,
        base_group: xkb_layout_index_t,
        latched_group: xkb_layout_index_t,
        locked_group: xkb_layout_index_t,
    ) -> xkb_state_component {
        unsafe {
            xkb_state_update_mask(
                self.0,
                base_mods,
                latched_mods,
                locked_mods,
                base_group,
                latched_group,
                locked_group,
            )
        }
    }

    /// C: xkb_state_key_get_one_sym(state, key)
    pub fn key_get_one_sym(&self, key: xkb_keycode_t) -> xkb_keysym_t {
        unsafe { xkb_state_key_get_one_sym(self.0, key) }
    }

    /// C: xkb_state_key_get_syms(state, key, &syms)
    pub fn key_get_syms(&self, key: xkb_keycode_t) -> &[xkb_keysym_t] {
        let mut syms: *const xkb_keysym_t = std::ptr::null();
        let count = unsafe { xkb_state_key_get_syms(self.0, key, &mut syms) };
        if count <= 0 || syms.is_null() {
            &[]
        } else {
            // SAFETY: see key_get_syms_by_level.
            unsafe { std::slice::from_raw_parts(syms, count as usize) }
        }
    }

    /// C: xkb_state_key_get_layout(state, key)
    pub fn key_get_layout(&self, key: xkb_keycode_t) -> xkb_layout_index_t {
        unsafe { xkb_state_key_get_layout(self.0, key) }
    }

    /// C: xkb_state_key_get_level(state, key, layout)
    pub fn key_get_level(
        &self,
        key: xkb_keycode_t,
        layout: xkb_layout_index_t,
    ) -> xkb_level_index_t {
        unsafe { xkb_state_key_get_level(self.0, key, layout) }
    }

    /// C: xkb_state_serialize_mods(state, XKB_STATE_MODS_EFFECTIVE)
    pub fn serialize_mods(&self, components: xkb_state_component) -> xkb_mod_mask_t {
        unsafe { xkb_state_serialize_mods(self.0, components) }
    }
}

impl Drop for XkbState {
    fn drop(&mut self) {
        // C: xkb_state_unref(state)
        unsafe { xkb_state_unref(self.0) };
    }
}
/******************************************************************************
 * keysyms (the xkbcommon-keysyms.h helpers)
 ******************************************************************************/

/// C: xkb_keysym_from_name(name, XKB_KEYSYM_NO_FLAGS)
///
/// Takes a Rust string, because C passes bind->symbol straight in; an embedded
/// NUL truncates the name exactly like the C string does.
pub fn keysym_from_name(name: &str) -> xkb_keysym_t {
    let bytes: Vec<u8> = name.bytes().take_while(|b| *b != 0).collect();
    let c_name = match CString::new(bytes) {
        Ok(s) => s,
        Err(_) => return XKB_KEY_NoSymbol,
    };
    unsafe { xkb_keysym_from_name(c_name.as_ptr(), XKB_KEYSYM_NO_FLAGS) }
}

/// C: xkb_keysym_get_name(keysym, str, sizeof(str)) — None on failure.
pub fn keysym_get_name(keysym: xkb_keysym_t) -> Option<String> {
    let mut buffer = [0 as c_char; 64];
    let len = unsafe { xkb_keysym_get_name(keysym, buffer.as_mut_ptr(), buffer.len()) };
    if len < 0 {
        return None;
    }
    let c_str = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(c_str.to_string_lossy().into_owned())
}

/// C: xkb_keysym_to_utf8(keysym, buffer, size) — None when the keysym has no
/// unicode representation.
pub fn keysym_to_utf8(keysym: xkb_keysym_t) -> Option<String> {
    let mut buffer = [0 as c_char; 8];
    let len = unsafe { xkb_keysym_to_utf8(keysym, buffer.as_mut_ptr(), buffer.len()) };
    if len <= 0 {
        return None;
    }
    let c_str = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(c_str.to_string_lossy().into_owned())
}

/// C: xkb_keysym_to_utf32(keysym)
pub fn keysym_to_utf32(keysym: xkb_keysym_t) -> u32 {
    unsafe { xkb_keysym_to_utf32(keysym) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // C: the keysym names the config parser hands to xkb_keysym_from_name
    // (src/config_directives.c bindsym).
    #[test]
    fn keysym_names_resolve_to_the_x11_values() {
        assert_eq!(keysym_from_name("a"), 0x0061);
        assert_eq!(keysym_from_name("A"), 0x0041);
        assert_eq!(keysym_from_name("Return"), 0xff0d);
        assert_eq!(keysym_from_name("space"), 0x0020);
        assert_eq!(keysym_from_name("XF86AudioRaiseVolume"), 0x1008ff13);
        assert_eq!(keysym_from_name("NoSuchKeysymName"), XKB_KEY_NoSymbol);
    }

    #[test]
    fn keysym_name_and_utf8_conversions() {
        assert_eq!(keysym_get_name(0xff0d).as_deref(), Some("Return"));
        assert_eq!(keysym_get_name(0x0061).as_deref(), Some("a"));
        assert_eq!(keysym_to_utf8(0x0061).as_deref(), Some("a"));
        assert_eq!(keysym_to_utf8(0x00e4).as_deref(), Some("ä"));
        assert_eq!(keysym_to_utf32(0x0061), 0x61);
        // xkbcommon spells the zero keysym out instead of failing, and it has
        // no unicode representation.
        assert_eq!(
            keysym_get_name(XKB_KEY_NoSymbol).as_deref(),
            Some("NoSymbol")
        );
        assert_eq!(keysym_to_utf8(XKB_KEY_NoSymbol), None);
    }

    /// Builds the "us" keymap from the local RMLVO data (C:
    /// xkb_keymap_new_from_names with all-NULL fields falls back to the
    /// defaults). No X server is involved.
    struct UsKeymap {
        context: XkbContext,
        keymap: XkbKeymap,
    }

    impl UsKeymap {
        fn new() -> UsKeymap {
            let context = XkbContext::new().expect("xkb_context_new");
            let layout = CString::new("us").unwrap();
            let names = xkb_rule_names {
                layout: layout.as_ptr(),
                ..Default::default()
            };
            let keymap = context.keymap_from_names(&names).expect("keymap");
            UsKeymap { context, keymap }
        }
    }

    #[test]
    fn keymap_resolves_keycodes_and_levels() {
        let us = UsKeymap::new();
        let _ = &us.context;

        // xkb_keymap_key_get_syms_by_level(keymap, key, layout, level, &syms)
        // for the evdev "us" layout: keycode 38 is "a", 36 is "Return".
        let syms = us.keymap.key_get_syms_by_level(38, 0, 0);
        assert_eq!(syms, &[0x0061]);
        let syms_shift = us.keymap.key_get_syms_by_level(38, 0, 1);
        assert_eq!(syms_shift, &[0x0041]);
        let ret = us.keymap.key_get_syms_by_level(36, 0, 0);
        assert_eq!(ret, &[0xff0d]);

        // A keycode the keymap does not define has no symbols (keycode 0 is
        // below the minimum of every keyboard map).
        assert!(us.keymap.key_get_syms_by_level(0, 0, 0).is_empty());
    }

    #[test]
    fn state_queries_follow_the_modifier_mask() {
        let us = UsKeymap::new();
        let mut state = XkbState::new(&us.keymap).expect("xkb_state_new");

        state.update_mask(0, 0, 0, 0, 0, 0);
        assert_eq!(state.key_get_one_sym(38), 0x0061);
        assert_eq!(state.key_get_level(38, 0), 0);

        // Shift depressed: level 1, uppercase symbol.
        state.update_mask(1, 0, 0, 0, 0, 0);
        assert_eq!(state.key_get_one_sym(38), 0x0041);
        assert_eq!(state.key_get_level(38, 0), 1);
        assert_eq!(state.serialize_mods(1), 1);

        // Mod1 (the i3 $mod) does not change the symbol of "a".
        state.update_mask(8, 0, 0, 0, 0, 0);
        assert_eq!(state.key_get_one_sym(38), 0x0061);
    }

    #[test]
    fn key_for_each_visits_the_keyboard() {
        let us = UsKeymap::new();
        let mut keycodes = Vec::new();
        us.keymap.key_for_each(&mut |key| keycodes.push(key));
        // A keyboard map defines a few hundred keycodes; 38 ("a") and 36
        // ("Return") have to be part of the iteration bindings.c walks.
        assert!(keycodes.len() > 100);
        assert!(keycodes.contains(&38));
        assert!(keycodes.contains(&36));
    }
}
