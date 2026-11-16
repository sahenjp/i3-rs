//! 1:1 port of src/bindings.c: Functions for configuring, finding and running
//! bindings.
//!
//! C: src/bindings.c (i3 4.25)
//!
//! The C globals of other translation units that this file reads/writes are
//! kept where C keeps them or are injected:
//!
//! * bindings (src/main.c:86) is Tree::bindings — the currently active list.
//!   C points that global at the active Mode's list; Rust keeps the active
//!   list in the Tree and copies the mode's list into it in switch_mode()
//!   (see Bindings).
//! * modes / current_binding_mode (src/config.c / src/bindings.c) are fields
//!   of Bindings.
//! * xkb_current_group (src/handlers.c:22), xkb_supported (src/main.c:103) and
//!   the xkbcommon keymap (src/bindings.c:16-18) are fields of Bindings as
//!   well.
//! * xcb_numlock_mask (src/xcb.c:12) stays in crate::xcb_util and is read
//!   through xcb_numlock_mask().
//! * command_error_nagbar_pid (src/bindings.c:19), current_configpath
//!   (src/config.c) and errorfilename (src/log.c:38) are fields of Bindings;
//!   the process-wide owners fill them.
//!
//! xkbcommon itself is not linked. The keymap/state API surface the C file
//! uses is expressed by the XkbKeymap trait, whose test double (FakeKeymap)
//! reproduces a real keymap closely enough to test the symbol-to-keycode
//! resolution.

// The C constant names are kept 1:1 (XKB_KEY_NoSymbol, …).
#![allow(non_upper_case_globals)]

use std::collections::VecDeque;

use slotmap::Key as _;

use crate::backend::{ConBackend, PropertyReply, XConn};
use crate::config::Mode;
use crate::data::{
    i3_event_state_mask_t, Binding, BindingRelease, Binding_Keycode, ConId, InputType,
};
use crate::ipc::{
    XCB_KEY_BUT_MASK_CONTROL, XCB_KEY_BUT_MASK_LOCK, XCB_KEY_BUT_MASK_MOD_1,
    XCB_KEY_BUT_MASK_MOD_2, XCB_KEY_BUT_MASK_MOD_3, XCB_KEY_BUT_MASK_MOD_4,
    XCB_KEY_BUT_MASK_MOD_5, XCB_KEY_BUT_MASK_SHIFT,
};
use crate::tree::{tree_render, Tree};
use libi3::log::{debuglog, errorlog};

/// C: src/bindings.c:25 const char *DEFAULT_BINDING_MODE = "default";
pub use crate::config_directives::DEFAULT_BINDING_MODE;

/******************************************************************************
 * Constants and globals
 *****************************************************************************/

/// C: xcb/xcb.h XCB_KEY_PRESS
pub const XCB_KEY_PRESS: u8 = 2;
/// C: xcb/xcb.h XCB_KEY_RELEASE
pub const XCB_KEY_RELEASE: u8 = 3;
/// C: xcb/xcb.h XCB_BUTTON_PRESS
pub const XCB_BUTTON_PRESS: u8 = 4;
/// C: xcb/xcb.h XCB_BUTTON_RELEASE
pub const XCB_BUTTON_RELEASE: u8 = 5;

/// C: xcb/xcb.h XCB_MOD_MASK_SHIFT
pub const XCB_MOD_MASK_SHIFT: u32 = 1 << 0;
/// C: xcb/xcb.h XCB_MOD_MASK_LOCK (CapsLock)
pub const XCB_MOD_MASK_LOCK: u32 = 1 << 1;
/// C: xcb/xcb.h XCB_GRAB_MODE_ASYNC
pub const XCB_GRAB_MODE_ASYNC: u8 = 1;
/// C: xcb/xcb.h XCB_GRAB_ANY
pub const XCB_GRAB_ANY: u8 = 0;
/// C: xcb/xcb.h XCB_BUTTON_INDEX_ANY
pub const XCB_BUTTON_INDEX_ANY: u8 = 0;
/// C: xcb/xcb.h XCB_BUTTON_MASK_ANY
pub const XCB_BUTTON_MASK_ANY: u16 = 0xffff;
/// C: xcb/xcb.h XCB_MAPPING_KEYBOARD
pub const XCB_MAPPING_KEYBOARD: u8 = 0;
/// C: xcb/xcb.h XCB_MAPPING_MODIFIER
pub const XCB_MAPPING_MODIFIER: u8 = 1;

/// C: xcb/xkb.h XCB_XKB_GROUP_1
pub const XCB_XKB_GROUP_1: u32 = 0;
/// C: xcb/xkb.h XCB_XKB_GROUP_2
pub const XCB_XKB_GROUP_2: u32 = 1;
/// C: xcb/xkb.h XCB_XKB_GROUP_3
pub const XCB_XKB_GROUP_3: u32 = 2;
/// C: xcb/xkb.h XCB_XKB_GROUP_4
pub const XCB_XKB_GROUP_4: u32 = 3;

/// C: xkbcommon-keysyms.h XKB_KEY_NoSymbol
pub const XKB_KEY_NoSymbol: u32 = 0;
/// C: xkbcommon-keysyms.h XKB_KEY_KP_Space
pub const XKB_KEY_KP_Space: u32 = 0xff80;
/// C: xkbcommon-keysyms.h XKB_KEY_KP_Equal
pub const XKB_KEY_KP_Equal: u32 = 0xffbd;
/// C: xkbcommon.h XKB_LAYOUT_INVALID
pub const XKB_LAYOUT_INVALID: u32 = u32::MAX;

/// C: src/xcb.c:12 unsigned int xcb_numlock_mask — read accessor for the
/// global owned by crate::xcb_util.
pub fn xcb_numlock_mask() -> u32 {
    // SAFETY: the global is only ever assigned as a whole word by the
    // modifier-mapping update (src/handlers.c:272); reading it is a plain
    // load, exactly like C.
    unsafe { crate::xcb_util::xcb_numlock_mask }
}

/// C: src/handlers.c:272 xcb_numlock_mask = aio_get_mod_mask_for(...) — the
/// modifier-mapping update writes the global.
pub fn set_xcb_numlock_mask(mask: u32) {
    // SAFETY: see xcb_numlock_mask().
    unsafe {
        crate::xcb_util::xcb_numlock_mask = mask;
    }
}

/******************************************************************************
 * xkbcommon abstraction
 *****************************************************************************/

/// The struct xkb_state the resolver builds for one binding.
///
/// C: src/bindings.c:366-375 (the struct xkb_state * members of struct
/// resolve); in C each state is an opaque object, here it is the value the
/// xkbcommon state functions read (the xkb_state_update_mask arguments).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XkbState {
    /// C: base_mods passed to xkb_state_update_mask.
    pub base_mods: u32,
    /// C: locked_group passed to xkb_state_update_mask.
    pub group: u32,
}

/// The xkbcommon keymap/state API surface used by src/bindings.c.
///
/// The real implementation wraps libxkbcommon (via xkbcommon-x11); i3-core
/// does not link it yet, so the resolution is driven through this trait
/// exactly like the C code drives xkbcommon.
pub trait XkbKeymap {
    /// C: xkb_keymap_key_for_each(xkb_keymap, add_keycode_if_matches, ...)
    fn keymap_key_for_each(&self, f: &mut dyn FnMut(u8));

    /// C: xkb_state_key_get_one_sym(state, key)
    fn state_key_get_one_sym(&self, state: &XkbState, key: u8) -> u32;

    /// C: xkb_state_key_get_layout(state, key)
    fn state_key_get_layout(&self, state: &XkbState, key: u8) -> u32;

    /// C: xkb_state_key_get_level(state, key, layout)
    fn state_key_get_level(&self, state: &XkbState, key: u8, layout: u32) -> u32;

    /// C: xkb_keysym_from_name(name, XKB_KEYSYM_NO_FLAGS)
    fn keysym_from_name(&self, name: &str) -> u32;

    /// C: xkb_keymap_key_get_syms_by_level(keymap, key, layout, level, &syms)
    ///
    /// The C code reaches the raw keymap through this call in
    /// libi3/get_mod_mask.c (xcb_key_symbols_get_keycode) only; bindings.c
    /// itself goes through the state queries above.
    fn keymap_key_get_syms_by_level(&self, key: u8, layout: u32, level: u32) -> Vec<u32>;
}

/// The struct xkb_rule_names filled by fill_rmlvo_from_root().
///
/// C: src/bindings.c:999
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XkbRuleNames {
    pub rules: Option<String>,
    pub model: Option<String>,
    pub layout: Option<String>,
    pub variant: Option<String>,
    pub options: Option<String>,
}

/// Creation of the xkbcommon keymap (C: the xkb_x11_* / xkb_keymap_new_*
/// calls of load_keymap()).
pub trait XkbKeymapFactory {
    /// C: xkb_x11_get_core_keyboard_device_id(conn)
    fn core_keyboard_device_id(&mut self, x: &mut dyn XConn) -> i32 {
        let _ = x;
        -1
    }

    /// C: xkb_x11_keymap_new_from_device(context, conn, device_id, 0)
    fn keymap_new_from_device(
        &mut self,
        x: &mut dyn XConn,
        device_id: i32,
    ) -> Option<Box<dyn XkbKeymap>> {
        let _ = (x, device_id);
        None
    }

    /// C: xkb_keymap_new_from_names(context, &names, 0)
    fn keymap_new_from_names(&mut self, names: &XkbRuleNames) -> Option<Box<dyn XkbKeymap>> {
        let _ = names;
        None
    }
}

/// C: src/bindings.c:358 struct resolve
struct Resolve<'a> {
    /// The binding which we are resolving.
    bind: &'a mut Binding,
    /// bind's keysym (translated to xkb_keysym_t), e.g. XKB_KEY_R.
    keysym: u32,
    /// The xkb state built from the user-provided modifiers and group.
    xkb_state: XkbState,
    /// Like xkb_state, just without the shift modifier, if shift was specified.
    xkb_state_no_shift: XkbState,
    /// Like xkb_state, but with NumLock.
    xkb_state_numlock: XkbState,
    /// Like xkb_state, but with NumLock, just without the shift modifier, if
    /// shift was specified.
    xkb_state_numlock_no_shift: XkbState,
}

/// C: src/bindings.c:378 ADD_TRANSLATED_KEY(code, mods)
fn add_translated_key(bind: &mut Binding, code: u8, mods: u32) {
    bind.keycodes_head.push_back(Binding_Keycode {
        keycode: code,
        modifiers: mods,
    });
}

/// C: src/bindings.c:392 add_keycode_if_matches — called for each keycode in
/// the keymap; adds the keycode to resolving.bind if it can result in the
/// keysym resolving.keysym.
fn add_keycode_if_matches(
    keymap: &dyn XkbKeymap,
    numlock_mask: u32,
    key: u8,
    resolving: &mut Resolve<'_>,
) {
    let mut numlock_state = resolving.xkb_state_numlock;
    let mut sym = keymap.state_key_get_one_sym(&resolving.xkb_state, key);
    if sym != resolving.keysym {
        /* Check if Shift was specified, and try resolving the symbol without
         * shift, so that "bindsym $mod+Shift+a nop" actually works. */
        let layout = keymap.state_key_get_layout(&resolving.xkb_state, key);
        if layout == XKB_LAYOUT_INVALID {
            return;
        }
        if keymap.state_key_get_level(&resolving.xkb_state, key, layout) > 1 {
            return;
        }
        /* Skip the Shift fallback for keypad keys, otherwise one cannot bind
         * KP_1 independent of KP_End. */
        if sym >= XKB_KEY_KP_Space && sym <= XKB_KEY_KP_Equal {
            return;
        }
        numlock_state = resolving.xkb_state_numlock_no_shift;
        sym = keymap.state_key_get_one_sym(&resolving.xkb_state_no_shift, key);
        if sym != resolving.keysym {
            return;
        }
    }
    let bind = &mut *resolving.bind;

    add_translated_key(bind, key, bind.event_state_mask);

    /* Also bind the key with active CapsLock */
    add_translated_key(bind, key, bind.event_state_mask | XCB_KEY_BUT_MASK_LOCK);

    /* If this binding is not explicitly for NumLock, check whether we need to
     * add a fallback. */
    if (bind.event_state_mask & numlock_mask) != numlock_mask {
        /* Check whether the keycode results in the same keysym when NumLock is
         * active. If so, grab the key with NumLock as well, so that users do
         * not need to duplicate every key binding with an additional Mod2
         * specified. */
        let sym_numlock = keymap.state_key_get_one_sym(&numlock_state, key);
        if sym_numlock == resolving.keysym {
            /* Also bind the key with active NumLock */
            add_translated_key(bind, key, bind.event_state_mask | numlock_mask);

            /* Also bind the key with active NumLock+CapsLock */
            add_translated_key(
                bind,
                key,
                bind.event_state_mask | numlock_mask | XCB_KEY_BUT_MASK_LOCK,
            );
        } else {
            debuglog(&format!(
                "Skipping automatic numlock fallback, key {} resolves to 0x{:x} with numlock\n",
                key, sym_numlock
            ));
        }
    }
}

/******************************************************************************
 * The C globals of src/bindings.c plus the mode list
 *****************************************************************************/

/// The state of src/bindings.c: the C globals of the module plus the mode
/// list of src/config.c.
///
/// Aliasing note: C keeps bindings (src/main.c:86) as a pointer to the active
/// Mode's TAILQ head, so get_binding()'s release-state mutations are visible
/// in the mode. Rust keeps the active list in Tree::bindings and copies the
/// mode's list in switch_mode(); every lookup/mutation therefore operates on
/// the Tree, and the mode's list is (re)translated on each switch_mode, which
/// is what the C code does as well.
pub struct Bindings {
    /// C: src/config.c struct modes_head modes (SLIST with insert-head, hence
    /// push_front like ConfigDirectives::modes).
    pub modes: VecDeque<Mode>,
    /// C: src/bindings.c char *current_binding_mode
    pub current_binding_mode: Option<String>,
    /// C: src/handlers.c:22 int xkb_current_group
    pub xkb_current_group: u32,
    /// C: src/main.c:103 bool xkb_supported = true
    pub xkb_supported: bool,
    /// C: src/bindings.c:18 static struct xkb_keymap *xkb_keymap
    pub keymap: Option<Box<dyn XkbKeymap>>,
    /// C: src/bindings.c:16 static struct xkb_context *xkb_context — the
    /// context only needs to exist once.
    pub xkb_context_created: bool,
    /// C: src/bindings.c:19 pid_t command_error_nagbar_pid = -1
    pub command_error_nagbar_pid: i32,
    /// C: src/config.c char *current_configpath (read by translate_keysyms)
    pub current_configpath: Option<String>,
    /// C: src/log.c:38 char *errorfilename (read by run_binding)
    pub errorfilename: Option<String>,
}

impl Default for Bindings {
    fn default() -> Self {
        Bindings::new()
    }
}

impl Bindings {
    /// A fresh state, like the C zero-initialized globals (plus the
    /// main.c:103 initializer of xkb_supported and the -1 initializer of
    /// command_error_nagbar_pid).
    pub fn new() -> Bindings {
        Bindings {
            modes: VecDeque::new(),
            current_binding_mode: None,
            xkb_current_group: XCB_XKB_GROUP_1,
            xkb_supported: true,
            keymap: None,
            xkb_context_created: false,
            command_error_nagbar_pid: -1,
            current_configpath: None,
            errorfilename: None,
        }
    }

    /// C: src/config.c:170-179 (the default mode is created during config
    /// initialization). Used by the config layer and the tests.
    pub fn init_default_mode(&mut self) {
        self.mode_from_name(DEFAULT_BINDING_MODE, false);
    }

    /// C: src/bindings.c:32 mode_from_name — returns the mode specified by
    /// name or creates a new mode and adds it to the list of modes.
    ///
    /// C returns a struct Mode *; Rust returns the index into modes (a
    /// VecDeque cannot hand out stable references across pushes).
    fn mode_from_name(&mut self, name: &str, pango_markup: bool) -> usize {
        if let Some(pos) = self.modes.iter().position(|m| m.name == name) {
            return pos;
        }
        self.modes.push_front(Mode {
            name: name.to_string(),
            pango_markup,
            bindings: VecDeque::new(),
        });
        0
    }

    /// The index of a mode, like the struct Mode * of mode_from_name().
    fn mode_index(&self, name: &str) -> Option<usize> {
        self.modes.iter().position(|m| m.name == name)
    }

    /// C: src/config.c:170-179 (init_configuration) + config.c:276-277
    /// (`extract_workspace_names_from_bindings(); reorder_bindings();`) —
    /// after load_configuration() the parsed modes are the global mode list,
    /// the global `bindings` pointer points at the default mode's list, and the
    /// bindings are extracted/reordered before load_keymap() and
    /// translate_keysyms() run (C: main.c:912-921).
    ///
    /// Tree::bindings is the port's stand-in for the C `bindings` pointer, so
    /// the default mode's list is copied into it here.
    pub fn load_config_bindings(
        &mut self,
        t: &mut Tree,
        modes: &VecDeque<Mode>,
        current_configpath: Option<String>,
    ) {
        self.modes = modes.clone();
        self.current_binding_mode = Some(DEFAULT_BINDING_MODE.to_string());
        self.current_configpath = current_configpath;
        let index = match self.mode_index(DEFAULT_BINDING_MODE) {
            Some(index) => index,
            None => {
                let index = self.mode_from_name(DEFAULT_BINDING_MODE, false);
                index
            }
        };
        t.bindings = self.modes[index].bindings.clone();
        crate::workspace::extract_workspace_names_from_bindings(t);
        reorder_bindings(self, t);
    }
}

/******************************************************************************
 * Side effects outside the module
 *****************************************************************************/

/// The side effects of src/bindings.c that live in other translation units.
pub trait BindingHooks {
    /// C: src/bindings.c:665-669 — builds {"change":"<mode>",
    /// "pango_markup":<bool>} and calls ipc_send_event("mode",
    /// I3_IPC_EVENT_MODE, event_msg).
    fn ipc_send_mode_event(&mut self, mode: &Mode) {
        let _ = mode;
    }

    /// C: src/bindings.c:902 ipc_send_binding_event("run", bind_cp, modename).
    fn ipc_send_binding_event(&mut self, bind: &Binding, modename: Option<&str>) {
        let _ = (bind, modename);
    }

    /// C: src/bindings.c:629 start_config_error_nagbar(current_configpath, true).
    fn start_config_error_nagbar(&mut self, current_configpath: Option<&str>, pango_markup: bool) {
        let _ = (current_configpath, pango_markup);
    }

    /// C: src/bindings.c:883-899 — the i3-nagbar invocation with the
    /// i3-sensible-pager "<errorfilename>" action (the C argv also carries
    /// config.font.pattern and the two message strings).
    fn start_command_error_nagbar(&mut self, pager_action: &str) {
        let _ = pager_action;
    }

    /// C: result->parse_error (src/bindings.c:883). The command parser behind
    /// ConBackend::parse_command only reports needs_tree_render, so the error
    /// flag is surfaced through the hook.
    fn command_parse_error(&mut self) -> bool {
        false
    }
}

/// No-op hooks (C: the absence of an IPC client / nagbar).
#[derive(Debug, Clone, Copy, Default)]
pub struct NullHooks;

impl BindingHooks for NullHooks {}

/******************************************************************************
 * configure_binding
 *****************************************************************************/

/// C: src/config_directives.c:127 event_state_from_str — converts a string
/// containing the group and modifiers to the corresponding bit mask. The
/// simpler strstr()-based approach of the C code is kept.
fn event_state_from_str(s: Option<&str>) -> i3_event_state_mask_t {
    let s = match s {
        Some(s) => s,
        None => return 0,
    };
    let mut result = 0u32;
    if s.contains("Mod1") {
        result |= XCB_KEY_BUT_MASK_MOD_1;
    }
    if s.contains("Mod2") {
        result |= XCB_KEY_BUT_MASK_MOD_2;
    }
    if s.contains("Mod3") {
        result |= XCB_KEY_BUT_MASK_MOD_3;
    }
    if s.contains("Mod4") {
        result |= XCB_KEY_BUT_MASK_MOD_4;
    }
    if s.contains("Mod5") {
        result |= XCB_KEY_BUT_MASK_MOD_5;
    }
    if s.contains("Control") || s.contains("Ctrl") {
        result |= XCB_KEY_BUT_MASK_CONTROL;
    }
    if s.contains("Shift") {
        result |= XCB_KEY_BUT_MASK_SHIFT;
    }
    if s.contains("Group1") {
        result |= crate::data::I3_XKB_GROUP_MASK_1.0 << 16;
    }
    if s.contains("Group2") || s.contains("Mode_switch") {
        result |= crate::data::I3_XKB_GROUP_MASK_2.0 << 16;
    }
    if s.contains("Group3") {
        result |= crate::data::I3_XKB_GROUP_MASK_3.0 << 16;
    }
    if s.contains("Group4") {
        result |= crate::data::I3_XKB_GROUP_MASK_4.0 << 16;
    }
    result
}

/// C: strncasecmp(s, prefix, strlen(prefix)) == 0.
fn strncasecmp_prefix(s: &str, prefix: &str) -> bool {
    s.as_bytes().len() >= prefix.len()
        && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// C: src/bindings.c:59 configure_binding — adds a binding from config
/// parameters given as strings and returns the binding structure. Returns
/// None if the input code could not be parsed.
///
/// C returns a pointer to the object it inserted into the mode; Rust returns a
/// copy (the list owns the binding).
#[allow(clippy::too_many_arguments)]
pub fn configure_binding(
    b: &mut Bindings,
    bindtype: &str,
    modifiers: Option<&str>,
    input_code: &str,
    release: Option<&str>,
    border: Option<&str>,
    whole_window: Option<&str>,
    exclude_titlebar: Option<&str>,
    command: &str,
    modename: &str,
    pango_markup: bool,
) -> Option<Binding> {
    let mut new_binding = Binding::default();
    debuglog(&format!(
        "Binding bindtype {}, modifiers {:?}, input code {}, release {:?}\n",
        bindtype, modifiers, input_code, release
    ));
    new_binding.release = if release.is_some() {
        BindingRelease::B_UPON_KEYRELEASE
    } else {
        BindingRelease::B_UPON_KEYPRESS
    };
    new_binding.border = border.is_some();
    new_binding.whole_window = whole_window.is_some();
    new_binding.exclude_titlebar = exclude_titlebar.is_some();
    if bindtype == "bindsym" {
        new_binding.input_type = if strncasecmp_prefix(input_code, "button") {
            InputType::B_MOUSE
        } else {
            InputType::B_KEYBOARD
        };
        new_binding.symbol = Some(input_code.to_string());
    } else {
        let keycode = match crate::commands::parse_long(input_code, 10) {
            Some(keycode) => keycode,
            None => {
                errorlog(&format!(
                    "Could not parse \"{}\" as an input code, ignoring this binding.\n",
                    input_code
                ));
                return None;
            }
        };
        new_binding.keycode = keycode as u32;
        new_binding.input_type = InputType::B_KEYBOARD;
    }
    new_binding.command = command.to_string();
    new_binding.event_state_mask = event_state_from_str(modifiers);
    let mut group_bits_set = 0;
    for mask in [
        crate::data::I3_XKB_GROUP_MASK_1.0,
        crate::data::I3_XKB_GROUP_MASK_2.0,
        crate::data::I3_XKB_GROUP_MASK_3.0,
        crate::data::I3_XKB_GROUP_MASK_4.0,
    ] {
        if (new_binding.event_state_mask >> 16) & mask != 0 {
            group_bits_set += 1;
        }
    }
    if group_bits_set > 1 {
        errorlog(
            "Keybinding has more than one Group specified, but your X server is always in \
             precisely one group. The keybinding can never trigger.\n",
        );
    }

    let mode_index = b.mode_from_name(modename, pango_markup);
    b.modes[mode_index].bindings.push_back(new_binding.clone());

    /* C: TAILQ_INIT(&(new_binding->keycodes_head)); — empty by default. */
    Some(new_binding)
}

/******************************************************************************
 * Grabbing
 *****************************************************************************/

/// C: src/bindings.c:113 binding_in_current_group
fn binding_in_current_group(b: &Bindings, bind: &Binding) -> bool {
    /* If no bits are set, the binding should be installed in every group. */
    if (bind.event_state_mask >> 16) == crate::data::I3_XKB_GROUP_MASK_ANY.0 {
        return true;
    }
    match b.xkb_current_group {
        XCB_XKB_GROUP_1 => ((bind.event_state_mask >> 16) & crate::data::I3_XKB_GROUP_MASK_1.0) != 0,
        XCB_XKB_GROUP_2 => ((bind.event_state_mask >> 16) & crate::data::I3_XKB_GROUP_MASK_2.0) != 0,
        XCB_XKB_GROUP_3 => ((bind.event_state_mask >> 16) & crate::data::I3_XKB_GROUP_MASK_3.0) != 0,
        XCB_XKB_GROUP_4 => ((bind.event_state_mask >> 16) & crate::data::I3_XKB_GROUP_MASK_4.0) != 0,
        _ => {
            errorlog(&format!(
                "BUG: xkb_current_group (= {}) outside of [XCB_XKB_GROUP_1..XCB_XKB_GROUP_4]\n",
                b.xkb_current_group
            ));
            false
        }
    }
}

/// C: src/bindings.c:133 grab_keycode_for_binding — grabs the key in all
/// NumLock/CapsLock combinations.
fn grab_keycode_for_binding(x: &mut dyn XConn, bind: &Binding, keycode: u8, root: u32) {
    let mods = (bind.event_state_mask & 0xFFFF) as u16;
    let numlock = xcb_numlock_mask() as u16;
    let lock = XCB_KEY_BUT_MASK_LOCK as u16;
    debuglog(&format!(
        "Binding Grabbing keycode {} with event state mask 0x{:x} (mods 0x{:x})\n",
        keycode, bind.event_state_mask, mods
    ));
    x.grab_key(false, root, mods, keycode, XCB_GRAB_MODE_ASYNC, XCB_GRAB_MODE_ASYNC);
    /* Also bind the key with active NumLock */
    x.grab_key(false, root, mods | numlock, keycode, XCB_GRAB_MODE_ASYNC, XCB_GRAB_MODE_ASYNC);
    /* Also bind the key with active CapsLock */
    x.grab_key(false, root, mods | lock, keycode, XCB_GRAB_MODE_ASYNC, XCB_GRAB_MODE_ASYNC);
    /* Also bind the key with active NumLock+CapsLock */
    x.grab_key(
        false,
        root,
        mods | numlock | lock,
        keycode,
        XCB_GRAB_MODE_ASYNC,
        XCB_GRAB_MODE_ASYNC,
    );
}

/// C: src/bindings.c:155 grab_all_keys — grabs the bound keys (tells X to
/// send us keypress events for those keycodes).
pub fn grab_all_keys(b: &Bindings, t: &Tree, x: &mut dyn XConn) {
    let root = x.root_screen().root;
    for bind in &t.bindings {
        if bind.input_type != InputType::B_KEYBOARD {
            continue;
        }

        if !binding_in_current_group(b, bind) {
            continue;
        }

        /* The easy case: the user specified a keycode directly. */
        if bind.keycode > 0 {
            grab_keycode_for_binding(x, bind, bind.keycode as u8, root);
            continue;
        }

        for binding_keycode in &bind.keycodes_head {
            let keycode = binding_keycode.keycode;
            let mods = (binding_keycode.modifiers & 0xFFFF) as u16;
            debuglog(&format!(
                "Binding Grabbing keycode {} with mods {}\n",
                keycode, mods
            ));
            x.grab_key(false, root, mods, keycode, XCB_GRAB_MODE_ASYNC, XCB_GRAB_MODE_ASYNC);
        }
    }
}

/// C: src/config.c:27 ungrab_all_keys (the function lives in config.c but
/// belongs to the bindings flow and is ported here).
pub fn ungrab_all_keys(x: &mut dyn XConn) {
    debuglog("Ungrabbing all keys\n");
    let root = x.root_screen().root;
    x.ungrab_key(XCB_GRAB_ANY, root, XCB_BUTTON_MASK_ANY);
}

/// C: src/bindings.c:1033 bindings_get_buttons_to_grab — returns the list of
/// buttons that should be grabbed on a window. Always contains 1-3; all
/// higher buttons only if there is a whole-window binding for them.
///
/// C returns a 0-terminated int *; Rust returns the values (no terminator).
pub fn bindings_get_buttons_to_grab(t: &Tree) -> Vec<u32> {
    /* Let's make the reasonable assumption that there's no more than 25
     * buttons. */
    const NUM_MAX: usize = 25;

    let mut buffer: Vec<u32> = Vec::with_capacity(NUM_MAX);

    /* We always return buttons 1 through 3. */
    buffer.push(1);
    buffer.push(2);
    buffer.push(3);

    for bind in &t.bindings {
        if buffer.len() + 1 == NUM_MAX {
            break;
        }

        /* We are only interested in whole window mouse bindings. */
        if bind.input_type != InputType::B_MOUSE || !bind.whole_window {
            continue;
        }

        let symbol = bind.symbol.clone().unwrap_or_default();
        let rest = if symbol.len() >= "button".len() {
            &symbol["button".len()..]
        } else {
            ""
        };
        let button = match crate::commands::parse_long(rest, 10) {
            Some(button) => button,
            None => {
                errorlog(
                    "Could not parse button number, skipping this binding. Please report this bug in i3.\n",
                );
                continue;
            }
        };

        /* Avoid duplicates. */
        if !buffer.contains(&(button as u32)) {
            buffer.push(button as u32);
        }
    }

    buffer
}

/// C: src/bindings.c:187 regrab_all_buttons — release the button grabs on all
/// managed windows and regrab them, reevaluating which buttons need to be
/// grabbed.
pub fn regrab_all_buttons(t: &Tree, x: &mut dyn XConn) {
    let buttons = bindings_get_buttons_to_grab(t);
    x.grab_server();

    for con_id in &t.all_cons {
        let window = match t.cons[*con_id].window {
            Some(window) => window,
            None => continue,
        };
        let window_id = t.windows[window].id;

        x.ungrab_button(XCB_BUTTON_INDEX_ANY, window_id, XCB_BUTTON_MASK_ANY);
        x.grab_buttons(window_id, &buttons);
    }

    x.ungrab_server();
}

/******************************************************************************
 * Binding lookup
 *****************************************************************************/

/// C: src/bindings.c:244-276 — the inner Binding_Keycode loop of get_binding().
///
/// input_keycode is Some for the "the user specified a symbol" branch (which
/// additionally compares the keycode) and None for the "keycode was specified
/// directly" branch.
pub fn binding_matches_keycodes(
    bind: &Binding,
    input_keycode: Option<u8>,
    modifiers_state: u32,
    is_release: bool,
) -> bool {
    for binding_keycode in &bind.keycodes_head {
        let modifiers_mask = binding_keycode.modifiers & 0x0000FFFF;
        let mods_match = modifiers_mask == modifiers_state;
        debuglog(&format!(
            "binding_keycode->modifiers = {}, modifiers_mask = {}, modifiers_state = {}, mods_match = {}\n",
            binding_keycode.modifiers,
            modifiers_mask,
            modifiers_state,
            if mods_match { "yes" } else { "no" }
        ));
        let keycode_matches = match input_keycode {
            Some(input_keycode) => binding_keycode.keycode == input_keycode,
            None => true,
        };
        if keycode_matches
            && (mods_match
                || (bind.release == BindingRelease::B_UPON_KEYRELEASE_IGNORE_MODS && is_release))
        {
            return true;
        }
    }
    false
}

/// C: src/bindings.c:210 get_binding — returns the index of the Binding with
/// the specified modifiers and keycode, or None if no such binding exists.
///
/// C returns a Binding *; the index is returned so that callers can keep
/// mutating the Tree (the release state transitions of this function modify
/// the list in place).
pub fn get_binding(
    t: &mut Tree,
    state_filtered: i3_event_state_mask_t,
    is_release: bool,
    input_code: u16,
    input_type: InputType,
) -> Option<usize> {
    if !is_release {
        /* On a press event, we first reset all B_UPON_KEYRELEASE_IGNORE_MODS
         * bindings back to B_UPON_KEYRELEASE */
        for bind in &mut t.bindings {
            if bind.input_type != input_type {
                continue;
            }
            if bind.release == BindingRelease::B_UPON_KEYRELEASE_IGNORE_MODS {
                bind.release = BindingRelease::B_UPON_KEYRELEASE;
            }
        }
    }

    let xkb_group_state = state_filtered & 0xFFFF0000;
    let modifiers_state = state_filtered & 0x0000FFFF;
    let mut result: Option<usize> = None;
    for (index, bind) in t.bindings.iter_mut().enumerate() {
        if bind.input_type != input_type {
            continue;
        }

        let xkb_group_mask = bind.event_state_mask & 0xFFFF0000;
        let groups_match = (xkb_group_state & xkb_group_mask) == xkb_group_mask;
        if !groups_match {
            debuglog("skipping binding because XKB groups do not match\n");
            continue;
        }

        /* For keyboard bindings where a symbol was specified by the user, we
         * need to look in the array of translated keycodes for the event's
         * keycode */
        let found_keycode = if input_type == InputType::B_KEYBOARD && bind.symbol.is_some() {
            binding_matches_keycodes(bind, Some(input_code as u8), modifiers_state, is_release)
        } else {
            /* This case is easier: The user specified a keycode */
            if bind.keycode != input_code as u32 {
                continue;
            }
            binding_matches_keycodes(bind, None, modifiers_state, is_release)
        };
        if !found_keycode {
            continue;
        }

        /* If this binding is a release binding, it matches the key which the
         * user pressed. We therefore mark it as B_UPON_KEYRELEASE_IGNORE_MODS
         * for later, so that the user can release the modifiers before the
         * actual key or button and the release event will still be matched. */
        if bind.release == BindingRelease::B_UPON_KEYRELEASE && !is_release {
            bind.release = BindingRelease::B_UPON_KEYRELEASE_IGNORE_MODS;
            debuglog("marked bind as B_UPON_KEYRELEASE_IGNORE_MODS\n");
            if result.is_some() {
                break;
            }
            continue;
        }

        /* Check if the binding is for a press or a release event */
        if bind.release == BindingRelease::B_UPON_KEYPRESS && is_release {
            continue;
        }

        if is_release {
            return Some(index);
        } else if result.is_none() {
            /* Continue looping to mark needed B_UPON_KEYRELEASE_IGNORE_MODS. */
            result = Some(index);
        }
    }

    result
}

/// Task-name alias of get_binding() (C: src/bindings.c:210 get_binding).
pub fn check_for_binding(
    t: &mut Tree,
    state_filtered: i3_event_state_mask_t,
    is_release: bool,
    input_code: u16,
    input_type: InputType,
) -> Option<usize> {
    get_binding(t, state_filtered, is_release, input_code, input_type)
}

/// The members of xcb_generic_event_t / xcb_key_press_event_t that
/// get_binding_from_xcb_event() reads.
///
/// C: src/bindings.c:315 xcb_generic_event_t *event
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingEvent {
    /// C: event->response_type
    pub response_type: u8,
    /// C: ((xcb_key_press_event_t *)event)->state
    pub state: u16,
    /// C: ((xcb_key_press_event_t *)event)->detail
    pub detail: u16,
}

/// C: src/bindings.c:315 get_binding_from_xcb_event — returns the binding that
/// matches the given xcb button or key event (as an index), or None.
pub fn get_binding_from_xcb_event(t: &mut Tree, event: &BindingEvent) -> Option<usize> {
    let is_release =
        event.response_type == XCB_KEY_RELEASE || event.response_type == XCB_BUTTON_RELEASE;

    let input_type = if event.response_type == XCB_BUTTON_RELEASE
        || event.response_type == XCB_BUTTON_PRESS
    {
        InputType::B_MOUSE
    } else {
        InputType::B_KEYBOARD
    };

    let event_state = event.state;

    /* Remove the CapsLock bit */
    let mut state_filtered = (event_state as u32) & !XCB_MOD_MASK_LOCK;
    debuglog(&format!("(removed capslock, state = 0x{:x})\n", state_filtered));
    /* Transform the keyboard_group from bit 13 and bit 14 into an
     * i3_xkb_group_mask_t, so that get_binding() can just bitwise AND the
     * configured bindings against state_filtered.
     *
     * These bits are only set because we set the XKB client flags
     * XCB_XKB_PER_CLIENT_FLAG_GRABS_USE_XKB_STATE and
     * XCB_XKB_PER_CLIENT_FLAG_LOOKUP_STATE_WHEN_GRABBED. See also doc/kbproto
     * section 2.2.2:
     * https://www.x.org/releases/X11R7.7/doc/kbproto/xkbproto.html#Computing_A_State_Field_from_an_XKB_State */
    match ((event_state & 0x6000) >> 13) as u32 {
        XCB_XKB_GROUP_1 => state_filtered |= crate::data::I3_XKB_GROUP_MASK_1.0 << 16,
        XCB_XKB_GROUP_2 => state_filtered |= crate::data::I3_XKB_GROUP_MASK_2.0 << 16,
        XCB_XKB_GROUP_3 => state_filtered |= crate::data::I3_XKB_GROUP_MASK_3.0 << 16,
        XCB_XKB_GROUP_4 => state_filtered |= crate::data::I3_XKB_GROUP_MASK_4.0 << 16,
        _ => {}
    }
    state_filtered &= !0x6000;
    debuglog(&format!(
        "(transformed keyboard group, state = 0x{:x})\n",
        state_filtered
    ));
    get_binding(t, state_filtered, is_release, event.detail, input_type)
}

/******************************************************************************
 * Keymap
 *****************************************************************************/

/// C: src/bindings.c:909 fill_rmlvo_from_root — reads _XKB_RULES_NAMES from
/// the root window. Returns 0 on success, -1 otherwise.
pub fn fill_rmlvo_from_root(x: &mut dyn XConn, xkb_names: &mut XkbRuleNames) -> i32 {
    let content_max_words: u32 = 256;

    let atom = match x.intern_atom("_XKB_RULES_NAMES") {
        Some(atom) => atom,
        None => return -1,
    };
    let root = x.root_screen().root;
    /* C re-requests the property with a size adjusted by bytes_after when the
     * value was truncated; PropertyReply (backend.rs) always carries the
     * complete value, so the retry is not needed here. */
    let prop_reply: PropertyReply = match x.get_property(
        root,
        atom,
        0, /* XCB_GET_PROPERTY_TYPE_ANY */
        0,
        content_max_words,
    ) {
        Some(reply) => reply,
        None => return -1,
    };
    if prop_reply.value_len() == 0 {
        return -1;
    }

    let mut walk = &prop_reply.value[..];
    for i in 0..5 {
        if walk.is_empty() {
            break;
        }
        let len = walk.iter().position(|&b| b == 0).unwrap_or(walk.len());
        let component = String::from_utf8_lossy(&walk[..len]).into_owned();
        debuglog(&format!(
            "component {} of _XKB_RULES_NAMES is \"{}\"\n",
            i, component
        ));
        match i {
            0 => xkb_names.rules = Some(component),
            1 => xkb_names.model = Some(component),
            2 => xkb_names.layout = Some(component),
            3 => xkb_names.variant = Some(component),
            4 => xkb_names.options = Some(component),
            _ => {}
        }
        let skip = (len + 1).min(walk.len());
        walk = &walk[skip..];
    }

    0
}

/// C: src/bindings.c:980 load_keymap — loads the XKB keymap from the X11
/// server and feeds it to xkbcommon.
pub fn load_keymap(
    b: &mut Bindings,
    x: &mut dyn XConn,
    factory: &mut dyn XkbKeymapFactory,
) -> bool {
    if !b.xkb_context_created {
        /* C: xkb_context_new(0) */
        b.xkb_context_created = true;
    }

    let new_keymap: Box<dyn XkbKeymap>;
    let device_id = if b.xkb_supported {
        factory.core_keyboard_device_id(x)
    } else {
        -1
    };
    if device_id > -1 {
        match factory.keymap_new_from_device(x, device_id) {
            Some(keymap) => new_keymap = keymap,
            None => {
                errorlog("xkb_x11_keymap_new_from_device failed\n");
                return false;
            }
        }
    } else {
        /* Likely there is no XKB support on this server, possibly because it
         * is a VNC server. */
        debuglog("No XKB / core keyboard device? Assembling keymap from local RMLVO.\n");
        let mut names = XkbRuleNames::default();
        if fill_rmlvo_from_root(x, &mut names) == -1 {
            errorlog(
                "Could not get _XKB_RULES_NAMES atom from root window, falling back to defaults.\n",
            );
            /* Using NULL for the fields of xkb_rule_names. */
            names = XkbRuleNames::default();
        }
        match factory.keymap_new_from_names(&names) {
            Some(keymap) => new_keymap = keymap,
            None => {
                errorlog("xkb_keymap_new_from_names failed\n");
                return false;
            }
        }
    }
    b.keymap = Some(new_keymap);

    true
}

/******************************************************************************
 * translate_keysyms
 *****************************************************************************/

/// C: src/bindings.c:449 translate_keysyms — translates keysymbols to keycodes
/// for all bindings which use keysyms.
///
/// Operates on the active list (Tree::bindings); the C code writes into the
/// active mode's TAILQ through the bindings pointer, which is why a
/// switched-to mode is re-translated in switch_mode().
pub fn translate_keysyms(b: &Bindings, t: &mut Tree, hooks: &mut dyn BindingHooks) {
    let mut has_errors = false;
    let numlock_mask = xcb_numlock_mask();

    let keymap: &dyn XkbKeymap = match b.keymap.as_deref() {
        Some(keymap) => keymap,
        None => {
            errorlog("Could not create XKB state, cannot translate keysyms.\n");
            return;
        }
    };

    for index in 0..t.bindings.len() {
        let mut bind = t.bindings[index].clone();

        if bind.input_type == InputType::B_MOUSE {
            let symbol = bind.symbol.clone().unwrap_or_default();
            let rest = if symbol.len() >= "button".len() {
                &symbol["button".len()..]
            } else {
                ""
            };
            match crate::commands::parse_long(rest, 10) {
                Some(button) => {
                    /* C: xcb_keycode_t key = button; bind->keycode = key; */
                    bind.keycode = button as u32;
                    debuglog(&format!("Binding Mouse button, Keycode = {}\n", button));
                }
                None => errorlog(&format!(
                    "Could not translate string to button: \"{}\"\n",
                    symbol
                )),
            }
        }

        let mut group = XCB_XKB_GROUP_1;
        if (bind.event_state_mask >> 16) & crate::data::I3_XKB_GROUP_MASK_2.0 != 0 {
            group = XCB_XKB_GROUP_2;
        } else if (bind.event_state_mask >> 16) & crate::data::I3_XKB_GROUP_MASK_3.0 != 0 {
            group = XCB_XKB_GROUP_3;
        } else if (bind.event_state_mask >> 16) & crate::data::I3_XKB_GROUP_MASK_4.0 != 0 {
            group = XCB_XKB_GROUP_4;
        }
        debuglog(&format!(
            "Binding group = {}, event_state_mask = 0x{:x}, &2 = {}, &3 = {}, &4 = {}\n",
            group,
            bind.event_state_mask,
            if bind.event_state_mask & crate::data::I3_XKB_GROUP_MASK_2.0 != 0 {
                "yes"
            } else {
                "no"
            },
            if bind.event_state_mask & crate::data::I3_XKB_GROUP_MASK_3.0 != 0 {
                "yes"
            } else {
                "no"
            },
            if bind.event_state_mask & crate::data::I3_XKB_GROUP_MASK_4.0 != 0 {
                "yes"
            } else {
                "no"
            }
        ));

        /* C: xkb_state_update_mask(dummy_state, base_mods, 0, 0, 0, 0, group) */
        let dummy_state = XkbState {
            base_mods: bind.event_state_mask & 0x1FFF,
            group,
        };
        /* C: same, with base_mods ^ XCB_KEY_BUT_MASK_SHIFT */
        let dummy_state_no_shift = XkbState {
            base_mods: (bind.event_state_mask & 0x1FFF) ^ XCB_KEY_BUT_MASK_SHIFT,
            group,
        };
        /* C: base_mods | xcb_numlock_mask */
        let dummy_state_numlock = XkbState {
            base_mods: (bind.event_state_mask & 0x1FFF) | numlock_mask,
            group,
        };
        /* C: (base_mods | xcb_numlock_mask) ^ XCB_KEY_BUT_MASK_SHIFT */
        let dummy_state_numlock_no_shift = XkbState {
            base_mods: ((bind.event_state_mask & 0x1FFF) | numlock_mask)
                ^ XCB_KEY_BUT_MASK_SHIFT,
            group,
        };

        if bind.keycode > 0 {
            /* We need to specify modifiers for the keycode binding (numlock
             * fallback). */
            bind.keycodes_head.clear();

            let keycode = bind.keycode as u8;
            let event_state_mask = bind.event_state_mask;

            add_translated_key(&mut bind, keycode, event_state_mask);

            /* Also bind the key with active CapsLock */
            add_translated_key(&mut bind, keycode, event_state_mask | XCB_KEY_BUT_MASK_LOCK);

            /* If this binding is not explicitly for NumLock, check whether we
             * need to add a fallback. */
            if (event_state_mask & numlock_mask) != numlock_mask {
                let sym = keymap.state_key_get_one_sym(&dummy_state, keycode);
                let sym_numlock = keymap.state_key_get_one_sym(&dummy_state_numlock, keycode);
                if sym == sym_numlock {
                    /* Also bind the key with active NumLock */
                    add_translated_key(&mut bind, keycode, event_state_mask | numlock_mask);

                    /* Also bind the key with active NumLock+CapsLock */
                    add_translated_key(
                        &mut bind,
                        keycode,
                        event_state_mask | numlock_mask | XCB_KEY_BUT_MASK_LOCK,
                    );
                } else {
                    debuglog(&format!(
                        "Skipping automatic numlock fallback, key {} resolves to 0x{:x} with numlock\n",
                        bind.keycode, sym_numlock
                    ));
                }
            }

            t.bindings[index] = bind;
            continue;
        }

        /* We need to translate the symbol to a keycode */
        let symbol = bind.symbol.clone().unwrap_or_default();
        let keysym = keymap.keysym_from_name(&symbol);
        if keysym == XKB_KEY_NoSymbol {
            errorlog(&format!(
                "Could not translate string to key symbol: \"{}\"\n",
                symbol
            ));
            continue;
        }

        bind.keycodes_head.clear();
        {
            let mut resolving = Resolve {
                bind: &mut bind,
                keysym,
                xkb_state: dummy_state,
                xkb_state_no_shift: dummy_state_no_shift,
                xkb_state_numlock: dummy_state_numlock,
                xkb_state_numlock_no_shift: dummy_state_numlock_no_shift,
            };
            keymap.keymap_key_for_each(&mut |key| {
                add_keycode_if_matches(keymap, numlock_mask, key, &mut resolving)
            });
        }

        for binding_keycode in &bind.keycodes_head {
            /* check for duplicate bindings */
            for (check_index, check) in t.bindings.iter().enumerate() {
                if check_index == index {
                    continue;
                }
                if check.symbol.is_some() {
                    continue;
                }
                if check.keycode != binding_keycode.keycode as u32
                    || check.event_state_mask != binding_keycode.modifiers
                    || check.release != bind.release
                {
                    continue;
                }
                has_errors = true;
                errorlog(&format!(
                    "Duplicate keybinding in config file:\n  keysym = {}, keycode = {}, state_mask = 0x{:x}\n",
                    symbol, check.keycode, bind.event_state_mask
                ));
            }
        }
        debuglog(&format!(
            "state=0x{:x}, cfg=\"{}\", sym=0x{:x} -> keycodes ({})\n",
            bind.event_state_mask,
            symbol,
            keysym,
            bind.keycodes_head.len()
        ));
        t.bindings[index] = bind;
    }

    if has_errors {
        hooks.start_config_error_nagbar(b.current_configpath.as_deref(), true);
    }
}

/******************************************************************************
 * Mode switching and reordering
 *****************************************************************************/

/// C: src/bindings.c:639 switch_mode — switches the key bindings to the given
/// mode, if the mode exists.
pub fn switch_mode(
    b: &mut Bindings,
    t: &mut Tree,
    x: &mut dyn XConn,
    new_mode: &str,
    hooks: &mut dyn BindingHooks,
) {
    debuglog(&format!("Switching to mode {}\n", new_mode));

    let mode_index = match b.modes.iter().position(|m| m.name == new_mode) {
        Some(index) => index,
        None => {
            errorlog("Mode not found\n");
            return;
        }
    };

    ungrab_all_keys(x);
    /* C: bindings = mode->bindings; */
    t.bindings = b.modes[mode_index].bindings.clone();
    b.current_binding_mode = Some(b.modes[mode_index].name.clone());
    translate_keysyms(b, t, hooks);
    grab_all_keys(b, t, x);
    regrab_all_buttons(t, x);

    /* Reset all B_UPON_KEYRELEASE_IGNORE_MODS bindings to avoid possibly
     * activating one of them. */
    for bind in &mut t.bindings {
        if bind.release == BindingRelease::B_UPON_KEYRELEASE_IGNORE_MODS {
            bind.release = BindingRelease::B_UPON_KEYRELEASE;
        }
    }

    let mode = b.modes[mode_index].clone();
    hooks.ipc_send_mode_event(&mode);
}

/// Applies the `mode` commands the command layer left behind.
///
/// C: src/commands.c:986 cmd_mode calls switch_mode() inline, because the
/// command layer can reach the global bindings. The port's run_binding() holds
/// `&Bindings` across parse_command(), so cmd_mode() only queues the request in
/// the backend (ConBackend::switch_mode) and the event loop drains it here, in
/// the same iteration. Each request goes through the full switch_mode() (key
/// ungrabs/grabs, translate_keysyms, the IPC mode event).
pub fn apply_pending_mode_switches(
    b: &mut Bindings,
    t: &mut Tree,
    x: &mut dyn XConn,
    backend: &mut dyn crate::backend::ConBackend,
    hooks: &mut dyn BindingHooks,
) {
    while let Some(new_mode) = backend.take_mode_switch_request() {
        switch_mode(b, t, x, &new_mode, hooks);
    }
}

/// C: src/bindings.c:678 reorder_binding_cmp — descending by event_state_mask
/// so that get_binding() matches the more specific binding first.
fn reorder_binding_cmp(first: &Binding, second: &Binding) -> std::cmp::Ordering {
    if first.event_state_mask < second.event_state_mask {
        return std::cmp::Ordering::Greater;
    }
    if first.event_state_mask == second.event_state_mask {
        return std::cmp::Ordering::Equal;
    }
    std::cmp::Ordering::Less
}

/// C: src/bindings.c:690 reorder_bindings_of_mode
fn reorder_bindings_of_mode(mode: &mut Mode) {
    /* C copies the bindings into an array, qsorts it and rebuilds the TAILQ;
     * sort_by() on the VecDeque is the same reordering. */
    let mut bindings: Vec<Binding> = mode.bindings.drain(..).collect();
    bindings.sort_by(reorder_binding_cmp);
    for bind in bindings {
        mode.bindings.push_back(bind);
    }
}

/// C: src/bindings.c:732 reorder_bindings — reorders bindings by
/// event_state_mask descendingly.
pub fn reorder_bindings(b: &mut Bindings, t: &mut Tree) {
    let current_name = b.current_binding_mode.clone();
    for mode in &mut b.modes {
        let current_mode = current_name.as_deref() == Some(mode.name.as_str());
        reorder_bindings_of_mode(mode);
        if current_mode {
            t.bindings = mode.bindings.clone();
        }
    }
}

/******************************************************************************
 * Duplicate detection
 *****************************************************************************/

/// C: src/bindings.c:747 binding_same_key — returns true if a is a key binding
/// for the same key as b.
fn binding_same_key(a: &Binding, b: &Binding) -> bool {
    /* Check if the input types are different */
    if a.input_type != b.input_type {
        return false;
    }

    /* Check if one is using keysym while the other is using bindsym. */
    if (a.symbol.is_none() && b.symbol.is_some()) || (a.symbol.is_some() && b.symbol.is_none()) {
        return false;
    }

    /* If a is NULL, b has to be NULL, too (see previous conditional).
     * If the keycodes differ, it can't be a duplicate. */
    if let (Some(a_symbol), Some(b_symbol)) = (&a.symbol, &b.symbol) {
        if !a_symbol.eq_ignore_ascii_case(b_symbol) {
            return false;
        }
    }

    /* Check if the keycodes or modifiers are different. If so, they can't be
     * duplicate */
    if a.keycode != b.keycode || a.event_state_mask != b.event_state_mask || a.release != b.release
    {
        return false;
    }

    true
}

/// C: src/config_parser.h struct context — the only member src/bindings.c
/// touches (check_for_duplicate_bindings sets has_errors).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BindingCheckContext {
    pub has_errors: bool,
}

/// C: src/bindings.c:784 check_for_duplicate_bindings — checks for duplicate
/// key bindings.
pub fn check_for_duplicate_bindings(t: &Tree, context: &mut BindingCheckContext) {
    for current_index in 0..t.bindings.len() {
        let current = &t.bindings[current_index];
        for bind in t.bindings.iter().take(current_index) {
            /* Abort when we reach the current keybinding, only check the
             * bindings before */
            if !binding_same_key(bind, current) {
                continue;
            }

            context.has_errors = true;
            if current.keycode != 0 {
                errorlog(&format!(
                    "Duplicate keybinding in config file:\n  state mask 0x{:x} with keycode {}, command \"{}\"\n",
                    current.event_state_mask, current.keycode, current.command
                ));
            } else {
                errorlog(&format!(
                    "Duplicate keybinding in config file:\n  state mask 0x{:x} with keysym {}, command \"{}\"\n",
                    current.event_state_mask,
                    current.symbol.as_deref().unwrap_or(""),
                    current.command
                ));
            }
        }
    }
}

/******************************************************************************
 * Binding lifetime
 *****************************************************************************/

/// C: src/bindings.c:813 binding_copy — creates a dynamically allocated copy of
/// bind (Rust: an owned clone, including the keycodes list).
pub fn binding_copy(bind: &Binding) -> Binding {
    bind.clone()
}

/// C: src/bindings.c:836 binding_free — frees the binding (Rust: dropping the
/// owned value does the same; the function is kept for the call sites).
pub fn binding_free(bind: Binding) {
    drop(bind);
}

/******************************************************************************
 * Running a binding
 *****************************************************************************/

/// The members of CommandResult* that run_binding() reads.
///
/// C: src/commands_parser.h struct CommandResult
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BindingRunResult {
    pub needs_tree_render: bool,
    pub parse_error: bool,
}

/// C: src/bindings.c:859 run_binding — runs the given binding and handles parse
/// errors. If con is passed, the command is executed with that container
/// selected by criteria.
///
/// The command execution hook is ConBackend::parse_command; C's
/// [con_id="%p"] prefix uses the Con pointer value, here the stable numeric
/// handle of the container (slotmap key data) is used.
pub fn run_binding(
    b: &Bindings,
    t: &mut Tree,
    cb: &mut dyn ConBackend,
    hooks: &mut dyn BindingHooks,
    bind: &Binding,
    con: Option<ConId>,
) -> BindingRunResult {
    let command = match con {
        None => bind.command.clone(),
        Some(con) => format!("[con_id=\"{}\"] {}", con.data().as_ffi(), bind.command),
    };

    /* We need to copy the binding and command since "reload" may be part of
     * the command, and then the memory that bind points to may not contain the
     * same data anymore. */
    let bind_cp = binding_copy(bind);
    /* The "mode" command might change the current mode, so back it up to
     * correctly produce an event later. */
    let modename = b.current_binding_mode.clone();

    let needs_tree_render = cb.parse_command(t, &command);

    let mut result = BindingRunResult {
        needs_tree_render,
        parse_error: false,
    };

    if result.needs_tree_render {
        tree_render(t, cb);
    }

    result.parse_error = hooks.command_parse_error();
    if result.parse_error {
        let errorfilename = b.errorfilename.clone().unwrap_or_default();
        let pageraction = format!("i3-sensible-pager \"{}\"\n", errorfilename);
        hooks.start_command_error_nagbar(&pageraction);
    }

    hooks.ipc_send_binding_event(&bind_cp, modename.as_deref());

    result
}

/// Task-name alias of run_binding() (C: src/bindings.c:859 run_binding).
pub fn binding_runner(
    b: &Bindings,
    t: &mut Tree,
    cb: &mut dyn ConBackend,
    hooks: &mut dyn BindingHooks,
    bind: &Binding,
    con: Option<ConId>,
) -> BindingRunResult {
    run_binding(b, t, cb, hooks, bind, con)
}

/******************************************************************************
 * Keyboard-mapping changes (the bindings part of src/handlers.c)
 *****************************************************************************/

/// C: src/handlers.c:263 handle_mapping_notify — called when the keyboard
/// mapping changes (for example by using Xmodmap); re-translates symbols.
///
/// numlock_mask is C's aio_get_mod_mask_for(XCB_NUM_LOCK, keysyms) result
/// (src/handlers.c:272).
pub fn handle_mapping_notify(
    b: &mut Bindings,
    t: &mut Tree,
    x: &mut dyn XConn,
    request: u8,
    numlock_mask: u32,
    hooks: &mut dyn BindingHooks,
) {
    if request != XCB_MAPPING_KEYBOARD && request != XCB_MAPPING_MODIFIER {
        return;
    }

    debuglog("Received mapping_notify for keyboard or modifier mapping, re-grabbing keys\n");
    /* C: xcb_refresh_keyboard_mapping(keysyms, event); — the keysym cache is
     * X11-side (src/x.c); the XKB keymap is reloaded by the caller. */
    set_xcb_numlock_mask(numlock_mask);

    ungrab_all_keys(x);
    translate_keysyms(b, t, hooks);
    grab_all_keys(b, t, x);
}

/// C: src/handlers.c:1448-1456 (the XKB_STATE_NOTIFY branch) — the keyboard
/// group changed; re-grab the bindings that are valid in the new group.
pub fn handle_xkb_state_notify(b: &mut Bindings, t: &Tree, x: &mut dyn XConn, group: u32) {
    debuglog(&format!("xkb state group = {}\n", group));
    if b.xkb_current_group == group {
        return;
    }
    b.xkb_current_group = group;
    ungrab_all_keys(x);
    grab_all_keys(b, t, x);
}

/******************************************************************************
 * Tests
 *****************************************************************************/

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::XConn;
    use crate::xcb_util::RecordingConn;
    use std::collections::HashMap;

    /// One key of a fake xkb keymap.
    struct FakeKey {
        keycode: u8,
        /// Keysyms per layout group (level 0 = unshifted, level 1 = Shift).
        groups: Vec<[u32; 2]>,
    }

    /// An xkb keymap test double: keysyms per keycode/group/level plus the
    /// keysym-name table.
    struct FakeKeymap {
        keys: Vec<FakeKey>,
        names: HashMap<String, u32>,
    }

    impl FakeKeymap {
        fn new() -> FakeKeymap {
            FakeKeymap {
                keys: Vec::new(),
                names: HashMap::new(),
            }
        }

        fn add_named(&mut self, name: &str, keysym: u32, keycode: u8) {
            self.names.insert(name.to_string(), keysym);
            self.keys.push(FakeKey {
                keycode,
                groups: vec![[keysym, keysym]],
            });
        }

        fn group_of(state: &XkbState) -> usize {
            state.group as usize
        }

        fn level_of(state: &XkbState) -> usize {
            if state.base_mods & XCB_MOD_MASK_SHIFT != 0 {
                1
            } else {
                0
            }
        }

        fn boxed(&self) -> Box<dyn XkbKeymap> {
            let mut copy = FakeKeymap::new();
            copy.names = self.names.clone();
            for key in &self.keys {
                copy.keys.push(FakeKey {
                    keycode: key.keycode,
                    groups: key.groups.clone(),
                });
            }
            Box::new(copy)
        }
    }

    impl XkbKeymap for FakeKeymap {
        fn keymap_key_for_each(&self, f: &mut dyn FnMut(u8)) {
            for key in &self.keys {
                f(key.keycode);
            }
        }

        fn state_key_get_one_sym(&self, state: &XkbState, key: u8) -> u32 {
            let fake_key = match self.keys.iter().find(|k| k.keycode == key) {
                Some(fake_key) => fake_key,
                None => return XKB_KEY_NoSymbol,
            };
            /* The fake keymap uses the same layout in every group. */
            let group = FakeKeymap::group_of(state).min(fake_key.groups.len() - 1);
            fake_key.groups[group][FakeKeymap::level_of(state)]
        }

        fn state_key_get_layout(&self, _state: &XkbState, key: u8) -> u32 {
            if self.keys.iter().any(|k| k.keycode == key) {
                0
            } else {
                XKB_LAYOUT_INVALID
            }
        }

        fn state_key_get_level(&self, state: &XkbState, _key: u8, _layout: u32) -> u32 {
            FakeKeymap::level_of(state) as u32
        }

        fn keysym_from_name(&self, name: &str) -> u32 {
            self.names.get(name).copied().unwrap_or(XKB_KEY_NoSymbol)
        }

        fn keymap_key_get_syms_by_level(&self, key: u8, layout: u32, level: u32) -> Vec<u32> {
            let fake_key = match self.keys.iter().find(|k| k.keycode == key) {
                Some(fake_key) => fake_key,
                None => return Vec::new(),
            };
            let group = layout as usize;
            if group >= fake_key.groups.len() || level > 1 {
                return Vec::new();
            }
            vec![fake_key.groups[group][level as usize]]
        }
    }

    /// Records everything the hooks are asked to do.
    #[derive(Default)]
    struct RecordingHooks {
        mode_events: Vec<(String, bool)>,
        binding_events: Vec<(String, Option<String>)>,
        config_error_nagbars: Vec<(Option<String>, bool)>,
        command_error_nagbars: Vec<String>,
        parse_error: bool,
    }

    impl BindingHooks for RecordingHooks {
        fn ipc_send_mode_event(&mut self, mode: &Mode) {
            self.mode_events.push((mode.name.clone(), mode.pango_markup));
        }

        fn ipc_send_binding_event(&mut self, bind: &Binding, modename: Option<&str>) {
            self.binding_events
                .push((bind.command.clone(), modename.map(|m| m.to_string())));
        }

        fn start_config_error_nagbar(
            &mut self,
            current_configpath: Option<&str>,
            pango_markup: bool,
        ) {
            self.config_error_nagbars
                .push((current_configpath.map(|p| p.to_string()), pango_markup));
        }

        fn start_command_error_nagbar(&mut self, pager_action: &str) {
            self.command_error_nagbars.push(pager_action.to_string());
        }

        fn command_parse_error(&mut self) -> bool {
            self.parse_error
        }
    }

    fn state_with_keymap() -> Bindings {
        let mut b = Bindings::new();
        b.init_default_mode();
        let mut keymap = FakeKeymap::new();
        /* The representative case: the "move" command is bound to $mod+m. */
        keymap.add_named("m", 0x6d /* XKB_KEY_m */, 50);
        keymap.add_named("j", 0x6a /* XKB_KEY_j */, 44);
        keymap.add_named("space", 0x20, 65);
        b.keymap = Some(keymap.boxed());
        b
    }

    fn test_tree() -> Tree {
        Tree::new()
    }

    /// C: configure_binding stores the parsed mask and the symbol.
    #[test]
    fn configure_binding_parses_modifiers_and_symbol() {
        let mut b = Bindings::new();
        b.init_default_mode();
        let bind = configure_binding(
            &mut b,
            "bindsym",
            Some("Mod4+Shift"),
            "m",
            None,
            None,
            None,
            None,
            "move left",
            DEFAULT_BINDING_MODE,
            false,
        )
        .expect("binding parsed");
        assert_eq!(bind.input_type, InputType::B_KEYBOARD);
        assert_eq!(bind.release, BindingRelease::B_UPON_KEYPRESS);
        assert_eq!(
            bind.event_state_mask,
            XCB_KEY_BUT_MASK_MOD_4 | XCB_KEY_BUT_MASK_SHIFT
        );
        assert_eq!(bind.symbol.as_deref(), Some("m"));
        assert_eq!(b.modes[0].bindings.len(), 1);

        // bindsym button1 is a mouse binding.
        let mouse = configure_binding(
            &mut b,
            "bindsym",
            None,
            "button1",
            None,
            None,
            Some("--whole-window"),
            None,
            "nop",
            DEFAULT_BINDING_MODE,
            false,
        )
        .expect("mouse binding parsed");
        assert_eq!(mouse.input_type, InputType::B_MOUSE);
        assert!(mouse.whole_window);

        // bindcode takes a numeric code; garbage is rejected like C's NULL.
        assert!(configure_binding(
            &mut b,
            "bindcode",
            None,
            "not-a-number",
            None,
            None,
            None,
            None,
            "nop",
            DEFAULT_BINDING_MODE,
            false
        )
        .is_none());
        let keycode_bind = configure_binding(
            &mut b,
            "bindcode",
            Some("Mod1"),
            "66",
            None,
            None,
            None,
            None,
            "nop",
            DEFAULT_BINDING_MODE,
            false,
        )
        .expect("bindcode parsed");
        assert_eq!(keycode_bind.keycode, 66);
        assert_eq!(keycode_bind.symbol, None);
    }

    /// C: translate_keysyms resolves the symbol to keycodes, including the
    /// CapsLock combination.
    #[test]
    fn translate_keysyms_resolves_move_binding() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        let mut hooks = RecordingHooks::default();
        let bind = configure_binding(
            &mut b,
            "bindsym",
            Some("Mod4+Shift"),
            "m",
            None,
            None,
            None,
            None,
            "move left",
            DEFAULT_BINDING_MODE,
            false,
        )
        .unwrap();
        t.bindings.push_back(bind);

        translate_keysyms(&b, &mut t, &mut hooks);
        assert!(hooks.config_error_nagbars.is_empty());

        let keycodes = &t.bindings[0].keycodes_head;
        assert_eq!(keycodes.len(), 2);
        // C: ADD_TRANSLATED_KEY(key, bind->event_state_mask) first.
        assert_eq!(keycodes[0].keycode, 50);
        assert_eq!(
            keycodes[0].modifiers,
            XCB_KEY_BUT_MASK_MOD_4 | XCB_KEY_BUT_MASK_SHIFT
        );
        // Then the CapsLock variant.
        assert_eq!(
            keycodes[1].modifiers,
            XCB_KEY_BUT_MASK_MOD_4 | XCB_KEY_BUT_MASK_SHIFT | XCB_KEY_BUT_MASK_LOCK
        );
    }

    /// C: the NumLock fallback is added when the keycode resolves to the same
    /// keysym with NumLock active (src/bindings.c:424-442).
    #[test]
    fn add_keycode_if_matches_adds_numlock_fallback() {
        let mut keymap = FakeKeymap::new();
        keymap.add_named("m", 0x6d, 50);
        let mut bind = Binding {
            event_state_mask: XCB_KEY_BUT_MASK_MOD_4,
            symbol: Some("m".to_string()),
            ..Default::default()
        };
        let state = XkbState {
            base_mods: XCB_KEY_BUT_MASK_MOD_4,
            group: XCB_XKB_GROUP_1,
        };
        let mut resolving = Resolve {
            bind: &mut bind,
            keysym: 0x6d,
            xkb_state: state,
            xkb_state_no_shift: state,
            xkb_state_numlock: XkbState {
                base_mods: XCB_KEY_BUT_MASK_MOD_4 | XCB_KEY_BUT_MASK_MOD_2,
                group: XCB_XKB_GROUP_1,
            },
            xkb_state_numlock_no_shift: XkbState {
                base_mods: XCB_KEY_BUT_MASK_MOD_4 | XCB_KEY_BUT_MASK_MOD_2,
                group: XCB_XKB_GROUP_1,
            },
        };
        add_keycode_if_matches(&keymap, XCB_KEY_BUT_MASK_MOD_2, 50, &mut resolving);
        let mods: Vec<u32> = bind.keycodes_head.iter().map(|k| k.modifiers).collect();
        assert_eq!(
            mods,
            vec![
                XCB_KEY_BUT_MASK_MOD_4,
                XCB_KEY_BUT_MASK_MOD_4 | XCB_KEY_BUT_MASK_LOCK,
                XCB_KEY_BUT_MASK_MOD_4 | XCB_KEY_BUT_MASK_MOD_2,
                XCB_KEY_BUT_MASK_MOD_4 | XCB_KEY_BUT_MASK_MOD_2 | XCB_KEY_BUT_MASK_LOCK,
            ]
        );
    }

    /// C: grab_all_keys grabs exactly the translated (keycode, mods) pairs on
    /// the root window.
    #[test]
    fn grab_all_keys_uses_translated_keycodes() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        let mut hooks = RecordingHooks::default();
        let bind = configure_binding(
            &mut b,
            "bindsym",
            Some("Mod4"),
            "m",
            None,
            None,
            None,
            None,
            "mode resize",
            DEFAULT_BINDING_MODE,
            false,
        )
        .unwrap();
        t.bindings.push_back(bind);
        translate_keysyms(&b, &mut t, &mut hooks);

        let mut x = RecordingConn::new(7);
        grab_all_keys(&b, &t, &mut x);
        let grabs: Vec<&String> = x.log.iter().filter(|l| l.starts_with("grab_key")).collect();
        assert_eq!(grabs.len(), 2);
        assert_eq!(
            grabs[0].as_str(),
            "grab_key(owner_events=false, win=0x7, mods=64, key=50, ptr=1, kbd=1)"
        );
        assert_eq!(
            grabs[1].as_str(),
            "grab_key(owner_events=false, win=0x7, mods=66, key=50, ptr=1, kbd=1)"
        );
    }

    /// C: ungrab_all_keys ungrabs every key on the root window.
    #[test]
    fn ungrab_all_keys_ungrabs_any_key() {
        let mut x = RecordingConn::new(3);
        ungrab_all_keys(&mut x);
        assert_eq!(
            x.log.last().unwrap().as_str(),
            "ungrab_key(key=0, win=0x3, mods=65535)"
        );
    }

    /// C: get_binding matches the modifier mask, the group mask and the release
    /// state.
    #[test]
    fn get_binding_matches_modifiers_and_groups() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        let mut hooks = RecordingHooks::default();

        // $mod+m (press semantics) and $mod+j with Group2.
        t.bindings.push_back(
            configure_binding(
                &mut b,
                "bindsym",
                Some("Mod4"),
                "m",
                None,
                None,
                None,
                None,
                "nop",
                DEFAULT_BINDING_MODE,
                false,
            )
            .unwrap(),
        );
        t.bindings.push_back(
            configure_binding(
                &mut b,
                "bindsym",
                Some("Mod4+Group2"),
                "j",
                None,
                None,
                None,
                None,
                "nop",
                DEFAULT_BINDING_MODE,
                false,
            )
            .unwrap(),
        );
        translate_keysyms(&b, &mut t, &mut hooks);

        // Mod4 + keycode 50 in group 1 -> binding 0.
        assert_eq!(
            get_binding(&mut t, XCB_KEY_BUT_MASK_MOD_4, false, 50, InputType::B_KEYBOARD),
            Some(0)
        );
        // Without the modifier -> no match.
        assert_eq!(get_binding(&mut t, 0, false, 50, InputType::B_KEYBOARD), None);
        // Group 1 + keycode 44 -> the Group2 binding does not match.
        assert_eq!(
            get_binding(
                &mut t,
                (crate::data::I3_XKB_GROUP_MASK_1.0 << 16) | XCB_KEY_BUT_MASK_MOD_4,
                false,
                44,
                InputType::B_KEYBOARD
            ),
            None
        );
        // Group 2 + keycode 44 -> binding 1 matches (ticket #1775 logic).
        assert_eq!(
            get_binding(
                &mut t,
                (crate::data::I3_XKB_GROUP_MASK_2.0 << 16) | XCB_KEY_BUT_MASK_MOD_4,
                false,
                44,
                InputType::B_KEYBOARD
            ),
            Some(1)
        );
    }

    /// C: get_binding_from_xcb_event transforms bits 13/14 of the event state
    /// into the XKB group mask (ticket #1775).
    #[test]
    fn get_binding_from_xcb_event_transforms_group_bits() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        let mut hooks = RecordingHooks::default();
        t.bindings.push_back(
            configure_binding(
                &mut b,
                "bindsym",
                Some("Group2"),
                "m",
                None,
                None,
                None,
                None,
                "nop",
                DEFAULT_BINDING_MODE,
                false,
            )
            .unwrap(),
        );
        translate_keysyms(&b, &mut t, &mut hooks);

        // 0x2000 == XCB_XKB_GROUP_2 << 13; CapsLock (lock) is stripped.
        let event = BindingEvent {
            response_type: XCB_KEY_PRESS,
            state: 0x2000 | XCB_KEY_BUT_MASK_LOCK as u16,
            detail: 50,
        };
        assert_eq!(get_binding_from_xcb_event(&mut t, &event), Some(0));

        // Bits 13/14 cleared -> group 1 -> no match.
        let event_group1 = BindingEvent {
            response_type: XCB_KEY_PRESS,
            state: 0,
            detail: 50,
        };
        assert_eq!(get_binding_from_xcb_event(&mut t, &event_group1), None);
    }

    /// C: a release binding is matched on press (marked
    /// B_UPON_KEYRELEASE_IGNORE_MODS) and then fires on release even if the
    /// modifiers were released first.
    #[test]
    fn release_binding_transitions_through_ignore_mods() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        let mut hooks = RecordingHooks::default();
        t.bindings.push_back(
            configure_binding(
                &mut b,
                "bindsym",
                Some("Mod4"),
                "m",
                Some("--release"),
                None,
                None,
                None,
                "nop",
                DEFAULT_BINDING_MODE,
                false,
            )
            .unwrap(),
        );
        translate_keysyms(&b, &mut t, &mut hooks);
        assert_eq!(t.bindings[0].release, BindingRelease::B_UPON_KEYRELEASE);

        // Press with Mod4: the binding is marked and does not run yet.
        assert_eq!(
            get_binding(&mut t, XCB_KEY_BUT_MASK_MOD_4, false, 50, InputType::B_KEYBOARD),
            None
        );
        assert_eq!(
            t.bindings[0].release,
            BindingRelease::B_UPON_KEYRELEASE_IGNORE_MODS
        );

        // Release with the modifiers already released: still matches.
        assert_eq!(
            get_binding(&mut t, 0, true, 50, InputType::B_KEYBOARD),
            Some(0)
        );

        // A press resets the IGNORE_MODS state back to B_UPON_KEYRELEASE.
        assert_eq!(
            get_binding(&mut t, XCB_KEY_BUT_MASK_MOD_4, false, 50, InputType::B_KEYBOARD),
            None
        );
        assert_eq!(
            t.bindings[0].release,
            BindingRelease::B_UPON_KEYRELEASE_IGNORE_MODS
        );
        // ... and a release with the exact modifiers matches as well.
        assert_eq!(
            get_binding(&mut t, XCB_KEY_BUT_MASK_MOD_4, true, 50, InputType::B_KEYBOARD),
            Some(0)
        );
    }

    /// C: a B_UPON_KEYPRESS binding never matches a release event.
    #[test]
    fn press_binding_does_not_match_release() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        let mut hooks = RecordingHooks::default();
        t.bindings.push_back(
            configure_binding(
                &mut b,
                "bindsym",
                Some("Mod4"),
                "m",
                None,
                None,
                None,
                None,
                "nop",
                DEFAULT_BINDING_MODE,
                false,
            )
            .unwrap(),
        );
        translate_keysyms(&b, &mut t, &mut hooks);
        assert_eq!(
            get_binding(&mut t, XCB_KEY_BUT_MASK_MOD_4, true, 50, InputType::B_KEYBOARD),
            None
        );
    }

    /// C: switch_mode ungrabs, re-translates, grabs the new mode's bindings and
    /// emits the IPC mode event.
    #[test]
    fn switch_mode_swaps_bindings_and_emits_event() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        let mut hooks = RecordingHooks::default();
        let mut x = RecordingConn::new(9);

        configure_binding(
            &mut b,
            "bindsym",
            Some("Mod4"),
            "m",
            None,
            None,
            None,
            None,
            "nop",
            DEFAULT_BINDING_MODE,
            false,
        );
        b.modes[0].pango_markup = true;
        configure_binding(
            &mut b,
            "bindsym",
            Some("Mod1"),
            "j",
            None,
            None,
            None,
            None,
            "resize shrink width 10 px",
            "resize",
            false,
        );

        switch_mode(&mut b, &mut t, &mut x, "resize", &mut hooks);

        assert_eq!(b.current_binding_mode.as_deref(), Some("resize"));
        assert_eq!(t.bindings.len(), 1);
        assert_eq!(t.bindings[0].command, "resize shrink width 10 px");
        assert_eq!(hooks.mode_events, vec![("resize".to_string(), false)]);
        assert_eq!(
            x.log.first().unwrap().as_str(),
            "ungrab_key(key=0, win=0x9, mods=65535)"
        );
        let grabs: Vec<&String> = x.log.iter().filter(|l| l.starts_with("grab_key")).collect();
        assert_eq!(grabs.len(), 2);
        assert!(grabs[0].contains("mods=8, key=44"));

        // An unknown mode logs and changes nothing.
        let before = t.bindings.len();
        switch_mode(&mut b, &mut t, &mut x, "nope", &mut hooks);
        assert_eq!(t.bindings.len(), before);
        assert_eq!(b.current_binding_mode.as_deref(), Some("resize"));
    }

    /// A backend that hands out pre-queued mode requests, like XBackend does
    /// after cmd_mode (C: cmd_mode calls switch_mode() inline).
    struct ModeRequestBackend {
        requests: Vec<String>,
    }

    impl ConBackend for ModeRequestBackend {
        fn take_mode_switch_request(&mut self) -> Option<String> {
            if self.requests.is_empty() {
                None
            } else {
                Some(self.requests.remove(0))
            }
        }
    }

    /// The event loop's drain: every `mode` command requested by the command
    /// layer switches the mode (and emits the IPC mode event); an unknown mode
    /// is a no-op. C: the switch_mode() call inside cmd_mode.
    #[test]
    fn apply_pending_mode_switches_drains_requests() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        let mut x = RecordingConn::new(9);
        let mut hooks = RecordingHooks::default();

        configure_binding(
            &mut b,
            "bindsym",
            Some("Mod1"),
            "j",
            None,
            None,
            None,
            None,
            "resize shrink width 10 px",
            "resize",
            false,
        );
        configure_binding(
            &mut b,
            "bindsym",
            Some("Mod4"),
            "m",
            None,
            None,
            None,
            None,
            "nop",
            DEFAULT_BINDING_MODE,
            false,
        );

        let mut backend = ModeRequestBackend {
            requests: vec![
                "resize".to_string(),
                "nope".to_string(),
                "default".to_string(),
            ],
        };

        apply_pending_mode_switches(&mut b, &mut t, &mut x, &mut backend, &mut hooks);

        // "nope" changed nothing, the last request ("default") is current.
        assert_eq!(b.current_binding_mode.as_deref(), Some(DEFAULT_BINDING_MODE));
        assert_eq!(t.bindings.len(), 1);
        assert_eq!(t.bindings[0].command, "nop");
        assert_eq!(
            hooks.mode_events,
            vec![
                ("resize".to_string(), false),
                (DEFAULT_BINDING_MODE.to_string(), false),
            ]
        );
        // An empty queue is not a request.
        assert!(backend.requests.is_empty());
        apply_pending_mode_switches(&mut b, &mut t, &mut x, &mut backend, &mut hooks);
        assert_eq!(hooks.mode_events.len(), 2);
    }

    /// C: reorder_bindings sorts descending by event_state_mask so that the
    /// more specific binding wins.
    #[test]
    fn reorder_bindings_puts_specific_first() {
        let mut b = Bindings::new();
        b.init_default_mode();
        configure_binding(
            &mut b,
            "bindsym",
            None,
            "n",
            None,
            None,
            None,
            None,
            "nop lower-case n pressed",
            DEFAULT_BINDING_MODE,
            false,
        );
        configure_binding(
            &mut b,
            "bindsym",
            Some("Shift"),
            "n",
            None,
            None,
            None,
            None,
            "nop upper-case n pressed",
            DEFAULT_BINDING_MODE,
            false,
        );
        b.current_binding_mode = Some(DEFAULT_BINDING_MODE.to_string());
        let mut t = test_tree();
        t.bindings = b.modes[0].bindings.clone();
        assert_eq!(t.bindings[0].event_state_mask, 0);

        reorder_bindings(&mut b, &mut t);
        assert_eq!(t.bindings[0].event_state_mask, XCB_KEY_BUT_MASK_SHIFT);
        assert_eq!(
            b.modes[0].bindings[0].event_state_mask,
            XCB_KEY_BUT_MASK_SHIFT
        );
    }

    /// C: check_for_duplicate_bindings reports the same keysym twice.
    #[test]
    fn check_for_duplicate_bindings_reports_duplicates() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        for _ in 0..2 {
            t.bindings.push_back(
                configure_binding(
                    &mut b,
                    "bindsym",
                    Some("Mod4"),
                    "m",
                    None,
                    None,
                    None,
                    None,
                    "nop",
                    DEFAULT_BINDING_MODE,
                    false,
                )
                .unwrap(),
            );
        }
        let mut context = BindingCheckContext::default();
        check_for_duplicate_bindings(&t, &mut context);
        assert!(context.has_errors);
    }

    /// C: run_binding executes the command through the command parser and emits
    /// the binding IPC event with the mode that was current *before* the
    /// command ran.
    #[test]
    fn run_binding_executes_and_emits_event() {
        struct TestBackend {
            commands: Vec<String>,
        }
        impl ConBackend for TestBackend {
            fn parse_command(&mut self, _t: &mut Tree, command: &str) -> bool {
                self.commands.push(command.to_string());
                false
            }
        }

        let mut b = Bindings::new();
        b.init_default_mode();
        b.current_binding_mode = Some(DEFAULT_BINDING_MODE.to_string());
        b.errorfilename = Some("/tmp/i3/errorlog".to_string());
        let mut t = test_tree();
        let bind = configure_binding(
            &mut b,
            "bindsym",
            Some("Mod4"),
            "m",
            None,
            None,
            None,
            None,
            "mode resize",
            DEFAULT_BINDING_MODE,
            false,
        )
        .unwrap();
        let mut hooks = RecordingHooks::default();
        let mut cb = TestBackend {
            commands: Vec::new(),
        };

        let result = run_binding(&b, &mut t, &mut cb, &mut hooks, &bind, None);
        assert!(!result.needs_tree_render);
        assert!(!result.parse_error);
        assert_eq!(cb.commands, vec!["mode resize".to_string()]);
        assert_eq!(
            hooks.binding_events,
            vec![("mode resize".to_string(), Some("default".to_string()))]
        );
        assert!(hooks.command_error_nagbars.is_empty());
    }

    /// C: run_binding prepends the con_id criteria when a con is passed and
    /// shows the error nagbar on a parse error.
    #[test]
    fn run_binding_with_con_and_parse_error() {
        struct TestBackend {
            commands: Vec<String>,
        }
        impl ConBackend for TestBackend {
            fn parse_command(&mut self, _t: &mut Tree, command: &str) -> bool {
                self.commands.push(command.to_string());
                false
            }
        }

        let mut b = Bindings::new();
        b.init_default_mode();
        b.errorfilename = Some("/tmp/i3/errorlog".to_string());
        let mut t = test_tree();
        let con = t.cons.insert(crate::data::Con::default());
        let bind = configure_binding(
            &mut b,
            "bindsym",
            Some("Mod4"),
            "m",
            None,
            None,
            None,
            None,
            "focus left",
            DEFAULT_BINDING_MODE,
            false,
        )
        .unwrap();
        let mut hooks = RecordingHooks {
            parse_error: true,
            ..Default::default()
        };
        let mut cb = TestBackend {
            commands: Vec::new(),
        };

        let result = run_binding(&b, &mut t, &mut cb, &mut hooks, &bind, Some(con));
        assert!(result.parse_error);
        assert_eq!(
            cb.commands,
            vec![format!("[con_id=\"{}\"] focus left", con.data().as_ffi())]
        );
        assert_eq!(
            hooks.command_error_nagbars,
            vec!["i3-sensible-pager \"/tmp/i3/errorlog\"\n".to_string()]
        );
    }

    /// C: bindings_get_buttons_to_grab always contains 1-3 plus whole-window
    /// mouse bindings (without duplicates).
    #[test]
    fn buttons_to_grab_includes_whole_window_bindings() {
        let mut b = Bindings::new();
        b.init_default_mode();
        let mut t = test_tree();
        for button in ["button4", "button4", "button9"] {
            t.bindings.push_back(
                configure_binding(
                    &mut b,
                    "bindsym",
                    None,
                    button,
                    None,
                    None,
                    Some("--whole-window"),
                    None,
                    "nop",
                    DEFAULT_BINDING_MODE,
                    false,
                )
                .unwrap(),
            );
        }
        // A non-whole-window binding is ignored.
        t.bindings.push_back(
            configure_binding(
                &mut b,
                "bindsym",
                None,
                "button5",
                None,
                None,
                None,
                None,
                "nop",
                DEFAULT_BINDING_MODE,
                false,
            )
            .unwrap(),
        );
        assert_eq!(bindings_get_buttons_to_grab(&t), vec![1, 2, 3, 4, 9]);
    }

    /// C: regrab_all_buttons ungrabs and regrabs on every con with a window.
    #[test]
    fn regrab_all_buttons_visits_window_cons() {
        let mut b = Bindings::new();
        b.init_default_mode();
        let mut t = test_tree();
        t.bindings.push_back(
            configure_binding(
                &mut b,
                "bindsym",
                None,
                "button1",
                None,
                None,
                Some("--whole-window"),
                None,
                "nop",
                DEFAULT_BINDING_MODE,
                false,
            )
            .unwrap(),
        );
        let window = t.windows.insert(crate::data::Window {
            id: 0x42,
            ..Default::default()
        });
        let con = t.cons.insert(crate::data::Con {
            window: Some(window),
            ..Default::default()
        });
        t.all_cons.push_back(con);
        // A con without a window is skipped.
        let empty = t.cons.insert(crate::data::Con::default());
        t.all_cons.push_back(empty);

        let mut x = RecordingConn::new(1);
        regrab_all_buttons(&t, &mut x);
        assert_eq!(x.log[0], "grab_server");
        assert_eq!(x.log[1], "ungrab_button(button=0, win=0x42, mods=65535)");
        assert_eq!(x.log[2], "grab_buttons(0x42, [1, 2, 3])");
        assert_eq!(x.log.last().unwrap(), "ungrab_server");
        assert_eq!(x.log.len(), 4);
    }

    /// C: load_keymap falls back to the local RMLVO when there is no XKB
    /// device, reading _XKB_RULES_NAMES from the root window.
    #[test]
    fn load_keymap_falls_back_to_rmlvo() {
        struct Factory {
            names: Option<XkbRuleNames>,
        }
        impl XkbKeymapFactory for Factory {
            fn keymap_new_from_names(&mut self, names: &XkbRuleNames) -> Option<Box<dyn XkbKeymap>> {
                self.names = Some(names.clone());
                Some(Box::new(FakeKeymap::new()))
            }
        }

        let mut b = Bindings::new();
        b.xkb_supported = true;
        let mut x = RecordingConn::new(1);
        let atom = x.intern_atom("_XKB_RULES_NAMES").expect("atom interned");
        let value: Vec<u8> = ["evdev", "pc105", "de", "", ""]
            .iter()
            .flat_map(|part| part.bytes().chain(std::iter::once(0)))
            .collect();
        x.props.insert(
            (1, atom),
            PropertyReply {
                type_: 0,
                format: 8,
                value,
            },
        );
        let mut factory = Factory { names: None };

        assert!(load_keymap(&mut b, &mut x, &mut factory));
        assert!(b.keymap.is_some());
        let names = factory.names.expect("names passed to the factory");
        assert_eq!(names.rules.as_deref(), Some("evdev"));
        assert_eq!(names.model.as_deref(), Some("pc105"));
        assert_eq!(names.layout.as_deref(), Some("de"));
        assert_eq!(names.variant.as_deref(), Some(""));
    }

    /// C: handle_mapping_notify re-grabs the keys after an Xmodmap change.
    #[test]
    fn handle_mapping_notify_regrabs() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        let mut hooks = RecordingHooks::default();
        let mut x = RecordingConn::new(11);
        t.bindings.push_back(
            configure_binding(
                &mut b,
                "bindsym",
                Some("Mod4"),
                "m",
                None,
                None,
                None,
                None,
                "nop",
                DEFAULT_BINDING_MODE,
                false,
            )
            .unwrap(),
        );

        handle_mapping_notify(&mut b, &mut t, &mut x, XCB_MAPPING_MODIFIER, 0, &mut hooks);
        assert_eq!(x.log[0], "ungrab_key(key=0, win=0xb, mods=65535)");
        assert_eq!(t.bindings[0].keycodes_head.len(), 2);
        let grabs = x.log.iter().filter(|l| l.starts_with("grab_key")).count();
        assert_eq!(grabs, 2);

        // A mapping change that is neither keyboard nor modifier is ignored.
        let before = x.log.len();
        handle_mapping_notify(&mut b, &mut t, &mut x, 5, 0, &mut hooks);
        assert_eq!(x.log.len(), before);
    }

    /// C: handle_xkb_state_notify re-grabs when the keyboard group changes.
    #[test]
    fn handle_xkb_state_notify_regrabs_on_group_change() {
        let mut b = state_with_keymap();
        let mut t = test_tree();
        t.bindings.push_back(
            configure_binding(
                &mut b,
                "bindsym",
                Some("Group2"),
                "m",
                None,
                None,
                None,
                None,
                "nop",
                DEFAULT_BINDING_MODE,
                false,
            )
            .unwrap(),
        );
        let mut hooks = RecordingHooks::default();
        translate_keysyms(&b, &mut t, &mut hooks);

        let mut x = RecordingConn::new(5);
        // Same group: nothing happens.
        handle_xkb_state_notify(&mut b, &t, &mut x, XCB_XKB_GROUP_1);
        assert!(x.log.is_empty());
        // Group 2: ungrabs and grabs the Group2 binding.
        handle_xkb_state_notify(&mut b, &t, &mut x, XCB_XKB_GROUP_2);
        assert_eq!(x.log[0], "ungrab_key(key=0, win=0x5, mods=65535)");
        assert_eq!(
            x.log.iter().filter(|l| l.starts_with("grab_key")).count(),
            2
        );
    }
}
