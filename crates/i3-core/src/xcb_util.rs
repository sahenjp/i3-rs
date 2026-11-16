//! xcb.c: Helper functions for easier usage of XCB (1:1 port).
//!
//! C: src/xcb.c and the small X11 helpers of libi3 (libi3/xcb_*.c family).
//!
//! The X server is abstracted behind XConn (see backend.rs) so that the logic
//! here can be unit-tested with the recording RecordingConn; the real
//! implementation for x11rb's XCBConnection (C: the xcb_connection_t) is
//! RustXConn.

use crate::backend::{
    ConfigWindow, GeometryReply, MonitorInfo, OutputInfo, PointerReply, PropMode, PropertyReply,
    ScreenInfo, WindowAttrReply, XConn,
    XCB_CW_BACK_PIXEL, XCB_CW_BORDER_PIXEL, XCB_CW_COLORMAP, XCB_CW_CURSOR,
    XCB_CW_EVENT_MASK, XCB_CW_OVERRIDE_REDIRECT, XCB_COPY_FROM_PARENT, XCB_WINDOW_CLASS_INPUT_ONLY,
};
use crate::data::{Rect, XCB_ATOM_NONE};

/// C: src/xcb.c:12 'unsigned int xcb_numlock_mask;'
pub static mut xcb_numlock_mask: u32 = 0;

/// The atoms i3 interns, split in the two xmacro groups.
///
/// C: include/i3-atoms_NET_SUPPORTED.xmacro.h (40 atoms) and
///    include/i3-atoms_rest.xmacro.h (23 atoms).
pub const NET_SUPPORTED_ATOM_NAMES: &[&str] = &[
    "_NET_SUPPORTED",
    "_NET_SUPPORTING_WM_CHECK",
    "_NET_WM_NAME",
    "_NET_WM_VISIBLE_NAME",
    "_NET_WM_MOVERESIZE",
    "_NET_WM_STATE_STICKY",
    "_NET_WM_STATE_FULLSCREEN",
    "_NET_WM_STATE_DEMANDS_ATTENTION",
    "_NET_WM_STATE_MODAL",
    "_NET_WM_STATE_HIDDEN",
    "_NET_WM_STATE_FOCUSED",
    "_NET_WM_STATE_MAXIMIZED_VERT",
    "_NET_WM_STATE_MAXIMIZED_HORZ",
    "_NET_WM_STATE",
    "_NET_WM_WINDOW_TYPE",
    "_NET_WM_WINDOW_TYPE_NORMAL",
    "_NET_WM_WINDOW_TYPE_DOCK",
    "_NET_WM_WINDOW_TYPE_DIALOG",
    "_NET_WM_WINDOW_TYPE_UTILITY",
    "_NET_WM_WINDOW_TYPE_TOOLBAR",
    "_NET_WM_WINDOW_TYPE_SPLASH",
    "_NET_WM_WINDOW_TYPE_MENU",
    "_NET_WM_WINDOW_TYPE_DROPDOWN_MENU",
    "_NET_WM_WINDOW_TYPE_POPUP_MENU",
    "_NET_WM_WINDOW_TYPE_TOOLTIP",
    "_NET_WM_WINDOW_TYPE_NOTIFICATION",
    "_NET_WM_DESKTOP",
    "_NET_WM_STRUT_PARTIAL",
    "_NET_CLIENT_LIST",
    "_NET_CLIENT_LIST_STACKING",
    "_NET_CURRENT_DESKTOP",
    "_NET_NUMBER_OF_DESKTOPS",
    "_NET_DESKTOP_NAMES",
    "_NET_DESKTOP_VIEWPORT",
    "_NET_ACTIVE_WINDOW",
    "_NET_CLOSE_WINDOW",
    "_NET_MOVERESIZE_WINDOW",
];

/// C: include/i3-atoms_rest.xmacro.h
pub const REST_ATOM_NAMES: &[&str] = &[
    "_NET_WM_USER_TIME",
    "_NET_STARTUP_ID",
    "_NET_WORKAREA",
    "_NET_WM_ICON",
    "WM_PROTOCOLS",
    "WM_DELETE_WINDOW",
    "UTF8_STRING",
    "WM_STATE",
    "WM_CLIENT_LEADER",
    "WM_TAKE_FOCUS",
    "WM_WINDOW_ROLE",
    "I3_SOCKET_PATH",
    "I3_CONFIG_PATH",
    "I3_SYNC",
    "I3_SHMLOG_PATH",
    "I3_PID",
    "I3_LOG_STREAM_SOCKET_PATH",
    "I3_FLOATING_WINDOW",
    "_NET_REQUEST_FRAME_EXTENTS",
    "_NET_FRAME_EXTENTS",
    "_MOTIF_WM_HINTS",
    "WM_CHANGE_STATE",
    "MANAGER",
];

/// The interned atom table; field names are the C macro names (A_##atom).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Atoms {
    pub A__NET_SUPPORTED: u32,
    pub A__NET_SUPPORTING_WM_CHECK: u32,
    pub A__NET_WM_NAME: u32,
    pub A__NET_WM_VISIBLE_NAME: u32,
    pub A__NET_WM_MOVERESIZE: u32,
    pub A__NET_WM_STATE_STICKY: u32,
    pub A__NET_WM_STATE_FULLSCREEN: u32,
    pub A__NET_WM_STATE_DEMANDS_ATTENTION: u32,
    pub A__NET_WM_STATE_MODAL: u32,
    pub A__NET_WM_STATE_HIDDEN: u32,
    pub A__NET_WM_STATE_FOCUSED: u32,
    pub A__NET_WM_STATE_MAXIMIZED_VERT: u32,
    pub A__NET_WM_STATE_MAXIMIZED_HORZ: u32,
    pub A__NET_WM_STATE: u32,
    pub A__NET_WM_WINDOW_TYPE: u32,
    pub A__NET_WM_WINDOW_TYPE_NORMAL: u32,
    pub A__NET_WM_WINDOW_TYPE_DOCK: u32,
    pub A__NET_WM_WINDOW_TYPE_DIALOG: u32,
    pub A__NET_WM_WINDOW_TYPE_UTILITY: u32,
    pub A__NET_WM_WINDOW_TYPE_TOOLBAR: u32,
    pub A__NET_WM_WINDOW_TYPE_SPLASH: u32,
    pub A__NET_WM_WINDOW_TYPE_MENU: u32,
    pub A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU: u32,
    pub A__NET_WM_WINDOW_TYPE_POPUP_MENU: u32,
    pub A__NET_WM_WINDOW_TYPE_TOOLTIP: u32,
    pub A__NET_WM_WINDOW_TYPE_NOTIFICATION: u32,
    pub A__NET_WM_DESKTOP: u32,
    pub A__NET_WM_STRUT_PARTIAL: u32,
    pub A__NET_CLIENT_LIST: u32,
    pub A__NET_CLIENT_LIST_STACKING: u32,
    pub A__NET_CURRENT_DESKTOP: u32,
    pub A__NET_NUMBER_OF_DESKTOPS: u32,
    pub A__NET_DESKTOP_NAMES: u32,
    pub A__NET_DESKTOP_VIEWPORT: u32,
    pub A__NET_ACTIVE_WINDOW: u32,
    pub A__NET_CLOSE_WINDOW: u32,
    pub A__NET_MOVERESIZE_WINDOW: u32,
    pub A__NET_WM_USER_TIME: u32,
    pub A__NET_STARTUP_ID: u32,
    pub A__NET_WORKAREA: u32,
    pub A__NET_WM_ICON: u32,
    pub A_WM_PROTOCOLS: u32,
    pub A_WM_DELETE_WINDOW: u32,
    pub A_UTF8_STRING: u32,
    pub A_WM_STATE: u32,
    pub A_WM_CLIENT_LEADER: u32,
    pub A_WM_TAKE_FOCUS: u32,
    pub A_WM_WINDOW_ROLE: u32,
    pub A_I3_SOCKET_PATH: u32,
    pub A_I3_CONFIG_PATH: u32,
    pub A_I3_SYNC: u32,
    pub A_I3_SHMLOG_PATH: u32,
    pub A_I3_PID: u32,
    pub A_I3_LOG_STREAM_SOCKET_PATH: u32,
    pub A_I3_FLOATING_WINDOW: u32,
    pub A__NET_REQUEST_FRAME_EXTENTS: u32,
    pub A__NET_FRAME_EXTENTS: u32,
    pub A__MOTIF_WM_HINTS: u32,
    pub A_WM_CHANGE_STATE: u32,
    pub A_MANAGER: u32,
}

/// The pairs (atom macro name, atom string) in declaration order.
pub const ATOM_TABLE: &[(&str, &str)] = &[
    ("A__NET_SUPPORTED", "_NET_SUPPORTED"),
    ("A__NET_SUPPORTING_WM_CHECK", "_NET_SUPPORTING_WM_CHECK"),
    ("A__NET_WM_NAME", "_NET_WM_NAME"),
    ("A__NET_WM_VISIBLE_NAME", "_NET_WM_VISIBLE_NAME"),
    ("A__NET_WM_MOVERESIZE", "_NET_WM_MOVERESIZE"),
    ("A__NET_WM_STATE_STICKY", "_NET_WM_STATE_STICKY"),
    ("A__NET_WM_STATE_FULLSCREEN", "_NET_WM_STATE_FULLSCREEN"),
    ("A__NET_WM_STATE_DEMANDS_ATTENTION", "_NET_WM_STATE_DEMANDS_ATTENTION"),
    ("A__NET_WM_STATE_MODAL", "_NET_WM_STATE_MODAL"),
    ("A__NET_WM_STATE_HIDDEN", "_NET_WM_STATE_HIDDEN"),
    ("A__NET_WM_STATE_FOCUSED", "_NET_WM_STATE_FOCUSED"),
    ("A__NET_WM_STATE_MAXIMIZED_VERT", "_NET_WM_STATE_MAXIMIZED_VERT"),
    ("A__NET_WM_STATE_MAXIMIZED_HORZ", "_NET_WM_STATE_MAXIMIZED_HORZ"),
    ("A__NET_WM_STATE", "_NET_WM_STATE"),
    ("A__NET_WM_WINDOW_TYPE", "_NET_WM_WINDOW_TYPE"),
    ("A__NET_WM_WINDOW_TYPE_NORMAL", "_NET_WM_WINDOW_TYPE_NORMAL"),
    ("A__NET_WM_WINDOW_TYPE_DOCK", "_NET_WM_WINDOW_TYPE_DOCK"),
    ("A__NET_WM_WINDOW_TYPE_DIALOG", "_NET_WM_WINDOW_TYPE_DIALOG"),
    ("A__NET_WM_WINDOW_TYPE_UTILITY", "_NET_WM_WINDOW_TYPE_UTILITY"),
    ("A__NET_WM_WINDOW_TYPE_TOOLBAR", "_NET_WM_WINDOW_TYPE_TOOLBAR"),
    ("A__NET_WM_WINDOW_TYPE_SPLASH", "_NET_WM_WINDOW_TYPE_SPLASH"),
    ("A__NET_WM_WINDOW_TYPE_MENU", "_NET_WM_WINDOW_TYPE_MENU"),
    ("A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU", "_NET_WM_WINDOW_TYPE_DROPDOWN_MENU"),
    ("A__NET_WM_WINDOW_TYPE_POPUP_MENU", "_NET_WM_WINDOW_TYPE_POPUP_MENU"),
    ("A__NET_WM_WINDOW_TYPE_TOOLTIP", "_NET_WM_WINDOW_TYPE_TOOLTIP"),
    ("A__NET_WM_WINDOW_TYPE_NOTIFICATION", "_NET_WM_WINDOW_TYPE_NOTIFICATION"),
    ("A__NET_WM_DESKTOP", "_NET_WM_DESKTOP"),
    ("A__NET_WM_STRUT_PARTIAL", "_NET_WM_STRUT_PARTIAL"),
    ("A__NET_CLIENT_LIST", "_NET_CLIENT_LIST"),
    ("A__NET_CLIENT_LIST_STACKING", "_NET_CLIENT_LIST_STACKING"),
    ("A__NET_CURRENT_DESKTOP", "_NET_CURRENT_DESKTOP"),
    ("A__NET_NUMBER_OF_DESKTOPS", "_NET_NUMBER_OF_DESKTOPS"),
    ("A__NET_DESKTOP_NAMES", "_NET_DESKTOP_NAMES"),
    ("A__NET_DESKTOP_VIEWPORT", "_NET_DESKTOP_VIEWPORT"),
    ("A__NET_ACTIVE_WINDOW", "_NET_ACTIVE_WINDOW"),
    ("A__NET_CLOSE_WINDOW", "_NET_CLOSE_WINDOW"),
    ("A__NET_MOVERESIZE_WINDOW", "_NET_MOVERESIZE_WINDOW"),
    ("A__NET_WM_USER_TIME", "_NET_WM_USER_TIME"),
    ("A__NET_STARTUP_ID", "_NET_STARTUP_ID"),
    ("A__NET_WORKAREA", "_NET_WORKAREA"),
    ("A__NET_WM_ICON", "_NET_WM_ICON"),
    ("A_WM_PROTOCOLS", "WM_PROTOCOLS"),
    ("A_WM_DELETE_WINDOW", "WM_DELETE_WINDOW"),
    ("A_UTF8_STRING", "UTF8_STRING"),
    ("A_WM_STATE", "WM_STATE"),
    ("A_WM_CLIENT_LEADER", "WM_CLIENT_LEADER"),
    ("A_WM_TAKE_FOCUS", "WM_TAKE_FOCUS"),
    ("A_WM_WINDOW_ROLE", "WM_WINDOW_ROLE"),
    ("A_I3_SOCKET_PATH", "I3_SOCKET_PATH"),
    ("A_I3_CONFIG_PATH", "I3_CONFIG_PATH"),
    ("A_I3_SYNC", "I3_SYNC"),
    ("A_I3_SHMLOG_PATH", "I3_SHMLOG_PATH"),
    ("A_I3_PID", "I3_PID"),
    ("A_I3_LOG_STREAM_SOCKET_PATH", "I3_LOG_STREAM_SOCKET_PATH"),
    ("A_I3_FLOATING_WINDOW", "I3_FLOATING_WINDOW"),
    ("A__NET_REQUEST_FRAME_EXTENTS", "_NET_REQUEST_FRAME_EXTENTS"),
    ("A__NET_FRAME_EXTENTS", "_NET_FRAME_EXTENTS"),
    ("A__MOTIF_WM_HINTS", "_MOTIF_WM_HINTS"),
    ("A_WM_CHANGE_STATE", "WM_CHANGE_STATE"),
    ("A_MANAGER", "MANAGER"),
];

impl Atoms {
    /// Intern all atoms in declaration order (C: src/main.c -- the two xmacro
    /// loops interning every atom at startup). Atoms that cannot be interned
    /// (no X server) keep 0 == XCB_ATOM_NONE.
    pub fn intern_all(x: &mut dyn XConn) -> Atoms {
        let mut atoms = Atoms::default();
        for (field, name) in ATOM_TABLE {
            let v = x.intern_atom(name).unwrap_or(XCB_ATOM_NONE);
            match *field {
                "A__NET_SUPPORTED" => atoms.A__NET_SUPPORTED = v,
                "A__NET_SUPPORTING_WM_CHECK" => atoms.A__NET_SUPPORTING_WM_CHECK = v,
                "A__NET_WM_NAME" => atoms.A__NET_WM_NAME = v,
                "A__NET_WM_VISIBLE_NAME" => atoms.A__NET_WM_VISIBLE_NAME = v,
                "A__NET_WM_MOVERESIZE" => atoms.A__NET_WM_MOVERESIZE = v,
                "A__NET_WM_STATE_STICKY" => atoms.A__NET_WM_STATE_STICKY = v,
                "A__NET_WM_STATE_FULLSCREEN" => atoms.A__NET_WM_STATE_FULLSCREEN = v,
                "A__NET_WM_STATE_DEMANDS_ATTENTION" => atoms.A__NET_WM_STATE_DEMANDS_ATTENTION = v,
                "A__NET_WM_STATE_MODAL" => atoms.A__NET_WM_STATE_MODAL = v,
                "A__NET_WM_STATE_HIDDEN" => atoms.A__NET_WM_STATE_HIDDEN = v,
                "A__NET_WM_STATE_FOCUSED" => atoms.A__NET_WM_STATE_FOCUSED = v,
                "A__NET_WM_STATE_MAXIMIZED_VERT" => atoms.A__NET_WM_STATE_MAXIMIZED_VERT = v,
                "A__NET_WM_STATE_MAXIMIZED_HORZ" => atoms.A__NET_WM_STATE_MAXIMIZED_HORZ = v,
                "A__NET_WM_STATE" => atoms.A__NET_WM_STATE = v,
                "A__NET_WM_WINDOW_TYPE" => atoms.A__NET_WM_WINDOW_TYPE = v,
                "A__NET_WM_WINDOW_TYPE_NORMAL" => atoms.A__NET_WM_WINDOW_TYPE_NORMAL = v,
                "A__NET_WM_WINDOW_TYPE_DOCK" => atoms.A__NET_WM_WINDOW_TYPE_DOCK = v,
                "A__NET_WM_WINDOW_TYPE_DIALOG" => atoms.A__NET_WM_WINDOW_TYPE_DIALOG = v,
                "A__NET_WM_WINDOW_TYPE_UTILITY" => atoms.A__NET_WM_WINDOW_TYPE_UTILITY = v,
                "A__NET_WM_WINDOW_TYPE_TOOLBAR" => atoms.A__NET_WM_WINDOW_TYPE_TOOLBAR = v,
                "A__NET_WM_WINDOW_TYPE_SPLASH" => atoms.A__NET_WM_WINDOW_TYPE_SPLASH = v,
                "A__NET_WM_WINDOW_TYPE_MENU" => atoms.A__NET_WM_WINDOW_TYPE_MENU = v,
                "A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU" => atoms.A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU = v,
                "A__NET_WM_WINDOW_TYPE_POPUP_MENU" => atoms.A__NET_WM_WINDOW_TYPE_POPUP_MENU = v,
                "A__NET_WM_WINDOW_TYPE_TOOLTIP" => atoms.A__NET_WM_WINDOW_TYPE_TOOLTIP = v,
                "A__NET_WM_WINDOW_TYPE_NOTIFICATION" => atoms.A__NET_WM_WINDOW_TYPE_NOTIFICATION = v,
                "A__NET_WM_DESKTOP" => atoms.A__NET_WM_DESKTOP = v,
                "A__NET_WM_STRUT_PARTIAL" => atoms.A__NET_WM_STRUT_PARTIAL = v,
                "A__NET_CLIENT_LIST" => atoms.A__NET_CLIENT_LIST = v,
                "A__NET_CLIENT_LIST_STACKING" => atoms.A__NET_CLIENT_LIST_STACKING = v,
                "A__NET_CURRENT_DESKTOP" => atoms.A__NET_CURRENT_DESKTOP = v,
                "A__NET_NUMBER_OF_DESKTOPS" => atoms.A__NET_NUMBER_OF_DESKTOPS = v,
                "A__NET_DESKTOP_NAMES" => atoms.A__NET_DESKTOP_NAMES = v,
                "A__NET_DESKTOP_VIEWPORT" => atoms.A__NET_DESKTOP_VIEWPORT = v,
                "A__NET_ACTIVE_WINDOW" => atoms.A__NET_ACTIVE_WINDOW = v,
                "A__NET_CLOSE_WINDOW" => atoms.A__NET_CLOSE_WINDOW = v,
                "A__NET_MOVERESIZE_WINDOW" => atoms.A__NET_MOVERESIZE_WINDOW = v,
                "A__NET_WM_USER_TIME" => atoms.A__NET_WM_USER_TIME = v,
                "A__NET_STARTUP_ID" => atoms.A__NET_STARTUP_ID = v,
                "A__NET_WORKAREA" => atoms.A__NET_WORKAREA = v,
                "A__NET_WM_ICON" => atoms.A__NET_WM_ICON = v,
                "A_WM_PROTOCOLS" => atoms.A_WM_PROTOCOLS = v,
                "A_WM_DELETE_WINDOW" => atoms.A_WM_DELETE_WINDOW = v,
                "A_UTF8_STRING" => atoms.A_UTF8_STRING = v,
                "A_WM_STATE" => atoms.A_WM_STATE = v,
                "A_WM_CLIENT_LEADER" => atoms.A_WM_CLIENT_LEADER = v,
                "A_WM_TAKE_FOCUS" => atoms.A_WM_TAKE_FOCUS = v,
                "A_WM_WINDOW_ROLE" => atoms.A_WM_WINDOW_ROLE = v,
                "A_I3_SOCKET_PATH" => atoms.A_I3_SOCKET_PATH = v,
                "A_I3_CONFIG_PATH" => atoms.A_I3_CONFIG_PATH = v,
                "A_I3_SYNC" => atoms.A_I3_SYNC = v,
                "A_I3_SHMLOG_PATH" => atoms.A_I3_SHMLOG_PATH = v,
                "A_I3_PID" => atoms.A_I3_PID = v,
                "A_I3_LOG_STREAM_SOCKET_PATH" => atoms.A_I3_LOG_STREAM_SOCKET_PATH = v,
                "A_I3_FLOATING_WINDOW" => atoms.A_I3_FLOATING_WINDOW = v,
                "A__NET_REQUEST_FRAME_EXTENTS" => atoms.A__NET_REQUEST_FRAME_EXTENTS = v,
                "A__NET_FRAME_EXTENTS" => atoms.A__NET_FRAME_EXTENTS = v,
                "A__MOTIF_WM_HINTS" => atoms.A__MOTIF_WM_HINTS = v,
                "A_WM_CHANGE_STATE" => atoms.A_WM_CHANGE_STATE = v,
                "A_MANAGER" => atoms.A_MANAGER = v,
                _ => {}
            }
        }
        atoms
    }
}

/// Convenience wrapper around xcb_create_window which takes care of depth,
/// generating an ID and checking for errors.
///
/// C: src/xcb.c:19 create_window
pub fn create_window(
    x: &mut dyn XConn,
    dims: Rect,
    depth: u16,
    visual: u32,
    window_class: u16,
    cursor: xcursor_cursor_t,
    map: bool,
    values: &[(u32, u32)],
) -> u32 {
    let result = x.generate_id();

    // If the window class is XCB_WINDOW_CLASS_INPUT_ONLY, we copy depth and
    // visual id from the parent window.
    let (depth, visual) = if window_class == XCB_WINDOW_CLASS_INPUT_ONLY {
        (XCB_COPY_FROM_PARENT as u16, XCB_COPY_FROM_PARENT)
    } else {
        (depth, visual)
    };

    let root_window = x.root_screen().root;
    x.create_window(
        depth,
        result,
        root_window, /* parent == root */
        dims.x as i16,
        dims.y as i16,
        dims.width as u16,
        dims.height as u16,
        0, /* border = 0, we draw our own */
        window_class,
        visual,
        values,
    );
    // C: xcb_request_check + ELOG on error (errors surface per-request in x11rb).

    // Set the cursor
    let cursor_values = [(XCB_CW_CURSOR, xcursor_get_cursor(x, cursor))];
    x.change_window_attributes(result, &cursor_values);

    // Map the window (= make it visible)
    if map {
        x.map_window(result);
    }

    result
}

/// Generates a configure_notify_event with absolute coordinates (relative to
/// the X root window, not to the client's frame) for the given client.
///
/// C: src/xcb.c:64 fake_absolute_configure_notify
pub fn fake_absolute_configure_notify(
    t: &crate::tree::Tree,
    x: &mut dyn XConn,
    con: crate::data::ConId,
) {
    use crate::con::con_has_managed_window;
    if !con_has_managed_window(t, con) {
        return;
    }
    let c = t.con(con);
    let window = c.window.expect("con_has_managed_window implies window");
    let w = t.window(window);
    let absolute = Rect::new(
        c.rect.x + c.window_rect.x,
        c.rect.y + c.window_rect.y,
        c.window_rect.width,
        c.window_rect.height,
    );
    // C: fake_configure_notify(conn, absolute, con->window->id, con->border_width)
    //    -- libi3/fake_configure_notify.c owns the event and the request; the
    //    connection is i3's XConn, wrapped so libi3 can issue it.
    let rectangle = x11rb::protocol::xproto::Rectangle {
        x: absolute.x as i16,
        y: absolute.y as i16,
        width: absolute.width as u16,
        height: absolute.height as u16,
    };
    libi3::fake_configure_notify::fake_configure_notify(
        &mut Libi3Conn(x),
        rectangle,
        w.id,
        c.border_width,
    );
}

/// Sends the WM_TAKE_FOCUS ClientMessage to the given window.
///
/// C: src/xcb.c:84 send_take_focus
pub fn send_take_focus(x: &mut dyn XConn, a: &Atoms, window: u32, timestamp: u32) {
    x.send_client_message(window, false, 0, a.A_WM_PROTOCOLS, [
        a.A_WM_TAKE_FOCUS,
        timestamp,
        0,
        0,
        0,
    ]);
}

/// Configures the given window to have the size/position specified by rect.
///
/// C: src/xcb.c:107 xcb_set_window_rect
pub fn xcb_set_window_rect(x: &mut dyn XConn, window: u32, r: Rect) {
    x.configure_window(
        window,
        &[
            (ConfigWindow::X, r.x as u32),
            (ConfigWindow::Y, r.y as u32),
            (ConfigWindow::WIDTH, r.width as u32),
            (ConfigWindow::HEIGHT, r.height as u32),
        ],
    );
    // C: src/xcb.c:115 add_ignore_event(ENSURE_FULL_SEQUENCE(cookie.sequence),
    // -1) -- RustXConn::configure_window already registers the sequence, so
    // nothing is left to do here for a real connection.
}

/// Returns the first supported _NET_WM_WINDOW_TYPE atom.
///
/// C: src/xcb.c:122 xcb_get_preferred_window_type
pub fn xcb_get_preferred_window_type(a: &Atoms, reply: Option<&PropertyReply>) -> u32 {
    let reply = match reply {
        Some(r) if !r.value.is_empty() => r,
        _ => return XCB_ATOM_NONE,
    };
    for atom in reply.words32() {
        if atom == a.A__NET_WM_WINDOW_TYPE_NORMAL
            || atom == a.A__NET_WM_WINDOW_TYPE_DIALOG
            || atom == a.A__NET_WM_WINDOW_TYPE_UTILITY
            || atom == a.A__NET_WM_WINDOW_TYPE_TOOLBAR
            || atom == a.A__NET_WM_WINDOW_TYPE_SPLASH
            || atom == a.A__NET_WM_WINDOW_TYPE_MENU
            || atom == a.A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU
            || atom == a.A__NET_WM_WINDOW_TYPE_POPUP_MENU
            || atom == a.A__NET_WM_WINDOW_TYPE_TOOLTIP
            || atom == a.A__NET_WM_WINDOW_TYPE_NOTIFICATION
        {
            return atom;
        }
    }
    XCB_ATOM_NONE
}

/// Returns true if the given reply contains the given atom.
///
/// C: src/xcb.c:154 xcb_reply_contains_atom
pub fn xcb_reply_contains_atom(prop: Option<&PropertyReply>, atom: u32) -> bool {
    let prop = match prop {
        Some(p) if !p.value.is_empty() => p,
        _ => return false,
    };
    prop.words32().contains(&atom)
}

/// C: xcb_visualtype_t (subset kept 1:1 with the fields the draw layer needs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisualType {
    pub visual_id: u32,
    pub class: u8,
    pub bits_per_rgb_value: u8,
    pub colormap_entries: u16,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
}

/// Get visual type specified by visualid.
///
/// C: src/xcb.c:177 get_visualtype_by_id
/// TODO(Phase 7): full depth iteration once the XConn setup is exposed.
pub fn get_visualtype_by_id(x: &mut dyn XConn, visual_id: u32) -> Option<VisualType> {
    let screen = x.root_screen();
    if screen.root_visual == visual_id {
        return Some(VisualType {
            visual_id,
            class: 4, /* TrueColor */
            bits_per_rgb_value: 8,
            colormap_entries: 256,
            red_mask: 0xff0000,
            green_mask: 0x00ff00,
            blue_mask: 0x0000ff,
        });
    }
    None
}

/// Get visualid with specified depth.
///
/// C: src/xcb.c:198 get_visualid_by_depth
pub fn get_visualid_by_depth(x: &mut dyn XConn, depth: u16) -> u32 {
    let screen = x.root_screen();
    if screen.root_depth == depth {
        return screen.root_visual;
    }
    0
}

/// C: XCB_ATOM_ATOM (the predefined atom type of a list of atoms).
const XCB_ATOM_ATOM: u32 = 4;

/// Add an atom to a list of atoms the given property defines.
///
/// C: src/xcb.c:221 xcb_add_property_atom
pub fn xcb_add_property_atom(x: &mut dyn XConn, window: u32, property: u32, atom: u32) {
    let data = atom.to_le_bytes();
    // C: XCB_ATOM_ATOM (4) — 6 is XCB_ATOM_CARDINAL; the wrong type made
    // _NET_WM_STATE unreadable for EWMH clients and xprop.
    x.change_property(window, property, XCB_ATOM_ATOM, 32, PropMode::APPEND, &data);
}

/// Remove an atom from a list of atoms the given property defines without
/// removing any other potentially set atoms.
///
/// C: src/xcb.c:231 xcb_remove_property_atom
pub fn xcb_remove_property_atom(x: &mut dyn XConn, window: u32, property: u32, atom: u32) {
    x.grab_server();

    let reply = x.get_property(window, property, 0 /* XCB_GET_PROPERTY_TYPE_ANY */, 0, 4096);
    let keep: Vec<u32> = match reply {
        Some(r) if !r.value.is_empty() => r.words32().into_iter().filter(|&a| a != atom).collect(),
        _ => {
            x.ungrab_server();
            return;
        }
    };

    let mut data = Vec::with_capacity(keep.len() * 4);
    for w in &keep {
        data.extend_from_slice(&w.to_le_bytes());
    }
    // C: XCB_ATOM_ATOM (4), see xcb_add_property_atom.
    x.change_property(window, property, XCB_ATOM_ATOM, 32, PropMode::REPLACE, &data);
    x.ungrab_server();
}

/// C: include/xcursor.h 'enum xcursor_cursor_t'
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum xcursor_cursor_t {
    XCURSOR_CURSOR_POINTER = 0,
    XCURSOR_CURSOR_RESIZE_HORIZONTAL,
    XCURSOR_CURSOR_RESIZE_VERTICAL,
    XCURSOR_CURSOR_TOP_LEFT_CORNER,
    XCURSOR_CURSOR_TOP_RIGHT_CORNER,
    XCURSOR_CURSOR_BOTTOM_LEFT_CORNER,
    XCURSOR_CURSOR_BOTTOM_RIGHT_CORNER,
    XCURSOR_CURSOR_MOVE,
    XCURSOR_CURSOR_WATCH,
}

/// C: src/xcursor.c:9 -- the cursor names, order matching xcursor_cursor_t.
pub const XCURSOR_NAMES: &[&str] = &[
    "left_ptr",
    "sb_h_double_arrow",
    "sb_v_double_arrow",
    "top_left_corner",
    "top_right_corner",
    "bottom_left_corner",
    "bottom_right_corner",
    "fleur",
    "watch",
];

/// C: X11/cursorfont.h glyph numbers of the cursors src/xcursor.c loads by
/// name (left_ptr, sb_h_double_arrow, …).
fn cursor_glyph(cursor: xcursor_cursor_t) -> u16 {
    match cursor {
        xcursor_cursor_t::XCURSOR_CURSOR_POINTER => 68,            // left_ptr
        xcursor_cursor_t::XCURSOR_CURSOR_RESIZE_HORIZONTAL => 108,  // sb_h_double_arrow
        xcursor_cursor_t::XCURSOR_CURSOR_RESIZE_VERTICAL => 116,    // sb_v_double_arrow
        xcursor_cursor_t::XCURSOR_CURSOR_TOP_LEFT_CORNER => 134,
        xcursor_cursor_t::XCURSOR_CURSOR_TOP_RIGHT_CORNER => 136,
        xcursor_cursor_t::XCURSOR_CURSOR_BOTTOM_LEFT_CORNER => 12,
        xcursor_cursor_t::XCURSOR_CURSOR_BOTTOM_RIGHT_CORNER => 14,
        xcursor_cursor_t::XCURSOR_CURSOR_WATCH => 150,
        xcursor_cursor_t::XCURSOR_CURSOR_MOVE => 52,                // fleur
    }
}

thread_local! {
    /// C: src/xcursor.c:20 static xcb_cursor_t cursors[XCURSOR_CURSOR_MAX] —
    /// every cursor is created once and reused.
    static CURSORS: std::cell::RefCell<[u32; 9]> = const { std::cell::RefCell::new([0; 9]) };
}

/// C: src/xcursor.c:39 xcursor_get_cursor -- the X11 cursor id.
///
/// The id is XCB_NONE (0) when the cursor could not be created; passing that
/// to XCB_CW_CURSOR / xcb_grab_pointer is valid ("no cursor") but an *invalid*
/// id (the old enum-discriminant stub) makes every request fail with
/// BadCursor (error 6), which is what broke the drag/resize pointer grabs.
pub fn xcursor_get_cursor(x: &mut dyn XConn, cursor: xcursor_cursor_t) -> u32 {
    let idx = cursor as usize;
    let cached = CURSORS.with(|c| c.borrow()[idx]);
    if cached != 0 {
        return cached;
    }
    let id = x.create_glyph_cursor(cursor_glyph(cursor));
    CURSORS.with(|c| c.borrow_mut()[idx] = id);
    id
}

/****************************************************************************\
 * x11rb implementation of XConn (real server side)
\****************************************************************************/

/// The X connection type of the process.
///
/// C: xcb_connection_t. The port uses x11rb's xcb_ffi connection instead of
/// the pure-Rust one because libxkbcommon-x11 (xkb_x11_setup_xkb_extension,
/// xkb_x11_keymap_new_from_device) takes a `struct xcb_connection_t *`, which
/// only the xcb_ffi connection can hand out (get_raw_xcb_connection()).
pub type XConnection = x11rb::xcb_ffi::XCBConnection;

/// The shared handle onto the one connection (C: the global `conn` of
/// src/main.c:118). Every layer gets this same Rc, never a second connection.
pub type SharedConnection = std::rc::Rc<XConnection>;

/// C: src/main.c:578 conn = xcb_connect(NULL, &conn_screen);
///
/// xcb_connect(NULL, …) parses $DISPLAY, which is what
/// XCBConnection::connect(None) does.
pub fn connect() -> Result<(SharedConnection, usize), String> {
    let (conn, screen) = XConnection::connect(None).map_err(|e| e.to_string())?;
    Ok((std::rc::Rc::new(conn), screen))
}

/// x11rb XCBConnection-backed XConn implementation.
///
/// C: xcb_connection_t via libxcb; here x11rb 0.13.
pub struct RustXConn {
    /// C: the single xcb_connection_t of the process.
    ///
    /// C has exactly one connection, and that is load-bearing: xcb_grab_server
    /// (used by manage_existing_windows) freezes every *other* client, so a
    /// second connection would deadlock its own WM. The port therefore shares
    /// the one connection instead of opening another one.
    pub conn: SharedConnection,
    pub screen_idx: usize,
}

impl RustXConn {
    /// C: main.c (xcb_connect + xcb_get_setup + setup.roots iteration).
    /// Returns None if no DISPLAY is available (same as C exiting with
    /// "Cannot open display").
    pub fn connect() -> Option<RustXConn> {
        let (conn, screen_idx) = connect().ok()?;
        Some(RustXConn { conn, screen_idx })
    }

    /// C: the xcb_connection_t opened by main() is the only one; wrap it so the
    /// X11 layer and the startup sequence speak over the same connection.
    pub fn wrap(conn: SharedConnection, screen_idx: usize) -> RustXConn {
        RustXConn { conn, screen_idx }
    }
}

impl XConn for RustXConn {
    fn intern_atom(&mut self, name: &str) -> Option<u32> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        self.conn
            .intern_atom(false, name.as_bytes())
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
    }

    fn atom_name(&mut self, atom: u32) -> Option<String> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        self.conn
            .get_atom_name(atom)
            .ok()?
            .reply()
            .ok()
            .map(|r| String::from_utf8_lossy(&r.name).into_owned())
    }

    fn get_property(&mut self, window: u32, property: u32, type_: u32, long_offset: u32, long_length: u32) -> Option<PropertyReply> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let r = self
            .conn
            .get_property(false, window, property, type_, long_offset, long_length)
            .ok()?
            .reply()
            .ok()?;
        Some(PropertyReply {
            type_: r.type_,
            format: r.format,
            value: r.value,
        })
    }

    fn change_property(&mut self, window: u32, property: u32, type_: u32, format: u8, mode: PropMode, data: &[u8]) {
        use x11rb::protocol::xproto::PropMode as XPropMode;
        use x11rb::protocol::xproto::ConnectionExt as _;
        let mode = match mode {
            PropMode::REPLACE => XPropMode::REPLACE,
            PropMode::PREPEND => XPropMode::PREPEND,
            PropMode::APPEND => XPropMode::APPEND,
        };
        // C: xcb_change_property(..., format, data_len, data) — data_len counts
        // *format units*, not bytes (so format 8 => bytes, 32 => bytes / 4).
        let data_len = match format {
            8 => data.len(),
            16 => data.len() / 2,
            _ => data.len() / 4,
        };
        let _ = self.conn.change_property(
            mode,
            window,
            property,
            type_,
            format,
            data_len as u32,
            data,
        );
    }

    fn delete_property(&mut self, window: u32, property: u32) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.delete_property(window, property);
    }

    fn create_window(&mut self, depth: u16, window: u32, parent: u32, x: i16, y: i16, width: u16, height: u16, border_width: u16, class: u16, visual: u32, values: &[(u32, u32)]) {
        use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, WindowClass};
        let mut aux = CreateWindowAux::default();
        for (flag, value) in values {
            match *flag {
                f if f == XCB_CW_BACK_PIXEL => aux.background_pixel = Some(*value),
                f if f == XCB_CW_BORDER_PIXEL => aux.border_pixel = Some(*value),
                f if f == XCB_CW_CURSOR => aux.cursor = Some(*value),
                f if f == XCB_CW_COLORMAP => aux.colormap = Some(*value),
                f if f == XCB_CW_EVENT_MASK => aux.event_mask = Some(x11rb::protocol::xproto::EventMask::from(*value)),
                f if f == XCB_CW_OVERRIDE_REDIRECT => aux.override_redirect = Some(*value),
                _ => {}
            }
        }
        let depth = if depth == 0 { x11rb::COPY_FROM_PARENT as u8 } else { depth as u8 };
        let visual = if visual == XCB_COPY_FROM_PARENT { x11rb::COPY_FROM_PARENT } else { visual };
        let class = match class {
            c if c == XCB_WINDOW_CLASS_INPUT_ONLY => WindowClass::INPUT_ONLY,
            _ => WindowClass::INPUT_OUTPUT,
        };
        let _ = self.conn.create_window(
            depth,
            window,
            parent,
            x,
            y,
            width,
            height,
            border_width,
            class,
            visual,
            &aux,
        );
    }

    fn create_colormap(&mut self, colormap: u32, window: u32, visual: u32) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.create_colormap(
            x11rb::protocol::xproto::ColormapAlloc::NONE,
            colormap,
            window,
            visual,
        );
    }

    fn map_window(&mut self, window: u32) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.map_window(window);
    }

    fn unmap_window(&mut self, window: u32) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.unmap_window(window);
    }

    fn destroy_window(&mut self, window: u32) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        if let Ok(cookie) = self.conn.destroy_window(window) {
            // C: src/handlers.c:536 — "we ignore all EnterNotify events which
            // have the same sequence as an UnmapNotify event", so that the
            // focus is not given to whatever window appears under the cursor
            // when a window disappears. C gets this registration from
            // handle_unmap_notify_event(); the frame of a container which is
            // being closed is already gone from the tree at that point (C hits
            // the same "Not a managed window" early return), so the port
            // registers the sequence for the frame it destroys itself.
            crate::handlers::add_ignore_event_seq(
                cookie.sequence_number() as u32,
                crate::handlers::XCB_ENTER_NOTIFY as i32,
            );
        }
    }

    fn configure_window(&mut self, window: u32, values: &[(ConfigWindow, u32)]) {
        use x11rb::protocol::xproto::{ConnectionExt as _, ConfigureWindowAux, StackMode};
        let mut aux = ConfigureWindowAux::default();
        for (flag, value) in values {
            match *flag {
                ConfigWindow::X => aux.x = Some(*value as i32),
                ConfigWindow::Y => aux.y = Some(*value as i32),
                ConfigWindow::WIDTH => aux.width = Some(*value as u32),
                ConfigWindow::HEIGHT => aux.height = Some(*value as u32),
                ConfigWindow::BORDER_WIDTH => aux.border_width = Some(*value as u32),
                ConfigWindow::SIBLING => aux.sibling = Some(*value),
                ConfigWindow::STACK_MODE => {
                    aux.stack_mode = Some(match *value {
                        1 => StackMode::BELOW,
                        2 => StackMode::TOP_IF,
                        3 => StackMode::BOTTOM_IF,
                        4 => StackMode::OPPOSITE,
                        _ => StackMode::ABOVE,
                    })
                }
            }
        }
        // C: src/xcb.c:108 -- configure and remember the sequence so the
        // resulting ConfigureNotify/EnterNotify is dropped by
        // handlers::event_is_ignored (C: xcb.c:115 add_ignore_event(..., -1)).
        if let Ok(cookie) = self.conn.configure_window(window, &aux) {
            crate::handlers::add_ignore_event_seq(cookie.sequence_number() as u32, -1);
        }
    }

    fn change_window_attributes(&mut self, window: u32, values: &[(u32, u32)]) {
        use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt as _, EventMask};
        let mut aux = ChangeWindowAttributesAux::default();
        for (flag, value) in values {
            match *flag {
                f if f == XCB_CW_BACK_PIXEL => aux.background_pixel = Some(*value),
                f if f == XCB_CW_BORDER_PIXEL => aux.border_pixel = Some(*value),
                f if f == XCB_CW_CURSOR => aux.cursor = Some(*value),
                f if f == XCB_CW_COLORMAP => aux.colormap = Some(*value),
                f if f == XCB_CW_EVENT_MASK => aux.event_mask = Some(EventMask::from(*value)),
                f if f == XCB_CW_OVERRIDE_REDIRECT => aux.override_redirect = Some(*value),
                _ => {}
            }
        }
        let _ = self.conn.change_window_attributes(window, &aux);
    }

    fn reparent_window(&mut self, window: u32, parent: u32, x: i16, y: i16) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.reparent_window(window, parent, x, y);
    }

    fn send_client_message(&mut self, window: u32, propagate: bool, event_mask: u32, type_: u32, data32: [u32; 5]) {
        use x11rb::protocol::xproto::{ClientMessageData, ClientMessageEvent, EventMask};
        use x11rb::protocol::xproto::ConnectionExt as _;
        let ev = ClientMessageEvent::new(32, window, type_, ClientMessageData::from(data32));
        let _ = self.conn.send_event(propagate, window, EventMask::from(event_mask), &ev);
    }

    fn send_configure_notify(
        &mut self,
        propagate: bool,
        destination: u32,
        event_mask: u32,
        event: &x11rb::protocol::xproto::ConfigureNotifyEvent,
    ) {
        use x11rb::protocol::xproto::{ConnectionExt as _, EventMask};
        /* C: xcb_send_event(conn, false, window, XCB_EVENT_MASK_STRUCTURE_NOTIFY,
         * (char *)generated_event) — x11rb serializes the same 32 bytes. */
        let _ = self
            .conn
            .send_event(propagate, destination, EventMask::from(event_mask), event);
    }

    fn kill_client(&mut self, window: u32) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.kill_client(window);
    }

    /// C: xcb_set_selection_owner — a void request; the following
    /// get_selection_owner round trip flushes it out before the reply is read
    /// (xcb orders the two exactly like the Xlib call pair did).
    fn set_selection_owner(&mut self, owner: u32, selection: u32, timestamp: u32) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.set_selection_owner(owner, selection, timestamp);
    }

    /// C: xcb_get_selection_owner — None on an X error (no reply).
    fn get_selection_owner(&mut self, selection: u32) -> Option<u32> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        self.conn
            .get_selection_owner(selection)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.owner)
    }

    fn grab_server(&mut self) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.grab_server();
    }

    fn ungrab_server(&mut self) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.ungrab_server();
    }

    fn flush(&mut self) {
        use x11rb::connection::Connection as _;
        let _ = self.conn.flush();
    }

    fn set_input_focus(&mut self, window: u32, timestamp: u32) {
        use x11rb::protocol::xproto::InputFocus;
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.set_input_focus(InputFocus::POINTER_ROOT, window, x11rb::protocol::xproto::Timestamp::from(timestamp));
    }

    fn query_pointer(&mut self, window: u32) -> Option<PointerReply> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let r = self.conn.query_pointer(window).ok()?.reply().ok()?;
        Some(PointerReply { root_x: r.root_x, root_y: r.root_y, child: r.child })
    }

    fn query_tree(&mut self, window: u32) -> Vec<u32> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        match self.conn.query_tree(window) {
            Ok(c) => match c.reply() {
                Ok(r) => r.children,
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        }
    }

    fn get_window_attributes(&mut self, window: u32) -> Option<WindowAttrReply> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let r = self.conn.get_window_attributes(window).ok()?.reply().ok()?;
        Some(WindowAttrReply {
            map_state: u8::from(r.map_state),
            override_redirect: r.override_redirect,
            visual: r.visual,
        })
    }

    fn get_geometry(&mut self, window: u32) -> Option<GeometryReply> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let r = self.conn.get_geometry(window).ok()?.reply().ok()?;
        Some(GeometryReply {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
            border_width: r.border_width,
            depth: r.depth as u16,
            // GetGeometryReply has no visual member in x11rb; the visual lives
            // in the window attributes (C: callers use attr->visual).
            visual: 0,
        })
    }

    fn change_save_set(&mut self, mode: u8, window: u32) {
        use x11rb::protocol::xproto::SetMode;
        use x11rb::protocol::xproto::ConnectionExt as _;
        let mode = if mode == 0 { SetMode::INSERT } else { SetMode::DELETE };
        let _ = self.conn.change_save_set(mode, window);
    }

    fn shape_select_input(&mut self, window: u32, enable: bool) {
        // C: xcb_shape_select_input(conn, window, enable) -- needs the shape
        // extension (TODO Phase 7); keep the call-site semantics.
        let _ = (window, enable);
    }

    fn grab_buttons(&mut self, window: u32, buttons: &[u32]) {
        use x11rb::protocol::xproto::{ButtonIndex, ConnectionExt as _, EventMask, GrabMode, ModMask};
        // C: src/xcb.c:267 xcb_grab_buttons — one synchronous pointer grab per
        // button, ANY modifier. The sync mode is what makes the click.c
        // allow_events() calls (replay/async) meaningful: until the WM replies
        // the client does not see the press.
        let root = self.root_screen().root;
        for &button in buttons {
            if button == 0 {
                continue;
            }
            let _ = self.conn.grab_button(
                false,
                window,
                EventMask::BUTTON_PRESS,
                GrabMode::SYNC,
                GrabMode::ASYNC,
                root,
                0u32, /* XCB_NONE: no cursor */
                ButtonIndex::from(button as u8),
                ModMask::ANY,
            );
        }
    }

    fn grab_key(&mut self, owner_events: bool, grab_window: u32, modifiers: u16, key: u8, pointer_mode: u8, keyboard_mode: u8) {
        use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
        // C: XCB_GRAB_MODE_ASYNC == 1, XCB_GRAB_MODE_SYNC == 0.
        let mode = |m: u8| {
            if m == 1 {
                GrabMode::ASYNC
            } else {
                GrabMode::SYNC
            }
        };
        let _ = self.conn.grab_key(
            owner_events,
            grab_window,
            ModMask::from(modifiers),
            key,
            mode(pointer_mode),
            mode(keyboard_mode),
        );
    }

    fn ungrab_key(&mut self, key: u8, grab_window: u32, modifiers: u16) {
        use x11rb::protocol::xproto::{ConnectionExt as _, ModMask};
        let _ = self.conn.ungrab_key(key, grab_window, ModMask::from(modifiers));
    }

    fn ungrab_button(&mut self, button: u8, grab_window: u32, modifiers: u16) {
        use x11rb::protocol::xproto::{ButtonIndex, ConnectionExt as _, ModMask};
        let _ = self
            .conn
            .ungrab_button(ButtonIndex::from(button), grab_window, ModMask::from(modifiers));
    }

    fn set_root_cursor(&mut self, cursor: u16) {
        // C: src/xcursor.c:50 xcursor_set_root_cursor — the root window's
        // cursor is set with XCB_CW_CURSOR. The cursor id comes from
        // xcursor_get_cursor(); the callers pass the semantic cursor.
        let root = self.root_screen().root;
        let id = match cursor as usize {
            0 => xcursor_get_cursor(self, xcursor_cursor_t::XCURSOR_CURSOR_POINTER),
            _ => u32::from(cursor),
        };
        self.change_window_attributes(root, &[(XCB_CW_CURSOR, id)]);
    }

    fn create_glyph_cursor(&mut self, glyph: u16) -> u32 {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::xproto::ConnectionExt as _;
        // C: xcb_cursor_load_cursor() of libXcursor. Without that library the
        // port draws the same cursor from the X core "cursor" font: source
        // glyph and (glyph + 1) as the mask, the convention of
        // XCreateFontCursor().
        let font = match self.conn.generate_id() {
            Ok(font) => font,
            Err(_) => return 0,
        };
        if self.conn.open_font(font, b"cursor").is_err() {
            return 0;
        }
        let cursor = match self.conn.generate_id() {
            Ok(cursor) => cursor,
            Err(_) => return 0,
        };
        let _ = self.conn.create_glyph_cursor(
            cursor,
            font,
            font,
            glyph,
            glyph.wrapping_add(1),
            0,
            0,
            0,
            0xffff,
            0xffff,
            0xffff,
        );
        let _ = self.conn.close_font(font);
        cursor
    }

    fn generate_id(&mut self) -> u32 {
        use x11rb::connection::Connection as _;
        self.conn.generate_id().unwrap_or(0)
    }

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
        use x11rb::protocol::xproto::{ConnectionExt as _, EventMask, GrabMode};
        let cookie = self
            .conn
            .grab_pointer(
                owner_events,
                grab_window,
                EventMask::from(event_mask),
                GrabMode::from(pointer_mode),
                GrabMode::from(keyboard_mode),
                confine_to,
                cursor,
                time,
            )
            .ok();
        let cookie = match cookie {
            Some(cookie) => cookie,
            None => {
                crate::log::errorlog("Could not grab pointer (request failed)\n");
                return None;
            }
        };
        let reply = match cookie.reply() {
            Ok(reply) => reply,
            Err(e) => {
                // C: ELOG("Could not grab pointer (error_code = %d)\n",
                // error->error_code)
                crate::log::errorlog(&format!("Could not grab pointer (error: {})\n", e));
                return None;
            }
        };
        Some(u8::from(reply.status))
    }

    fn grab_keyboard(
        &mut self,
        owner_events: bool,
        grab_window: u32,
        time: u32,
        pointer_mode: u8,
        keyboard_mode: u8,
    ) -> Option<u8> {
        use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode};
        let r = self
            .conn
            .grab_keyboard(
                owner_events,
                grab_window,
                time,
                GrabMode::from(pointer_mode),
                GrabMode::from(keyboard_mode),
            )
            .ok()?
            .reply()
            .ok()?;
        Some(u8::from(r.status))
    }

    fn ungrab_pointer(&mut self, time: u32) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.ungrab_pointer(time);
    }

    fn ungrab_keyboard(&mut self, time: u32) {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let _ = self.conn.ungrab_keyboard(time);
    }

    fn allow_events(&mut self, mode: u8, timestamp: u32) {
        use x11rb::protocol::xproto::{Allow, ConnectionExt as _};
        let _ = self.conn.allow_events(Allow::from(mode), timestamp);
    }

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
        use x11rb::protocol::xproto::ConnectionExt as _;
        if let Ok(cookie) = self.conn.warp_pointer(
            src_window, dst_window, src_x, src_y, src_width, src_height, dst_x, dst_y,
        ) {
            // C: src/x.c:1379-1383 temporarily drops the root window's event
            // mask "to ignore MotionNotify events generated by warping"; the
            // warp also makes the pointer enter whatever window is at the new
            // position, so that EnterNotify is ignored as well (it is not
            // caused by the user moving the mouse).
            crate::handlers::add_ignore_event_seq(
                cookie.sequence_number() as u32,
                crate::handlers::XCB_ENTER_NOTIFY as i32,
            );
        }
    }

    fn circulate_window(&mut self, circulate: u8, window: u32) {
        use x11rb::protocol::xproto::{Circulate, ConnectionExt as _};
        let _ = self.conn.circulate_window(Circulate::from(circulate), window);
    }

    fn change_active_pointer_grab(&mut self, cursor: u32, time: u32, event_mask: u32) {
        use x11rb::protocol::xproto::{ConnectionExt as _, EventMask};
        let _ = self
            .conn
            .change_active_pointer_grab(cursor, time, EventMask::from(event_mask));
    }

    fn poll_for_event(&mut self) -> Option<x11rb::protocol::Event> {
        use x11rb::connection::Connection as _;
        self.conn.poll_for_event().ok().flatten()
    }

    fn wait_for_event(&mut self) -> Option<x11rb::protocol::Event> {
        use x11rb::connection::Connection as _;
        self.conn.wait_for_event().ok()
    }

    fn randr_query_version(&mut self) -> Option<(u32, u32)> {
        use x11rb::protocol::randr::ConnectionExt as _;
        let r = self.conn.randr_query_version(1, 5).ok()?.reply().ok()?;
        Some((r.major_version, r.minor_version))
    }

    fn randr_get_screen_resources(&mut self, root: u32) -> Option<(u32, Vec<u32>)> {
        use x11rb::protocol::randr::ConnectionExt as _;
        let r = self.conn.randr_get_screen_resources_current(root).ok()?.reply().ok()?;
        Some((r.config_timestamp, r.outputs))
    }

    fn randr_get_monitors(&mut self, root: u32) -> Option<Vec<MonitorInfo>> {
        use x11rb::protocol::randr::ConnectionExt as _;
        // C: xcb_randr_get_monitors(conn, root, true) — only active monitors.
        let r = self.conn.randr_get_monitors(root, true).ok()?.reply().ok()?;
        Some(
            r.monitors
                .into_iter()
                .map(|m| MonitorInfo {
                    name: m.name,
                    primary: m.primary,
                    automatic: m.automatic,
                    x: m.x,
                    y: m.y,
                    width: m.width,
                    height: m.height,
                    width_in_millimeters: m.width_in_millimeters,
                    height_in_millimeters: m.height_in_millimeters,
                    outputs: m.outputs,
                })
                .collect(),
        )
    }

    fn randr_get_output_info(&mut self, output: u32, config_timestamp: u32) -> Option<OutputInfo> {
        use x11rb::protocol::randr::ConnectionExt as _;
        let r = self
            .conn
            .randr_get_output_info(output, config_timestamp)
            .ok()?
            .reply()
            .ok()?;
        Some(OutputInfo {
            name: String::from_utf8_lossy(&r.name).into_owned(),
            crtc: r.crtc,
        })
    }

    fn randr_get_crtc_info(&mut self, crtc: u32, config_timestamp: u32) -> Option<GeometryReply> {
        use x11rb::protocol::randr::ConnectionExt as _;
        let r = self
            .conn
            .randr_get_crtc_info(crtc, config_timestamp)
            .ok()?
            .reply()
            .ok()?;
        Some(GeometryReply {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
            border_width: 0,
            depth: 0,
            visual: 0,
        })
    }

    fn randr_get_output_primary(&mut self, root: u32) -> Option<u32> {
        use x11rb::protocol::randr::ConnectionExt as _;
        let r = self.conn.randr_get_output_primary(root).ok()?.reply().ok()?;
        Some(r.output)
    }

    fn xinerama_screens(&mut self) -> Option<Vec<crate::data::Rect>> {
        use x11rb::protocol::xinerama::ConnectionExt as _;
        // C: src/xinerama.c:110 xcb_get_extension_data(conn, &xcb_xinerama_id)
        // -> use the root output when the extension is not present.
        let present = self.query_extension("XINERAMA").map(|(p, _)| p).unwrap_or(false);
        if !present {
            crate::log::debuglog("Xinerama extension not found, using root output.\n");
            return None;
        }
        // C: src/xinerama.c:114 xcb_xinerama_is_active
        let active = self.conn.xinerama_is_active().ok()?.reply().ok()?.state;
        if active == 0 {
            crate::log::debuglog("Xinerama is not active (in your X-Server), using root output.\n");
            return None;
        }
        // C: src/xinerama.c:39 xcb_xinerama_query_screens
        let r = self.conn.xinerama_query_screens().ok()?.reply().ok()?;
        Some(
            r.screen_info
                .into_iter()
                .map(|s| crate::data::Rect::new(
                    s.x_org as u32,
                    s.y_org as u32,
                    s.width as u32,
                    s.height as u32,
                ))
                .collect(),
        )
    }

    fn randr_select_input(&mut self, root: u32, mask: u32) {
        use x11rb::protocol::randr::ConnectionExt as _;
        // C: the XCB_RANDR_NOTIFY_MASK_* bitmask of xcb_randr_select_input.
        let mask = x11rb::protocol::randr::NotifyMask::from(mask as u16);
        let _ = self.conn.randr_select_input(root, mask);
    }

    fn query_extension(&mut self, name: &str) -> Option<(bool, u8)> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        let r = self.conn.query_extension(name.as_bytes()).ok()?.reply().ok()?;
        Some((r.present, r.first_event))
    }

    fn raw_xcb_connection(&mut self) -> Option<*mut std::ffi::c_void> {
        // C: the xcb_connection_t * passed to xkb_x11_setup_xkb_extension() and
        // xkb_x11_keymap_new_from_device().
        Some(self.conn.get_raw_xcb_connection())
    }

    fn get_modifier_mapping(&mut self) -> Option<Vec<u8>> {
        use x11rb::protocol::xproto::ConnectionExt as _;
        // C: xcb_get_modifier_mapping / xcb_get_modifier_mapping_reply.
        self.conn
            .get_modifier_mapping()
            .ok()?
            .reply()
            .ok()
            .map(|reply| reply.keycodes)
    }

    fn root_screen(&mut self) -> ScreenInfo {
        use x11rb::connection::Connection as _;
        let setup = self.conn.setup();
        let idx = self.screen_idx.min(setup.roots.len().saturating_sub(1));
        let screen = &setup.roots[idx];
        ScreenInfo {
            root: screen.root,
            width_in_pixels: screen.width_in_pixels,
            height_in_pixels: screen.height_in_pixels,
            root_depth: screen.root_depth as u16,
            root_visual: screen.root_visual,
            default_colormap: screen.default_colormap,
            black_pixel: screen.black_pixel,
            white_pixel: screen.white_pixel,
        }
    }

    /// C: xcb_aux_find_visual_by_attrs(screen, -1, depth) — the first visual in
    /// the depth list of the screen with that depth (xcb-util walks
    /// allowed_depths in order, exactly like this loop).
    fn find_visual_by_depth(&mut self, depth: u16) -> Option<(u32, u16)> {
        use x11rb::connection::Connection as _;
        let setup = self.conn.setup();
        let idx = self.screen_idx.min(setup.roots.len().saturating_sub(1));
        let screen = &setup.roots[idx];
        for allowed in &screen.allowed_depths {
            if allowed.depth as u16 == depth {
                if let Some(visual) = allowed.visuals.first() {
                    return Some((visual.visual_id, allowed.depth as u16));
                }
            }
        }
        None
    }
}

/****************************************************************************\
 * RecordingConn: test double recording every call.
\****************************************************************************/

/// Test double: records the requests in call order.
/// Adapter for the libi3 X11 helpers (libi3/get_mod_mask.c and
/// libi3/fake_configure_notify.c): libi3 owns the requests, i3-core's XConn
/// serves them, so RecordingConn tests and i3bar's XConnLike get them too.
pub struct Libi3Conn<'a>(pub &'a mut dyn XConn);

impl libi3::get_mod_mask::ModMaskConn for Libi3Conn<'_> {
    /// C: libi3/get_mod_mask.c:28 xcb_flush(conn)
    fn flush(&mut self) {
        self.0.flush();
    }

    /// C: libi3/get_mod_mask.c:31-32 xcb_get_modifier_mapping
    fn get_modifier_mapping(&mut self) -> Option<Vec<u8>> {
        self.0.get_modifier_mapping()
    }
}

impl libi3::fake_configure_notify::SendEventConn for Libi3Conn<'_> {
    /// C: libi3/fake_configure_notify.c:41 xcb_send_event
    fn send_event(
        &mut self,
        propagate: bool,
        destination: u32,
        event_mask: u32,
        event: x11rb::protocol::xproto::ConfigureNotifyEvent,
    ) {
        self.0
            .send_configure_notify(propagate, destination, event_mask, &event);
    }

    /// C: libi3/fake_configure_notify.c:42 xcb_flush(conn)
    fn flush(&mut self) {
        self.0.flush();
    }
}

#[derive(Debug, Default)]
pub struct RecordingConn {
    pub root: u32,
    pub log: Vec<String>,
    pub next_id: u32,
    pub atoms: std::collections::HashMap<String, u32>,
    pub props: std::collections::HashMap<(u32, u32), PropertyReply>,
    pub geoms: std::collections::HashMap<u32, GeometryReply>,
    pub attrs: std::collections::HashMap<u32, WindowAttrReply>,
    pub children: Vec<u32>,
    /// Canned xcb_query_pointer reply.
    pub pointer: Option<PointerReply>,
    /// Canned RandR version reply; None makes the extension look absent.
    pub randr_version: Option<(u32, u32)>,
    /// Canned xcb_randr_get_monitors reply; None is the X error case.
    pub monitors: Option<Vec<MonitorInfo>>,
    /// Canned xcb_randr_get_screen_resources_current reply:
    /// (config_timestamp, outputs).
    pub screen_resources: Option<(u32, Vec<u32>)>,
    pub output_infos: std::collections::HashMap<u32, OutputInfo>,
    pub crtc_infos: std::collections::HashMap<u32, GeometryReply>,
    /// Canned xcb_randr_get_output_primary reply.
    pub output_primary: Option<u32>,
    /// Canned xcb_get_extension_data replies: extension name -> first_event
    /// (0 means "not present", as in xcb_get_extension_data).
    pub extensions: std::collections::HashMap<&'static str, u8>,
    /// Canned xcb_get_modifier_mapping reply: the 8 * keycodes_per_modifier
    /// keycodes, empty when the request should fail.
    pub modifier_mapping: Vec<u8>,
    /// The raw value of every xcb_change_property written so far, keyed by
    /// (window, property). The log line only carries the length, so tests
    /// that check what was actually written (e.g. _NET_ACTIVE_WINDOW) read
    /// the payload here.
    pub written_properties: std::collections::HashMap<(u32, u32), Vec<u8>>,
    /// The modelled selection table: selection atom -> owner window, as
    /// xcb_set_selection_owner left it (XCB_NONE removes the entry).
    pub selections: std::collections::HashMap<u32, u32>,
    /// Canned xcb_get_selection_owner reply, overriding `selections` when set —
    /// this is how a test plays "another tray is already running".
    pub selection_owner_reply: Option<u32>,
    /// Canned xcb_aux_find_visual_by_attrs(-1, depth) result: the 32-bit
    /// ARGB32 visual a test screen offers (None = no such visual).
    pub visual_by_depth: Option<(u32, u16)>,
}

impl RecordingConn {
    pub fn new(root: u32) -> RecordingConn {
        RecordingConn { root, ..Default::default() }
    }

    pub fn intern_named(&mut self, name: &str) -> u32 {
        let len = self.atoms.len();
        *self.atoms.entry(name.to_string()).or_insert((len + 1) as u32)
    }
}

impl XConn for RecordingConn {
    /// C: xcb_intern_atom — one stable id per unique name. XCB_NONE (0) is the
    /// failure value the callers check for, so it is never returned.
    fn intern_atom(&mut self, name: &str) -> Option<u32> {
        Some(self.intern_named(name))
    }

    /// C: xcb_get_atom_name — the reverse lookup over the interned names.
    fn atom_name(&mut self, atom: u32) -> Option<String> {
        self.atoms
            .iter()
            .find(|(_, id)| **id == atom)
            .map(|(name, _)| name.clone())
    }

    /// C: xcb_get_property — the canned reply from props, keyed by
    /// (window, property). The type/offset/length arguments are not modelled:
    /// tests store exactly the reply the caller is expected to read.
    fn get_property(
        &mut self,
        window: u32,
        property: u32,
        _type: u32,
        _long_offset: u32,
        _long_length: u32,
    ) -> Option<PropertyReply> {
        self.props.get(&(window, property)).cloned()
    }

    /// C: xcb_change_property — the log line carries only the byte length; the
    /// payload itself lands in written_properties so tests can read it back.
    fn change_property(
        &mut self,
        window: u32,
        property: u32,
        type_: u32,
        format: u8,
        mode: PropMode,
        data: &[u8],
    ) {
        self.log.push(format!(
            "change_property(win={:#x}, prop={}, type={}, fmt={}, mode={:?}, bytes={})",
            window,
            property,
            type_,
            format,
            mode,
            data.len()
        ));
        match mode {
            PropMode::REPLACE => {
                self.written_properties
                    .insert((window, property), data.to_vec());
            }
            PropMode::APPEND => {
                self.written_properties
                    .entry((window, property))
                    .or_default()
                    .extend_from_slice(data);
            }
            PropMode::PREPEND => {
                let old = self
                    .written_properties
                    .remove(&(window, property))
                    .unwrap_or_default();
                let mut merged = data.to_vec();
                merged.extend_from_slice(&old);
                self.written_properties.insert((window, property), merged);
            }
        }
    }

    /// C: xcb_delete_property.
    fn delete_property(&mut self, window: u32, property: u32) {
        self.log
            .push(format!("delete_property(win={:#x}, prop={})", window, property));
        self.written_properties.remove(&(window, property));
    }

    /// C: xcb_create_window.
    fn create_window(
        &mut self,
        depth: u16,
        window: u32,
        parent: u32,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        border_width: u16,
        class: u16,
        visual: u32,
        values: &[(u32, u32)],
    ) {
        self.log.push(format!(
            "create_window({:#x}, parent={:#x}, x={}, y={}, {}x{}, bw={}, depth={}, class={}, visual={:#x}, values={:?})",
            window, parent, x, y, width, height, border_width, depth, class, visual, values
        ));
    }

    /// C: xcb_create_colormap.
    fn create_colormap(&mut self, colormap: u32, window: u32, visual: u32) {
        self.log.push(format!(
            "create_colormap({:#x}, win={:#x}, visual={:#x})",
            colormap, window, visual
        ));
    }

    /// C: xcb_map_window.
    fn map_window(&mut self, window: u32) {
        self.log.push(format!("map_window({:#x})", window));
    }

    /// C: xcb_unmap_window.
    fn unmap_window(&mut self, window: u32) {
        self.log.push(format!("unmap_window({:#x})", window));
    }

    /// C: xcb_destroy_window.
    fn destroy_window(&mut self, window: u32) {
        self.log.push(format!("destroy_window({:#x})", window));
    }

    /// C: xcb_configure_window — the value pairs print as named flags, so a
    /// test can tell a move from a stack change at a glance.
    fn configure_window(&mut self, window: u32, values: &[(ConfigWindow, u32)]) {
        let parts: Vec<String> = values
            .iter()
            .map(|(flag, value)| {
                let name = match flag {
                    ConfigWindow::X => "X",
                    ConfigWindow::Y => "Y",
                    ConfigWindow::WIDTH => "WIDTH",
                    ConfigWindow::HEIGHT => "HEIGHT",
                    ConfigWindow::BORDER_WIDTH => "BORDER_WIDTH",
                    ConfigWindow::SIBLING => "SIBLING",
                    ConfigWindow::STACK_MODE => "STACK_MODE",
                };
                format!("({}, {})", name, value)
            })
            .collect();
        self.log.push(format!(
            "configure_window({:#x}, [{}])",
            window,
            parts.join(", ")
        ));
    }

    /// C: xcb_change_window_attributes — the CW flags are printed as the raw
    /// XCB_CW_* bit values.
    fn change_window_attributes(&mut self, window: u32, values: &[(u32, u32)]) {
        let parts: Vec<String> = values
            .iter()
            .map(|(flag, value)| format!("({:#x}, {})", flag, value))
            .collect();
        self.log.push(format!(
            "change_window_attributes({:#x}, [{}])",
            window,
            parts.join(", ")
        ));
    }

    /// C: xcb_reparent_window.
    fn reparent_window(&mut self, window: u32, parent: u32, x: i16, y: i16) {
        self.log.push(format!(
            "reparent_window({:#x} -> {:#x} @ {},{})",
            window, parent, x, y
        ));
    }

    /// C: xcb_send_event — the five 32-bit data words are all a test needs to
    /// identify the message (WM_DELETE_WINDOW, WM_TAKE_FOCUS, ...).
    fn send_client_message(
        &mut self,
        window: u32,
        propagate: bool,
        event_mask: u32,
        type_: u32,
        data32: [u32; 5],
    ) {
        self.log.push(format!(
            "send_client_message(win={:#x}, type={}, mask={:#x}, propagate={}, data32=[{}, {}, {}, {}, {}])",
            window, type_, event_mask, propagate,
            data32[0], data32[1], data32[2], data32[3], data32[4]
        ));
    }

    /// C: xcb_send_event of libi3/fake_configure_notify.c — the field list is
    /// what the caller computed (src/xcb.c fake_absolute_configure_notify).
    fn send_configure_notify(
        &mut self,
        propagate: bool,
        destination: u32,
        event_mask: u32,
        event: &x11rb::protocol::xproto::ConfigureNotifyEvent,
    ) {
        self.log.push(format!(
            "send_configure_notify(win={:#x}, mask={:#x}, propagate={}, x={}, y={}, w={}, h={}, bw={})",
            destination, event_mask, propagate, event.x, event.y, event.width, event.height,
            event.border_width
        ));
    }

    /// C: xcb_set_selection_owner — records the request and the resulting
    /// ownership, so a later get_selection_owner replies with what a real
    /// server would report.
    fn set_selection_owner(&mut self, owner: u32, selection: u32, timestamp: u32) {
        self.log.push(format!(
            "set_selection_owner(owner={:#x}, selection={}, ts={})",
            owner, selection, timestamp
        ));
        if owner == 0 {
            self.selections.remove(&selection);
        } else {
            self.selections.insert(selection, owner);
        }
    }

    /// C: xcb_get_selection_owner — the canned override wins (the "another
    /// tray owns it" case), otherwise the modelled table; an unowned selection
    /// replies XCB_NONE (0) like the real server.
    fn get_selection_owner(&mut self, selection: u32) -> Option<u32> {
        self.log
            .push(format!("get_selection_owner(selection={})", selection));
        Some(
            self.selection_owner_reply
                .or_else(|| self.selections.get(&selection).copied())
                .unwrap_or(0),
        )
    }

    /// C: xcb_kill_client.
    fn kill_client(&mut self, window: u32) {
        self.log.push(format!("kill_client({:#x})", window));
    }

    /// C: xcb_grab_server.
    fn grab_server(&mut self) {
        self.log.push("grab_server".to_string());
    }

    /// C: xcb_ungrab_server.
    fn ungrab_server(&mut self) {
        self.log.push("ungrab_server".to_string());
    }

    /// C: xcb_flush.
    fn flush(&mut self) {
        self.log.push("flush".to_string());
    }

    /// C: xcb_set_input_focus (the timestamp is the C global last_timestamp).
    fn set_input_focus(&mut self, window: u32, timestamp: u32) {
        self.log
            .push(format!("set_input_focus({:#x}, ts={})", window, timestamp));
    }

    /// C: xcb_query_pointer — the canned reply, None when the test did not set
    /// one (the X error case).
    fn query_pointer(&mut self, window: u32) -> Option<PointerReply> {
        self.log.push(format!("query_pointer({:#x})", window));
        self.pointer
    }

    /// C: xcb_query_tree — the canned child list.
    fn query_tree(&mut self, window: u32) -> Vec<u32> {
        self.log.push(format!("query_tree({:#x})", window));
        self.children.clone()
    }

    /// C: xcb_get_window_attributes.
    fn get_window_attributes(&mut self, window: u32) -> Option<WindowAttrReply> {
        self.log.push(format!("get_window_attributes({:#x})", window));
        self.attrs.get(&window).copied()
    }

    /// C: xcb_get_geometry.
    fn get_geometry(&mut self, window: u32) -> Option<GeometryReply> {
        self.log.push(format!("get_geometry({:#x})", window));
        self.geoms.get(&window).copied()
    }

    /// C: xcb_change_save_set (mode: 0 = insert, 1 = delete).
    fn change_save_set(&mut self, mode: u8, window: u32) {
        self.log
            .push(format!("change_save_set(mode={}, win={:#x})", mode, window));
    }

    /// C: xcb_shape_select_input.
    fn shape_select_input(&mut self, window: u32, enable: bool) {
        self.log.push(format!(
            "shape_select_input({:#x}, enable={})",
            window, enable
        ));
    }

    /// C: xcb_grab_button for every button of the click bindings.
    fn grab_buttons(&mut self, window: u32, buttons: &[u32]) {
        self.log
            .push(format!("grab_buttons({:#x}, {:?})", window, buttons));
    }

    /// C: xcb_grab_key.
    fn grab_key(
        &mut self,
        owner_events: bool,
        grab_window: u32,
        modifiers: u16,
        key: u8,
        pointer_mode: u8,
        keyboard_mode: u8,
    ) {
        self.log.push(format!(
            "grab_key(owner_events={}, win={:#x}, mods={}, key={}, ptr={}, kbd={})",
            owner_events, grab_window, modifiers, key, pointer_mode, keyboard_mode
        ));
    }

    /// C: xcb_ungrab_key (the wildcard any-key ungrab of ungrab_all_keys).
    fn ungrab_key(&mut self, key: u8, grab_window: u32, modifiers: u16) {
        self.log.push(format!(
            "ungrab_key(key={}, win={:#x}, mods={})",
            key, grab_window, modifiers
        ));
    }

    /// C: xcb_ungrab_button.
    fn ungrab_button(&mut self, button: u8, grab_window: u32, modifiers: u16) {
        self.log.push(format!(
            "ungrab_button(button={}, win={:#x}, mods={})",
            button, grab_window, modifiers
        ));
    }

    /// C: xcb_create_glyph_cursor — a fresh id per glyph, like generate_id.
    fn create_glyph_cursor(&mut self, glyph: u16) -> u32 {
        self.log.push(format!("create_glyph_cursor({})", glyph));
        self.generate_id()
    }

    /// C: xcursor_set_root_cursor.
    fn set_root_cursor(&mut self, cursor: u16) {
        self.log.push(format!("set_root_cursor({})", cursor));
    }

    /// C: xcb_generate_id. The counter starts at 0, so the first id is 1 and
    /// never collides with XCB_NONE (0xffff_ffff).
    fn generate_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    /// C: the xcb_screen_t of the connection — the canned geometry of the root
    /// window when a test stored one, otherwise a plain 1280x800 screen.
    fn root_screen(&mut self) -> ScreenInfo {
        let geom = self.geoms.get(&self.root).copied();
        let width = geom.map(|g| g.width).filter(|w| *w != 0).unwrap_or(1280);
        let height = geom.map(|g| g.height).filter(|h| *h != 0).unwrap_or(800);
        let depth = geom.map(|g| g.depth).filter(|d| *d != 0).unwrap_or(24);
        let visual = geom.map(|g| g.visual).filter(|v| *v != 0).unwrap_or(0x21);
        ScreenInfo {
            root: self.root,
            width_in_pixels: width,
            height_in_pixels: height,
            root_depth: depth,
            root_visual: visual,
            default_colormap: 0x20,
            black_pixel: 0,
            white_pixel: 0x00ff_ffff,
        }
    }

    /// C: xcb_aux_find_visual_by_attrs(screen, -1, depth) — the canned
    /// visual a test configured (None models a screen without such a visual,
    /// e.g. one that cannot do transparency).
    fn find_visual_by_depth(&mut self, depth: u16) -> Option<(u32, u16)> {
        self.log.push(format!("find_visual_by_depth({})", depth));
        self.visual_by_depth.filter(|(_, d)| *d == depth)
    }

    /// C: xcb_randr_query_version — None makes the extension look absent.
    fn randr_query_version(&mut self) -> Option<(u32, u32)> {
        self.log.push("randr_query_version".to_string());
        self.randr_version
    }

    /// C: xcb_randr_get_screen_resources_current.
    fn randr_get_screen_resources(&mut self, root: u32) -> Option<(u32, Vec<u32>)> {
        self.log
            .push(format!("randr_get_screen_resources({:#x})", root));
        self.screen_resources.clone()
    }

    /// C: xcb_randr_get_monitors (RandR 1.5).
    fn randr_get_monitors(&mut self, root: u32) -> Option<Vec<MonitorInfo>> {
        self.log.push(format!("randr_get_monitors({:#x})", root));
        self.monitors.clone()
    }

    /// C: xcb_randr_get_output_info.
    fn randr_get_output_info(&mut self, output: u32, config_timestamp: u32) -> Option<OutputInfo> {
        self.log.push(format!(
            "randr_get_output_info({}, {})",
            output, config_timestamp
        ));
        self.output_infos.get(&output).cloned()
    }

    /// C: xcb_randr_get_crtc_info.
    fn randr_get_crtc_info(&mut self, crtc: u32, config_timestamp: u32) -> Option<GeometryReply> {
        self.log
            .push(format!("randr_get_crtc_info({}, {})", crtc, config_timestamp));
        self.crtc_infos.get(&crtc).copied()
    }

    /// C: xcb_randr_get_output_primary (XCB_NONE when there is none).
    fn randr_get_output_primary(&mut self, root: u32) -> Option<u32> {
        self.log.push(format!("randr_get_output_primary({:#x})", root));
        self.output_primary
    }

    /// C: xcb_randr_select_input — the XCB_RANDR_NOTIFY_MASK_* bitmask.
    fn randr_select_input(&mut self, root: u32, mask: u32) {
        self.log
            .push(format!("randr_select_input({:#x}, mask={:#x})", root, mask));
    }

    /// C: xcb_get_extension_data — an unknown extension answers "not present",
    /// an entry stored with first_event 0 does the same (present == false).
    fn query_extension(&mut self, name: &str) -> Option<(bool, u8)> {
        let first_event = self.extensions.get(name).copied().unwrap_or(0);
        self.log.push(format!(
            "query_extension({}, first_event={})",
            name, first_event
        ));
        Some((first_event != 0, first_event))
    }

    /// C: xcb_grab_pointer. The double never owns a server, so the grab is
    /// recorded and reported as an X error (None): the drag/resize code then
    /// takes its abort path, which is what the tests expect.
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
        self.log.push(format!(
            "grab_pointer(owner_events={}, win={:#x}, mask={:#x}, ptr={}, kbd={}, confine={:#x}, cursor={:#x}, ts={})",
            owner_events, grab_window, event_mask, pointer_mode, keyboard_mode, confine_to, cursor, time
        ));
        None
    }

    /// C: xcb_grab_keyboard — same abort semantics as grab_pointer.
    fn grab_keyboard(
        &mut self,
        owner_events: bool,
        grab_window: u32,
        time: u32,
        pointer_mode: u8,
        keyboard_mode: u8,
    ) -> Option<u8> {
        self.log.push(format!(
            "grab_keyboard(owner_events={}, win={:#x}, ts={}, ptr={}, kbd={})",
            owner_events, grab_window, time, pointer_mode, keyboard_mode
        ));
        None
    }

    /// C: xcb_ungrab_pointer.
    fn ungrab_pointer(&mut self, time: u32) {
        self.log.push(format!("ungrab_pointer(ts={})", time));
    }

    /// C: xcb_ungrab_keyboard.
    fn ungrab_keyboard(&mut self, time: u32) {
        self.log.push(format!("ungrab_keyboard(ts={})", time));
    }

    /// C: xcb_allow_events.
    fn allow_events(&mut self, mode: u8, timestamp: u32) {
        self.log
            .push(format!("allow_events(mode={}, ts={})", mode, timestamp));
    }

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
        self.log.push(format!(
            "warp_pointer(src={:#x}, dst={:#x}, src_pos={},{} src_size={}x{}, dst_pos={},{})",
            src_window, dst_window, src_x, src_y, src_width, src_height, dst_x, dst_y
        ));
    }

    /// C: xcb_circulate_window.
    fn circulate_window(&mut self, circulate: u8, window: u32) {
        self.log
            .push(format!("circulate_window({}, {:#x})", circulate, window));
    }

    /// C: xcb_change_active_pointer_grab.
    fn change_active_pointer_grab(&mut self, cursor: u32, time: u32, event_mask: u32) {
        self.log.push(format!(
            "change_active_pointer_grab(cursor={:#x}, ts={}, mask={:#x})",
            cursor, time, event_mask
        ));
    }

    /// C: xcb_poll_for_event — the double has no event queue.
    fn poll_for_event(&mut self) -> Option<x11rb::protocol::Event> {
        None
    }

    /// C: blocking on the X file descriptor — the double never blocks.
    fn wait_for_event(&mut self) -> Option<x11rb::protocol::Event> {
        None
    }

    /// C: the xcb_connection_t * handed to libxkbcommon-x11: none.
    fn raw_xcb_connection(&mut self) -> Option<*mut std::ffi::c_void> {
        None
    }

    /// C: xcb_get_modifier_mapping — the canned keycode list; empty means the
    /// request fails (None), exactly like an X error.
    fn get_modifier_mapping(&mut self) -> Option<Vec<u8>> {
        self.log.push("get_modifier_mapping".to_string());
        if self.modifier_mapping.is_empty() {
            return None;
        }
        Some(self.modifier_mapping.clone())
    }
}
