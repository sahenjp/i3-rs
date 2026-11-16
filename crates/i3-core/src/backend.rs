//! ConBackend: the injection point for all X11/IPC side effects used by the
//! tree layer. The ported logic calls these methods at the exact points where
//! the C code calls into x.c / ipc.c / ewmh.c. `NullBackend` implements every
//! method as a no-op and is used by the unit tests.
//!
//! The real X11 implementation lives in a later phase.

use crate::data::{ConId, KillWindow, Rect, WindowId};
use crate::tree::Tree;

/// All X11 / IPC / EWMH side effects of the tree layer.
///
/// Method names mirror the C functions they stand in for
/// (C: src/x.c, src/ipc.c, src/ewmh.c, src/load_layout.c, src/match.c).
#[allow(unused_variables)]
pub trait ConBackend {
    /// C: src/commands.c:1160 cmd_move_window_to_mouse -- the command layer
    /// needs the pointer position, but only the X11 layer owns the connection.
    /// Returns (window under the pointer, root x, root y).
    fn x_query_pointer_root(&mut self) -> Option<(u32, i32, i32)> {
        None
    }
    /// C: src/x.c (x_con_init) -- create the frame window for a new con.
    fn x_con_init(&mut self, t: &mut Tree, con: ConId) {}

    /// C: src/x.c (x_con_kill) -- destroy the con's frame window.
    fn x_con_kill(&mut self, t: &mut Tree, con: ConId) {}

    /// C: src/x.c (x_set_name) -- set the X11 name of the con's frame window.
    fn x_set_name(&mut self, t: &Tree, con: ConId, name: &str) {}

    /// C: src/x.c (x_window_kill) -- send WM_DELETE / kill the client.
    fn x_window_kill(&mut self, t: &Tree, window_id: u32, kill_window: KillWindow) {}

    /// C: src/tree.c:226-260 -- the unmap + reparent-to-root + ignore-event
    /// block of tree_close_internal (xcb_change_window_attributes /
    /// xcb_unmap_window / xcb_reparent_window / add_ignore_event).
    fn x_reparent_window_to_root(&mut self, t: &Tree, window_id: u32, x: u32, y: u32) {}

    /// C: src/tree.c:262-266 -- set WM_STATE to WM_STATE_WITHDRAWN.
    fn x_set_wm_state_withdrawn(&mut self, t: &Tree, window_id: u32) {}

    /// C: src/tree.c:273 -- xcb_change_save_set(XCB_SET_MODE_DELETE, ...).
    fn x_change_save_set(&mut self, t: &Tree, window_id: u32) {}

    /// C: src/tree.c:277-279 -- xcb_shape_select_input(..., false).
    fn x_shape_select_input(&mut self, t: &Tree, window_id: u32, enable: bool) {}

    /// C: src/x.c:1565 x_mask_event_mask — set FRAME_EVENT_MASK & mask on
    /// every mapped frame window (used by the drag/resize loops to suppress
    /// EnterNotify: they must not change the focus while dragging).
    fn x_mask_event_mask(&mut self, t: &Tree, mask: u32) {}

    /// C: src/x.c (x_push_changes) -- push the rendered tree to X11.
    fn x_push_changes(&mut self, t: &mut Tree, con: ConId) {}

    /// C: src/x.c:199 (x_reinit) -- reset the cached frame state
    /// (child_mapped, window_rect) after the con's client window changed.
    fn x_reinit(&mut self, t: &mut Tree, con: ConId) {}

    /// C: src/x.c (x_raise_con) -- raise the con's frame window.
    fn x_raise_con(&mut self, t: &Tree, con: ConId) {}

    /// C: src/x.c:1553 (x_set_warp_to) -- set/clear the container whose rect
    /// the pointer is warped to on the next x_push_changes().
    fn x_set_warp_to(&mut self, t: &Tree, con: Option<ConId>) {}

    /// C: src/x.c (x_move_win) -- move the client window from src to dst.
    fn x_move_win(&mut self, t: &mut Tree, src: ConId, dst: ConId) {}

    /// C: src/x.c (x_reparent_child) -- reparent the client window of src into dst.
    fn x_reparent_child(&mut self, t: &mut Tree, dst: ConId, src: ConId) {}

    /// C: src/ipc.c (ipc_send_window_event).
    fn ipc_send_window_event(&mut self, t: &Tree, event: &str, con: ConId) {}

    /// C: no counterpart — the draw code reads the globals directly
    /// (src/x.c x_draw_decoration uses config.client.*, config.font.height).
    /// The port's drawing backend keeps a copy (XBackend::set_draw_config), so
    /// every load_configuration() caller has to refresh it: the startup path
    /// does, and cmd_reload does right before tree_render() — otherwise the
    /// decorations would keep the colors/font of the previous configuration.
    fn apply_draw_config(&mut self, _config: &crate::config::Configuration) {}

    /// C: src/ipc.c (ipc_send_workspace_event).
    fn ipc_send_workspace_event(&mut self, t: &Tree, event: &str, ws: ConId, current: Option<ConId>) {}

    /// C: src/ipc.c:1673 ipc_send_barconfig_update_event — sends the dumped
    /// Barconfig to the subscribers of the "barconfig_update" event.
    fn ipc_send_barconfig_update_event(&mut self, _t: &Tree, _barconfig: &crate::config::Barconfig) {}

    /// C: src/ewmh.c (ewmh_update_wm_desktop).
    fn ewmh_update_wm_desktop(&mut self, t: &mut Tree) {}

    /// C: src/ewmh.c (ewmh_update_current_desktop).
    fn ewmh_update_current_desktop(&mut self, t: &mut Tree) {}

    /// C: src/ewmh.c (ewmh_update_desktop_properties).
    fn ewmh_update_desktop_properties(&mut self, t: &mut Tree) {}

    /// C: src/ewmh.c:281 ewmh_update_sticky — add/remove _NET_WM_STATE_STICKY
    /// on a client window. (src/commands.c:1537 cmd_sticky and the
    /// _NET_WM_STATE_STICKY client message in src/handlers.c:717.)
    fn ewmh_update_sticky(&mut self, _window_id: u32, _sticky: bool) {}

    /// C: src/ewmh.c:218 ewmh_update_visible_name — set/remove
    /// _NET_WM_VISIBLE_NAME with the title_format result.
    /// (src/commands.c:2038/2044 cmd_title_format, src/window.c:86/126.)
    fn ewmh_update_visible_name(&mut self, _window_id: u32, _name: Option<&str>) {}

    /// C: src/load_layout.c (restore_open_placeholder_windows).
    fn restore_open_placeholder_windows(&mut self, t: &mut Tree, con: ConId) {}

    /// C: libi3 (slurp) -- read a file into a buffer.
    fn slurp(&mut self, path: &str) -> Option<Vec<u8>> {
        None
    }

    /// C: src/load_layout.c (tree_append_json). Returns whether a root con
    /// could be appended.
    fn tree_append_json(&mut self, t: &mut Tree, con: ConId, buf: &[u8]) -> bool {
        false
    }

    /// C: src/startup.c (startup_sequence_delete_by_window).
    fn startup_sequence_delete_by_window(&mut self, t: &Tree, window: WindowId) {}

    /// C: src/commands.c (parse_command). Returns needs_tree_render.
    fn parse_command(&mut self, t: &mut Tree, command: &str) -> bool {
        false
    }

    /// C: src/startup.c:136 start_application — starts `command` through the
    /// shell (`exec` command, config autostarts). `window_id` is XCB_NONE
    /// unless the criteria selected a container with a window.
    ///
    /// C forks and execs inline because the command layer can reach everything
    /// it needs through globals; the port injects the process layer this way
    /// (startup::CommandExec is the /bin/sh -c spawn) so that the command layer
    /// stays testable without forking.
    fn start_application(
        &mut self,
        t: &Tree,
        command: &str,
        no_startup_id: bool,
        window_id: u32,
    ) {
        let _ = (t, command, no_startup_id, window_id);
    }

    /// C: src/bindings.c:639 switch_mode — switches the key bindings to the
    /// given mode.
    ///
    /// C calls this synchronously from cmd_mode() because the command layer can
    /// reach the global `bindings`; the port's run_binding() holds `&Bindings`
    /// across parse_command(), so a synchronous mutation is not possible from
    /// there. The real backend records the request and the event loop applies it
    /// with bindings::switch_mode() in the same iteration (see
    /// take_mode_switch_request()).
    fn switch_mode(&mut self, t: &mut Tree, mode: &str) {
        let _ = (t, mode);
    }

    /// The mode switch requested by the `mode` command (see [switch_mode]),
    /// or None. The C counterpart is the direct switch_mode() call inside
    /// cmd_mode.
    fn take_mode_switch_request(&mut self) -> Option<String> {
        None
    }

    /// C: src/floating.c (floating_set_hint_atom).
    fn floating_set_hint_atom(&mut self, t: &Tree, con: ConId, floating: bool) {}

    /// C: src/x.c -- add/remove _NET_WM_STATE_FULLSCREEN on a client window.
    fn x_set_net_wm_state_fullscreen(&mut self, t: &Tree, window_id: u32, add: bool) {}

    /// C: src/x.c (x_push_node) -- push a single con (used by floating.c).
    fn x_push_node(&mut self, t: &mut Tree, con: ConId) {}

    /// C: src/ewmh.c (ewmh_update_active_window) -- misc hook kept for later phases.
    fn ewmh_update_misc(&mut self, t: &Tree) {}
}

/// No-op backend for tests and non-X usage.
///
/// C: n/a (stands in for the absence of an X server).
#[derive(Debug, Clone, Copy, Default)]
pub struct NullBackend;

impl ConBackend for NullBackend {}

/****************************************************************************
 * XConn: the minimal X11 connection abstraction used by the X11/event layer
 * (Phase 3: x.rs, handlers.rs, manage.rs, randr.rs, ewmh.rs, xcb_util.rs).
 ****************************************************************************/

/// C: xcb_prop_mode_t (XCB_PROP_MODE_REPLACE=0, PREPEND=1, APPEND=2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropMode {
    REPLACE = 0,
    PREPEND = 1,
    APPEND = 2,
}

/// Subset of `xcb_get_property_reply_t` (value bytes are raw, format 8/16/32).
/// C: xcb_get_property_reply_t
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PropertyReply {
    /// The actual type of the property (an atom id).
    pub type_: u32,
    /// 8, 16 or 32.
    pub format: u8,
    /// Raw property value bytes.
    pub value: Vec<u8>,
}

impl PropertyReply {
    /// C: xcb_get_property_value_length (length in bytes of the value).
    pub fn value_len(&self) -> usize {
        self.value.len()
    }

    /// Interpret the value as a list of 32-bit words (atoms / cardinals).
    pub fn words32(&self) -> Vec<u32> {
        self.value
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    /// Interpret the value as a NUL-terminated string list, returning the first.
    pub fn first_string(&self) -> Option<String> {
        let s = std::str::from_utf8(&self.value).ok()?;
        s.split('\u{0}').next().map(|s| s.to_string())
    }
}

/// Subset of `xcb_query_pointer_reply_t`.
///
/// C: xcb_query_pointer_reply_t (src/main.c:990 picks the output under the
/// pointer with root_x / root_y)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PointerReply {
    pub root_x: i16,
    pub root_y: i16,
    pub child: u32,
}

/// Subset of `xcb_get_window_attributes_reply_t`.
/// C: xcb_get_window_attributes_reply_t
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowAttrReply {
    /// XCB_MAP_STATE_*: 0 = UnMapped, 1 = UnViewable, 2 = Viewable.
    pub map_state: u8,
    pub override_redirect: bool,
    pub visual: u32,
}

/// Subset of `xcb_get_geometry_reply_t`.
/// C: xcb_get_geometry_reply_t
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GeometryReply {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub border_width: u16,
    pub depth: u16,
    pub visual: u32,
}

/// Subset of `xcb_screen_t` (the root screen, C: `root_screen`).
/// C: xcb_screen_t
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScreenInfo {
    pub root: u32,
    pub width_in_pixels: u16,
    pub height_in_pixels: u16,
    pub root_depth: u16,
    pub root_visual: u32,
    pub default_colormap: u32,
    pub black_pixel: u32,
    pub white_pixel: u32,
}

/// C: XCB_NONE — the "not set" value of RandR output/crtc ids.
pub const XCB_NONE: u32 = 0xffff_ffff;

/// Subset of `xcb_randr_monitor_info_t` as returned by
/// xcb_randr_get_monitors (RandR 1.5).
///
/// C: xcb_randr_monitor_info_t (used by src/randr.c:583
/// randr_query_outputs_15)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MonitorInfo {
    /// The monitor name, an atom (C: monitor_info->name, resolved through
    /// xcb_get_atom_name in src/randr.c:626).
    pub name: u32,
    pub primary: bool,
    pub automatic: bool,
    /// C: int16_t x / y of the monitor_info.
    pub x: i16,
    pub y: i16,
    /// C: uint16_t width / height in pixels.
    pub width: u16,
    pub height: u16,
    /// C: uint16_t width_in_millimeters (x11rb widens it to u32).
    pub width_in_millimeters: u32,
    pub height_in_millimeters: u32,
    /// The RandR outputs belonging to this monitor
    /// (C: xcb_randr_monitor_info_outputs).
    pub outputs: Vec<u32>,
}

/// Subset of `xcb_randr_get_output_info_reply_t`.
///
/// C: xcb_randr_get_output_info_reply_t (src/randr.c:497 handle_output and
/// src/randr.c:631 for the 1.5 output names)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputInfo {
    /// C: xcb_randr_get_output_info_name
    pub name: String,
    /// C: output->crtc — XCB_NONE when no CRTC is attached.
    pub crtc: u32,
}

/// XCB_CONFIG_WINDOW_* value list flag (subset).
/// C: xcb_config_window_t.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigWindow {
    X = 1,
    Y = 2,
    WIDTH = 4,
    HEIGHT = 8,
    BORDER_WIDTH = 16,
    SIBLING = 32,
    STACK_MODE = 64,
}

/// XCB_STACK_MODE_* (C: xcb_stack_mode_t).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackMode {
    ABOVE = 0,
    BELOW = 1,
    TOP_IF = 2,
    BOTTOM_IF = 3,
    OPPOSITE = 4,
}

/// XCB_WINDOW_CLASS (C: xcb_window_class_t).
pub const XCB_WINDOW_CLASS_INPUT_OUTPUT: u16 = 1;
pub const XCB_WINDOW_CLASS_INPUT_ONLY: u16 = 2;
/// C: XCB_COPY_FROM_PARENT.
pub const XCB_COPY_FROM_PARENT: u32 = 0;
/// C: XCB_EVENT_MASK constants used by the X11 layer.
///
/// The values are the X11 protocol bit numbers (include/xproto.h XCB_EVENT_MASK_*);
/// they are not arbitrary: a wrong bit silently subscribes to a different
/// event (e.g. PROPERTY_CHANGE at 1<<18 would subscribe ResizeRedirect instead,
/// so property updates from clients were never delivered).
pub const XCB_EVENT_MASK_PROPERTY_CHANGE: u32 = 1 << 22;
pub const XCB_EVENT_MASK_STRUCTURE_NOTIFY: u32 = 1 << 17;
pub const XCB_EVENT_MASK_ENTER_WINDOW: u32 = 1 << 4;
pub const XCB_EVENT_MASK_LEAVE_WINDOW: u32 = 1 << 5;
pub const XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT: u32 = 1 << 20;
pub const XCB_EVENT_MASK_SUBSTRUCTURE_NOTIFY: u32 = 1 << 19;
pub const XCB_EVENT_MASK_BUTTON_PRESS: u32 = 1 << 2;
pub const XCB_EVENT_MASK_BUTTON_RELEASE: u32 = 1 << 3;
pub const XCB_EVENT_MASK_POINTER_MOTION: u32 = 1 << 6;
pub const XCB_EVENT_MASK_EXPOSURE: u32 = 1 << 15;
pub const XCB_EVENT_MASK_FOCUS_CHANGE: u32 = 1 << 21;
/// C: XCB_RANDR_NOTIFY_MASK_* (selected in src/randr.c:1145).
pub const XCB_RANDR_NOTIFY_MASK_SCREEN_CHANGE: u32 = 1 << 0;
pub const XCB_RANDR_NOTIFY_MASK_CRTC_CHANGE: u32 = 1 << 1;
pub const XCB_RANDR_NOTIFY_MASK_OUTPUT_CHANGE: u32 = 1 << 2;
pub const XCB_RANDR_NOTIFY_MASK_OUTPUT_PROPERTY: u32 = 1 << 3;

/// C: XCB_CW_* subset.
pub const XCB_CW_EVENT_MASK: u32 = 1 << 11;
pub const XCB_CW_CURSOR: u32 = 1 << 14;
pub const XCB_CW_OVERRIDE_REDIRECT: u32 = 1 << 9;
pub const XCB_CW_COLORMAP: u32 = 1 << 13;
pub const XCB_CW_BORDER_PIXEL: u32 = 1 << 3;
pub const XCB_CW_BACK_PIXEL: u32 = 1 << 1;

/// C: XCB_ALLOW_* (xcb_allow_events modes).
pub const XCB_ALLOW_ASYNC_POINTER: u8 = 0;
pub const XCB_ALLOW_SYNC_POINTER: u8 = 1;
pub const XCB_ALLOW_REPLAY_POINTER: u8 = 2;
/// C: XCB_GRAB_MODE_*.
pub const XCB_GRAB_MODE_SYNC: u8 = 0;
pub const XCB_GRAB_MODE_ASYNC: u8 = 1;
/// C: XCB_GRAB_STATUS_SUCCESS.
pub const XCB_GRAB_STATUS_SUCCESS: u8 = 0;
/// C: XCB_CIRCULATE_RAISE_LOWEST / XCB_CIRCULATE_RAISE_TOP.
pub const XCB_CIRCULATE_RAISE_LOWEST: u8 = 0;
pub const XCB_CIRCULATE_RAISE_TOP: u8 = 1;
/// C: XCB_BUTTON_INDEX_ANY / XCB_BUTTON_CLICK_* / XCB_BUTTON_SCROLL_*.
pub const XCB_BUTTON_INDEX_ANY: u8 = 0;
pub const XCB_BUTTON_CLICK_LEFT: u8 = 1;
pub const XCB_BUTTON_CLICK_MIDDLE: u8 = 2;
pub const XCB_BUTTON_CLICK_RIGHT: u8 = 3;
pub const XCB_BUTTON_SCROLL_UP: u8 = 4;
pub const XCB_BUTTON_SCROLL_DOWN: u8 = 5;
pub const XCB_BUTTON_SCROLL_LEFT: u8 = 6;
pub const XCB_BUTTON_SCROLL_RIGHT: u8 = 7;
/// C: XCB_KEY_BUT_MASK_SHIFT.
pub const XCB_KEY_BUT_MASK_SHIFT: u16 = 1 << 0;
/// C: XCB_CURRENT_TIME.
pub const XCB_CURRENT_TIME: u32 = 0;

/// The X11 connection abstraction: every X request the event/X layer makes,
/// expressed as a method so it can be recorded in tests (RecordingConn) and
/// served by x11rb's RustConnection in xcb_util.rs.
///
/// C: xcb_connection_t (the subset of requests i3 issues).
#[allow(unused_variables)]
pub trait XConn {
    /// C: xcb_intern_atom.
    fn intern_atom(&mut self, name: &str) -> Option<u32> {
        None
    }
    /// C: xcb_get_atom_name.
    fn atom_name(&mut self, atom: u32) -> Option<String> {
        None
    }
    /// C: xcb_get_property.
    fn get_property(&mut self, window: u32, property: u32, type_: u32, long_offset: u32, long_length: u32) -> Option<PropertyReply> {
        None
    }
    /// C: xcb_change_property.
    fn change_property(&mut self, window: u32, property: u32, type_: u32, format: u8, mode: PropMode, data: &[u8]) {}
    /// C: xcb_delete_property.
    fn delete_property(&mut self, window: u32, property: u32) {}
    /// C: xcb_create_window.
    fn create_window(&mut self, depth: u16, window: u32, parent: u32, x: i16, y: i16, width: u16, height: u16, border_width: u16, class: u16, visual: u32, values: &[(u32, u32)]) {}

    /// C: xcb_create_colormap (x_con_init for non-root depths).
    fn create_colormap(&mut self, colormap: u32, window: u32, visual: u32) {}
    /// C: xcb_map_window.
    fn map_window(&mut self, window: u32) {}
    /// C: xcb_unmap_window.
    fn unmap_window(&mut self, window: u32) {}
    /// C: xcb_destroy_window.
    fn destroy_window(&mut self, window: u32) {}
    /// C: xcb_configure_window (values are (ConfigWindow flag, value) pairs;
    /// for STACK_MODE the value is a StackMode discriminant).
    fn configure_window(&mut self, window: u32, values: &[(ConfigWindow, u32)]) {}
    /// C: xcb_change_window_attributes (values are (CW flag, value) pairs).
    fn change_window_attributes(&mut self, window: u32, values: &[(u32, u32)]) {}
    /// C: xcb_reparent_window.
    fn reparent_window(&mut self, window: u32, parent: u32, x: i16, y: i16) {}
    /// C: xcb_send_event (32-bit client message data).
    fn send_client_message(&mut self, window: u32, propagate: bool, event_mask: u32, type_: u32, data32: [u32; 5]) {}
    /// C: xcb_send_event with a synthetic ConfigureNotify (the 32-byte event
    /// libi3/fake_configure_notify.c builds); i3bar's tray clients and i3's
    /// configure requests both need it so that the client believes it owns the
    /// geometry we gave it.
    fn send_configure_notify(
        &mut self,
        propagate: bool,
        destination: u32,
        event_mask: u32,
        event: &x11rb::protocol::xproto::ConfigureNotifyEvent,
    ) {
        let _ = (propagate, destination, event_mask, event);
    }
    /// C: xcb_kill_client.
    fn kill_client(&mut self, window: u32) {}
    /// C: xcb_set_selection_owner — owner XCB_NONE (0) releases the selection
    /// (used by i3bar's tray manager, i3bar/src/xcb.c:1540).
    fn set_selection_owner(&mut self, owner: u32, selection: u32, timestamp: u32) {}
    /// C: xcb_get_selection_owner. None is the X error case (the C callers
    /// treat a missing reply as fatal); XCB_NONE (0) means nobody owns the
    /// selection.
    fn get_selection_owner(&mut self, selection: u32) -> Option<u32> {
        None
    }
    /// C: xcb_grab_server / xcb_ungrab_server.
    fn grab_server(&mut self) {}
    fn ungrab_server(&mut self) {}
    /// C: xcb_flush / xcb_aux_sync.
    fn flush(&mut self) {}
    /// C: xcb_set_input_focus(conn, XCB_INPUT_FOCUS_POINTER_ROOT, window,
    /// timestamp) — the timestamp is the C global last_timestamp
    /// (src/x.c:1399/1452).
    fn set_input_focus(&mut self, window: u32, timestamp: u32) {}
    /// C: xcb_query_pointer — the pointer position relative to the root
    /// window (src/main.c:990).
    fn query_pointer(&mut self, window: u32) -> Option<PointerReply> {
        None
    }
    /// C: xcb_query_tree -- children of the given window.
    fn query_tree(&mut self, window: u32) -> Vec<u32> {
        Vec::new()
    }
    /// C: xcb_aux_find_visual_by_attrs(root_screen, -1, depth) — the first
    /// visual of the given depth, returned as (visual_id, depth).
    ///
    /// A negative screen_attr in the xcb-util helper means "match the depth"
    /// (i3bar/src/xcb.c:1354 asks for depth 32 to get the ARGB32 visual the
    /// transparent bar needs); None is the NULL the C code checks.
    fn find_visual_by_depth(&mut self, depth: u16) -> Option<(u32, u16)> {
        let _ = depth;
        None
    }
    /// C: xcb_get_window_attributes.
    fn get_window_attributes(&mut self, window: u32) -> Option<WindowAttrReply> {
        None
    }
    /// C: xcb_get_geometry.
    fn get_geometry(&mut self, window: u32) -> Option<GeometryReply> {
        None
    }
    /// C: xcb_change_save_set (mode: 0 = insert, 1 = delete).
    fn change_save_set(&mut self, mode: u8, window: u32) {}
    /// C: xcb_shape_select_input.
    fn shape_select_input(&mut self, window: u32, enable: bool) {}
    /// C: xcb_grab_button (buttons to grab on client windows).
    fn grab_buttons(&mut self, window: u32, buttons: &[u32]) {}
    /// C: xcb_grab_key (src/bindings.c:136 the GRAB_KEY macro and :177
    /// grab_all_keys). pointer_mode/keyboard_mode are XCB_GRAB_MODE_* values.
    fn grab_key(&mut self, owner_events: bool, grab_window: u32, modifiers: u16, key: u8, pointer_mode: u8, keyboard_mode: u8) {}
    /// C: xcb_ungrab_key (src/config.c:29 ungrab_all_keys).
    fn ungrab_key(&mut self, key: u8, grab_window: u32, modifiers: u16) {}
    /// C: xcb_ungrab_button (src/bindings.c:197 regrab_all_buttons).
    fn ungrab_button(&mut self, button: u8, grab_window: u32, modifiers: u16) {}
    /// C: the xcb_create_glyph_cursor() behind xcb_cursor_load_cursor()
    /// (src/xcursor.c xcursor_load_cursors loads the themed cursors once).
    /// Returns the cursor id, XCB_NONE (0) on failure.
    fn create_glyph_cursor(&mut self, glyph: u16) -> u32 {
        0
    }
    /// C: src/xcursor.c:50 xcursor_set_root_cursor (called from
    /// src/startup.c:213/254 with XCURSOR_CURSOR_WATCH / _POINTER).
    fn set_root_cursor(&mut self, cursor: u16) {}
    /// C: xcb_generate_id.
    fn generate_id(&mut self) -> u32 {
        0
    }
    /// The root screen (C: root_screen).
    fn root_screen(&mut self) -> ScreenInfo {
        ScreenInfo::default()
    }
    /// C: xcb_randr_query_version — (major, minor). None when the RandR
    /// extension is missing or the request failed (src/randr.c:1107 treats
    /// that as "RandR is not present").
    fn randr_query_version(&mut self) -> Option<(u32, u32)> {
        None
    }
    /// C: xcb_randr_get_screen_resources_current — (config_timestamp, outputs).
    /// A None reply is the X error case of src/randr.c:814.
    fn randr_get_screen_resources(&mut self, root: u32) -> Option<(u32, Vec<u32>)> {
        None
    }
    /// C: xcb_randr_get_monitors(root, true) — the RandR 1.5 monitor list.
    /// None is the X error case of src/randr.c:600.
    fn randr_get_monitors(&mut self, root: u32) -> Option<Vec<MonitorInfo>> {
        None
    }
    /// C: xcb_randr_get_output_info(output, config_timestamp).
    fn randr_get_output_info(&mut self, output: u32, config_timestamp: u32) -> Option<OutputInfo> {
        None
    }
    /// C: xcb_randr_get_crtc_info(crtc, config_timestamp).
    fn randr_get_crtc_info(&mut self, crtc: u32, config_timestamp: u32) -> Option<GeometryReply> {
        None
    }
    /// C: xcb_randr_get_output_primary — XCB_NONE when there is none.
    fn randr_get_output_primary(&mut self, root: u32) -> Option<u32> {
        None
    }
    /// C: xcb_randr_select_input — the XCB_RANDR_NOTIFY_MASK_* bitmask.
    fn randr_select_input(&mut self, root: u32, mask: u32) {}
    /// C: src/xinerama.c:110-121 — the Xinerama screens as (x, y, width,
    /// height) rects. None means "use the root output": either the Xinerama
    /// extension is missing or it reports itself as inactive.
    fn xinerama_screens(&mut self) -> Option<Vec<Rect>> {
        None
    }
    /// C: xcb_get_extension_data — (present, first_event) of an extension.
    /// None when the query itself failed.
    fn query_extension(&mut self, name: &str) -> Option<(bool, u8)> {
        None
    }

    /// C: xcb_grab_pointer — the reply status (XCB_GRAB_STATUS_SUCCESS on
    /// success). None is the X error case (no reply).
    fn grab_pointer(
        &mut self,
        owner_events: bool,
        grab_window: u32,
        event_mask: u32,
        pointer_mode: u8,
        keyboard_mode: u8,
        confine_to: u32,
        cursor: u32,
        time: u32,
    ) -> Option<u8> {
        None
    }
    /// C: xcb_grab_keyboard — the reply status.
    fn grab_keyboard(
        &mut self,
        owner_events: bool,
        grab_window: u32,
        time: u32,
        pointer_mode: u8,
        keyboard_mode: u8,
    ) -> Option<u8> {
        None
    }
    /// C: xcb_ungrab_pointer.
    fn ungrab_pointer(&mut self, time: u32) {}
    /// C: xcb_ungrab_keyboard.
    fn ungrab_keyboard(&mut self, time: u32) {}
    /// C: xcb_allow_events.
    fn allow_events(&mut self, mode: u8, timestamp: u32) {}
    /// C: xcb_warp_pointer.
    fn warp_pointer(
        &mut self,
        src_window: u32,
        dst_window: u32,
        src_x: i16,
        src_y: i16,
        src_width: u16,
        src_height: u16,
        dst_x: i16,
        dst_y: i16,
    ) {
    }
    /// C: xcb_circulate_window.
    fn circulate_window(&mut self, circulate: u8, window: u32) {}
    /// C: xcb_change_active_pointer_grab.
    fn change_active_pointer_grab(&mut self, cursor: u32, time: u32, event_mask: u32) {}
    /// C: xcb_poll_for_event — the next already-queued event, if any.
    ///
    /// The interactive drag loop (src/drag.c drain_drag_events) reads the X
    /// queue itself instead of going through the main loop; this is where it
    /// gets the events from.
    fn poll_for_event(&mut self) -> Option<x11rb::protocol::Event> {
        None
    }
    /// C: the libev xcb watcher blocking on the X file descriptor when the
    /// queue is empty (drain_drag_events is called again by the prepare
    /// watcher after libev wakes up).
    fn wait_for_event(&mut self) -> Option<x11rb::protocol::Event> {
        None
    }

    /// C: the global `conn` (xcb_connection_t *) handed to libxkbcommon-x11.
    /// None for the test doubles, which have no X server behind them.
    fn raw_xcb_connection(&mut self) -> Option<*mut std::ffi::c_void> {
        None
    }

    /// C: xcb_get_modifier_mapping — the 8 * keycodes_per_modifier keycodes of
    /// the modifier map, in Shift/Lock/Control/Mod1..Mod5 order (the layout
    /// libi3/get_mod_mask.c walks).
    fn get_modifier_mapping(&mut self) -> Option<Vec<u8>> {
        None
    }
}