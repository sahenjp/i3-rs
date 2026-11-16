//! manage.c: Initially managing new windows (or existing ones on restart)
//! plus window.c: Updates window attributes (X11 hints/properties) (1:1 port).
//!
//! C: src/manage.c, src/window.c. Property replies come from the XConn
//! abstraction instead of xcb cookies; the parallel-request pattern of C
//! (issuing all GetProperty cookies then reading replies) is replaced by
//! sequential gets on the connection (same property set, same order).

use crate::backend::{
    ConBackend, GeometryReply, PropertyReply, WindowAttrReply, XConn, XCB_CW_EVENT_MASK,

};
use crate::data::XCB_WINDOW_NONE;
use crate::con::{
    con_accepts_window, con_activate, con_attach, con_by_window_id, con_descend_tiling_focused,
    con_detach, con_get_fullscreen_covering_ws, con_get_output, con_get_workspace, con_num_windows,
    con_set_border_style, con_set_urgency, con_merge_into,
};
use crate::data::{
    BorderStyle, ConId, ConType, Match, WindowDock, WindowId,
    A_NO_FOCUS, A_TO_OUTPUT, A_TO_WORKSPACE, A_TO_WORKSPACE_NUMBER,

};
use crate::floating::floating_enable;
use crate::con::con_is_internal;
use crate::match_::match_matches_window;
use crate::output::{get_output_containing, output_primary_name};
use crate::render::render_con;
use crate::tree::{tree_open_con, tree_render, Tree};
use crate::workspace::{get_existing_workspace_by_num, workspace_get, workspace_is_visible};
use crate::xcb_util::{xcb_get_preferred_window_type, xcb_reply_contains_atom, xcb_set_window_rect, Atoms};
// Only the test module below constructs a RecordingConn; importing it at module
// level would be an unused import in the normal build.
#[cfg(test)]
use crate::xcb_util::RecordingConn;
use crate::ewmh::{ewmh_get_workspace_by_index, ewmh_update_wm_desktop, NET_WM_DESKTOP_ALL, NET_WM_DESKTOP_NONE};
use crate::util::ws_name_to_number;
use crate::assignments::{assignment_for, run_assignments};
use slotmap::Key as _;

/// C: src/window.c:18 window_free (slotmap makes this a removal).
pub fn window_free(t: &mut Tree, window: WindowId) {
    t.window_remove(window);
}

/// Updates the WM_CLASS (consisting of the class and instance) for the
/// given window.
///
/// C: src/window.c:34 window_update_class
pub fn window_update_class(t: &mut Tree, window: WindowId, prop: Option<&PropertyReply>) {
    let prop = match prop {
        Some(p) if !p.value.is_empty() => p,
        _ => return,
    };
    // C: xcb_icccm_get_wm_class_reply splits the property into two
    // NUL-terminated strings: instance first, class second.
    let bytes = &prop.value;
    let first_len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let instance = String::from_utf8_lossy(&bytes[..first_len]).into_owned();
    let class_index = first_len + 1;
    let class = if class_index < bytes.len() {
        let rest = &bytes[class_index..];
        let class_len = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        String::from_utf8_lossy(&rest[..class_len]).into_owned()
    } else {
        String::new()
    };
    let w = t.window_mut(window);
    w.class_instance = Some(instance);
    w.class_class = if class.is_empty() { None } else { Some(class) };
}

/// Updates the name by using _NET_WM_NAME (encoded in UTF-8) for the given
/// window. Further updates using window_update_name_legacy will be ignored.
///
/// C: src/window.c:68 window_update_name
pub fn window_update_name(t: &mut Tree, window: WindowId, prop: Option<&PropertyReply>) {
    let prop = match prop {
        Some(p) if !p.value.is_empty() => p,
        _ => return,
    };
    // Truncate the name at the first zero byte. See #3515.
    let bytes = &prop.value;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let name = String::from_utf8_lossy(&bytes[..end]).into_owned();
    {
        let w = t.window_mut(window);
        w.name = Some(name);
        w.name_x_changed = true;
        w.uses_net_wm_name = true;
    }
    // C: con_parse_title_format + ewmh_update_visible_name -- the title_format
    // rendering is ported in the render phase; here the name change flag is set.
}

/// Updates the name by using WM_NAME (encoded in COMPOUND_TEXT).
///
/// C: src/window.c:104 window_update_name_legacy
pub fn window_update_name_legacy(t: &mut Tree, window: WindowId, prop: Option<&PropertyReply>) {
    let prop = match prop {
        Some(p) if !p.value.is_empty() => p,
        _ => return,
    };
    // ignore update when the window is known to already have a UTF-8 name
    if t.windows[window].uses_net_wm_name {
        return;
    }
    let bytes = &prop.value;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let name = String::from_utf8_lossy(&bytes[..end]).into_owned();
    let w = t.window_mut(window);
    w.name = Some(name);
    w.name_x_changed = true;
}

/// Updates the CLIENT_LEADER (logical parent window).
///
/// C: src/window.c:143 window_update_leader
pub fn window_update_leader(t: &mut Tree, window: WindowId, prop: Option<&PropertyReply>) {
    match prop {
        Some(p) if !p.value.is_empty() => {
            let words = p.words32();
            if let Some(&leader) = words.first() {
                t.window_mut(window).leader = leader;
            }
        }
        _ => {
            t.window_mut(window).leader = XCB_WINDOW_NONE;
        }
    }
}

/// Updates the TRANSIENT_FOR (logical parent window).
///
/// C: src/window.c:168 window_update_transient_for
pub fn window_update_transient_for(t: &mut Tree, window: WindowId, prop: Option<&PropertyReply>) {
    match prop {
        Some(p) if !p.value.is_empty() => {
            let words = p.words32();
            if let Some(&transient_for) = words.first() {
                t.window_mut(window).transient_for = transient_for;
            }
        }
        _ => {
            t.window_mut(window).transient_for = XCB_WINDOW_NONE;
        }
    }
}

/// Updates the _NET_WM_STRUT_PARTIAL (reserved pixels at the screen edges).
///
/// C: src/window.c:193 window_update_strut_partial
pub fn window_update_strut_partial(t: &mut Tree, window: WindowId, prop: Option<&PropertyReply>) {
    let prop = match prop {
        Some(p) if !p.value.is_empty() => p,
        _ => {
            // C: DLOG("_NET_WM_STRUT_PARTIAL not set.\n");
            crate::log::dlog_at(
                file!(),
                "window_update_strut_partial",
                line!(),
                "_NET_WM_STRUT_PARTIAL not set.\n",
            );
            return;
        }
    };
    let words = prop.words32();
    if words.len() < 4 {
        return;
    }
    // C: src/window.c:203 DLOG("Reserved pixels changed to: left = %d,
    // right = %d, top = %d, bottom = %d\n", ...)
    crate::log::dlog_at(
        file!(),
        "window_update_strut_partial",
        line!(),
        &format!(
            "Reserved pixels changed to: left = {}, right = {}, top = {}, bottom = {}\n",
            words[0], words[1], words[2], words[3]
        ),
    );
    let w = t.window_mut(window);
    w.reserved.left = words[0];
    w.reserved.right = words[1];
    w.reserved.top = words[2];
    w.reserved.bottom = words[3];

    // C: src/handlers.c:1260 — handle_strut_partial_change() calls this and then
    // moves an already managed dock client to the dockarea its reserved edges
    // now point at. The move is a no-op unless the container already sits in a
    // dockarea (C: src/handlers.c:1263), so the manage_window() caller keeps the
    // C behaviour of only storing the values.
    strut_partial_change_dock(t, window);
}

/// Moves a dock client to the dockarea selected by its reserved edges.
///
/// C: src/handlers.c:1259 handle_strut_partial_change (the dock part after
/// window_update_strut_partial), src/handlers.c:1262-1304.
pub fn strut_partial_change_dock(t: &mut Tree, window: WindowId) {
    // C works on the client window (con->window->id); the port indexes the
    // managed window by its slotmap key, so translate to the X11 window id.
    let x_window = t.window(window).id;
    let con = match con_by_window_id(t, x_window) {
        Some(con) => con,
        None => return,
    };

    /* we only handle this change for dock clients */
    match t.cons[con].parent {
        Some(parent) if t.cons[parent].r#type == ConType::CT_DOCKAREA => {}
        _ => return,
    }

    let mut search_at = t.croot;
    if let Some(output) = con_get_output(t, con) {
        // C: DLOG("Starting search at output %s\n", output->name);
        crate::log::dlog_at(
            file!(),
            "handle_strut_partial_change",
            line!(),
            &format!(
                "Starting search at output {}\n",
                t.cons[output].name.clone().unwrap_or_default()
            ),
        );
        search_at = Some(output);
    }

    /* find out the desired position of this dock window */
    let (top, bottom) = {
        let w = t.window(window);
        (w.reserved.top, w.reserved.bottom)
    };
    let dock = if top > 0 && bottom == 0 {
        // C: DLOG("Top dock client\n");
        WindowDock::W_DOCK_TOP
    } else if top == 0 && bottom > 0 {
        // C: DLOG("Bottom dock client\n");
        WindowDock::W_DOCK_BOTTOM
    } else {
        // C: DLOG("Ignoring invalid reserved edges (_NET_WM_STRUT_PARTIAL),
        // using position as fallback:\n");
        let search_rect = search_at.map(|s| t.cons[s].rect).unwrap_or_default();
        if (t.cons[con].geometry.y as i32) < (search_rect.height as i32 / 2) {
            WindowDock::W_DOCK_TOP
        } else {
            WindowDock::W_DOCK_BOTTOM
        }
    };
    t.window_mut(window).dock = dock;

    /* find the dockarea */
    let dockarea = match con_for_window_with_match(t, search_at, Some(window)).0 {
        Some(dockarea) => dockarea,
        // C: assert(dockarea != NULL) — a managed dock client always matches a
        // dockarea; keep the tree untouched instead of aborting.
        None => return,
    };

    /* attach the dock to the dock area */
    con_detach(t, con);
    con_attach(t, &mut crate::backend::NullBackend, con, dockarea, true);
    crate::tree::request_tree_render();
}

/// Updates the WM_WINDOW_ROLE.
///
/// C: src/window.c:218 window_update_role
pub fn window_update_role(t: &mut Tree, window: WindowId, prop: Option<&PropertyReply>) {
    let prop = match prop {
        Some(p) if !p.value.is_empty() => p,
        _ => return,
    };
    let role = String::from_utf8_lossy(&prop.value).into_owned();
    t.window_mut(window).role = Some(role);
}

/// Maps an interned `_NET_WM_WINDOW_TYPE_*` atom to the value stored in
/// `Window::window_type`.
///
/// C keeps the X11 atom itself in `i3Window::window_type`
/// (src/window.c:239 window_update_type) and compares it with the atom that
/// match_parse_property interned from the criterion string (src/match.c:346).
/// The port parses the criteria without a connection, so the criterion
/// values are stable placeholder ids (crate::dump). The runtime atom has to
/// be mapped onto the very same ids for `match_matches_window` to compare
/// equal numbers; atoms i3 does not track map to XCB_ATOM_NONE, exactly like
/// xcb_get_preferred_window_type() drops them.
pub fn window_type_from_atom(a: &Atoms, atom: u32) -> u32 {
    use crate::dump::{
        A__NET_WM_WINDOW_TYPE_DIALOG, A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU,
        A__NET_WM_WINDOW_TYPE_MENU, A__NET_WM_WINDOW_TYPE_NORMAL,
        A__NET_WM_WINDOW_TYPE_NOTIFICATION, A__NET_WM_WINDOW_TYPE_POPUP_MENU,
        A__NET_WM_WINDOW_TYPE_SPLASH, A__NET_WM_WINDOW_TYPE_TOOLBAR,
        A__NET_WM_WINDOW_TYPE_TOOLTIP, A__NET_WM_WINDOW_TYPE_UTILITY,
    };

    if atom == crate::data::XCB_ATOM_NONE {
        return crate::data::XCB_ATOM_NONE;
    }
    if atom == a.A__NET_WM_WINDOW_TYPE_NORMAL {
        A__NET_WM_WINDOW_TYPE_NORMAL
    } else if atom == a.A__NET_WM_WINDOW_TYPE_DIALOG {
        A__NET_WM_WINDOW_TYPE_DIALOG
    } else if atom == a.A__NET_WM_WINDOW_TYPE_UTILITY {
        A__NET_WM_WINDOW_TYPE_UTILITY
    } else if atom == a.A__NET_WM_WINDOW_TYPE_TOOLBAR {
        A__NET_WM_WINDOW_TYPE_TOOLBAR
    } else if atom == a.A__NET_WM_WINDOW_TYPE_SPLASH {
        A__NET_WM_WINDOW_TYPE_SPLASH
    } else if atom == a.A__NET_WM_WINDOW_TYPE_MENU {
        A__NET_WM_WINDOW_TYPE_MENU
    } else if atom == a.A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU {
        A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU
    } else if atom == a.A__NET_WM_WINDOW_TYPE_POPUP_MENU {
        A__NET_WM_WINDOW_TYPE_POPUP_MENU
    } else if atom == a.A__NET_WM_WINDOW_TYPE_TOOLTIP {
        A__NET_WM_WINDOW_TYPE_TOOLTIP
    } else if atom == a.A__NET_WM_WINDOW_TYPE_NOTIFICATION {
        A__NET_WM_WINDOW_TYPE_NOTIFICATION
    } else {
        crate::data::XCB_ATOM_NONE
    }
}

/// Updates the _NET_WM_WINDOW_TYPE property.
///
/// C: src/window.c:239 window_update_type
pub fn window_update_type(t: &mut Tree, b: &mut dyn ConBackend, a: &Atoms, window: WindowId, reply: Option<&PropertyReply>) {
    let new_type = window_type_from_atom(a, xcb_get_preferred_window_type(a, reply));
    if new_type == crate::data::XCB_ATOM_NONE {
        return;
    }
    t.window_mut(window).window_type = new_type;
    run_assignments(t, b, Some(window));
}

/// WM_SIZE_HINTS flag bits (C: xcb_icccm.h).
pub const XCB_ICCCM_SIZE_HINT_US_POSITION: u32 = 1 << 0;
pub const XCB_ICCCM_SIZE_HINT_US_SIZE: u32 = 1 << 1;
pub const XCB_ICCCM_SIZE_HINT_P_POSITION: u32 = 1 << 2;
pub const XCB_ICCCM_SIZE_HINT_P_SIZE: u32 = 1 << 3;
pub const XCB_ICCCM_SIZE_HINT_P_MIN_SIZE: u32 = 1 << 4;
pub const XCB_ICCCM_SIZE_HINT_P_MAX_SIZE: u32 = 1 << 5;
pub const XCB_ICCCM_SIZE_HINT_P_RESIZE_INC: u32 = 1 << 6;
pub const XCB_ICCCM_SIZE_HINT_P_ASPECT: u32 = 1 << 7;
pub const XCB_ICCCM_SIZE_HINT_BASE_SIZE: u32 = 1 << 8;
pub const XCB_ICCCM_SIZE_HINT_P_WIN_GRAVITY: u32 = 1 << 9;

/// WM_HINTS flag bits (C: xcb_icccm.h).
pub const XCB_ICCCM_WM_HINT_INPUT: u32 = 1 << 0;
pub const XCB_ICCCM_WM_HINT_X_URGENCY: u32 = 1 << 8;

/// A parsed xcb_size_hints_t (only the fields i3 reads).
///
/// C: xcb_size_hints_t
#[derive(Debug, Clone, Copy, Default)]
pub struct SizeHints {
    pub flags: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub min_width: i32,
    pub min_height: i32,
    pub max_width: i32,
    pub max_height: i32,
    pub width_inc: i32,
    pub height_inc: i32,
    pub min_aspect_num: i32,
    pub min_aspect_den: i32,
    pub max_aspect_num: i32,
    pub max_aspect_den: i32,
    pub base_width: i32,
    pub base_height: i32,
}

/// Parse WM_NORMAL_HINTS property bytes into SizeHints.
///
/// C: xcb_icccm_get_wm_size_hints_from_reply
pub fn parse_size_hints(value: &[u8]) -> Option<SizeHints> {
    let words: Vec<i32> = value
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    // C: xcb_icccm_get_wm_size_hints_from_reply() reads only the fields the
    // reply actually carries (it fills the rest with 0), so a client which
    // publishes a truncated WM_NORMAL_HINTS list still gets its flags and the
    // sizes it did send. Only an empty reply is unusable.
    if words.is_empty() {
        return None;
    }
    // C: xcb_size_hints_t (the WM_SIZE_HINTS wire layout):
    //    flags, x, y, width, height, min_width, min_height, max_width,
    //    max_height, width_inc, height_inc, min_aspect(num, den),
    //    max_aspect(num, den), base_width, base_height, win_gravity
    let g = |i: usize| words.get(i).copied().unwrap_or(0);
    Some(SizeHints {
        flags: words[0] as u32,
        x: g(1),
        y: g(2),
        width: g(3),
        height: g(4),
        min_width: g(5),
        min_height: g(6),
        max_width: g(7),
        max_height: g(8),
        width_inc: g(9),
        height_inc: g(10),
        min_aspect_num: g(11),
        min_aspect_den: g(12),
        max_aspect_num: g(13),
        max_aspect_den: g(14),
        base_width: g(15),
        base_height: g(16),
    })
}

/// Updates the WM_NORMAL_HINTS.
///
/// C: src/window.c:257 window_update_normal_hints
pub fn window_update_normal_hints(
    t: &mut Tree,
    window: WindowId,
    reply: Option<&PropertyReply>,
    geom: Option<&mut GeometryReply>,
) -> bool {
    let mut changed = false;
    let size_hints: SizeHints = match reply {
        Some(r) => match parse_size_hints(&r.value) {
            Some(h) => h,
            None => return false,
        },
        None => return false,
    };

    macro_rules! assign_if_changed {
        ($orig:expr, $new:expr) => {{
            if $orig != $new {
                $orig = $new;
                changed = true;
            }
        }};
    }

    let w = &mut t.windows[window];

    if (size_hints.flags & XCB_ICCCM_SIZE_HINT_P_MIN_SIZE) != 0 {
        assign_if_changed!(w.min_width, size_hints.min_width);
        assign_if_changed!(w.min_height, size_hints.min_height);
    }

    if (size_hints.flags & XCB_ICCCM_SIZE_HINT_P_MAX_SIZE) != 0 {
        let max_width = size_hints.max_width.max(0);
        let max_height = size_hints.max_height.max(0);
        assign_if_changed!(w.max_width, max_width);
        assign_if_changed!(w.max_height, max_height);
    } else {
        assign_if_changed!(w.max_width, 0);
        assign_if_changed!(w.max_height, 0);
    }

    if (size_hints.flags & XCB_ICCCM_SIZE_HINT_P_RESIZE_INC) != 0 {
        if size_hints.width_inc > 0 && size_hints.width_inc < 0xFFFF {
            assign_if_changed!(w.width_increment, size_hints.width_inc);
        } else {
            assign_if_changed!(w.width_increment, 0);
        }
        if size_hints.height_inc > 0 && size_hints.height_inc < 0xFFFF {
            assign_if_changed!(w.height_increment, size_hints.height_inc);
        } else {
            assign_if_changed!(w.height_increment, 0);
        }
    } else {
        assign_if_changed!(w.width_increment, 0);
        assign_if_changed!(w.height_increment, 0);
    }

    // The base width / height is the desired size of the window.
    if (size_hints.flags & XCB_ICCCM_SIZE_HINT_BASE_SIZE) != 0
        && w.base_width >= 0
        && w.base_height >= 0
    {
        assign_if_changed!(w.base_width, size_hints.base_width);
        assign_if_changed!(w.base_height, size_hints.base_height);
    } else {
        assign_if_changed!(w.base_width, 0);
        assign_if_changed!(w.base_height, 0);
    }

    if let Some(geom) = geom {
        if ((size_hints.flags & XCB_ICCCM_SIZE_HINT_US_POSITION) != 0
            || (size_hints.flags & XCB_ICCCM_SIZE_HINT_P_POSITION) != 0)
            && ((size_hints.flags & XCB_ICCCM_SIZE_HINT_US_SIZE) != 0
                || (size_hints.flags & XCB_ICCCM_SIZE_HINT_P_SIZE) != 0)
        {
            geom.x = size_hints.x as i16;
            geom.y = size_hints.y as i16;
            geom.width = size_hints.width as u16;
            geom.height = size_hints.height as u16;
        }
    }

    // If no aspect ratio was set or if it was invalid, we ignore the hints
    if (size_hints.flags & XCB_ICCCM_SIZE_HINT_P_ASPECT) != 0
        && size_hints.min_aspect_num >= 0
        && size_hints.min_aspect_den > 0
        && size_hints.max_aspect_num >= 0
        && size_hints.max_aspect_den > 0
    {
        let min_aspect = size_hints.min_aspect_num as f64 / size_hints.min_aspect_den as f64;
        let max_aspect = size_hints.max_aspect_num as f64 / size_hints.max_aspect_den as f64;
        if (w.min_aspect_ratio - min_aspect).abs() > f64::EPSILON {
            w.min_aspect_ratio = min_aspect;
            changed = true;
        }
        if (w.max_aspect_ratio - max_aspect).abs() > f64::EPSILON {
            w.max_aspect_ratio = max_aspect;
            changed = true;
        }
    } else {
        assign_if_changed!(w.min_aspect_ratio, 0.0);
        assign_if_changed!(w.max_aspect_ratio, 0.0);
    }

    changed
}

/// Updates the WM_HINTS (we only care about the input focus handling part).
///
/// C: src/window.c:378 window_update_hints
pub fn window_update_hints(t: &mut Tree, window: WindowId, prop: Option<&PropertyReply>, mut urgency_hint: Option<&mut bool>) {
    if let Some(u) = urgency_hint.as_deref_mut() {
        *u = false;
    }

    let prop = match prop {
        Some(p) if !p.value.is_empty() => p,
        _ => return,
    };

    // WM_HINTS layout: flags, input, initial_state, icon pixmap, icon window,
    // icon mask, window group. We need flags (word 0) and input (word 1).
    let words = prop.words32();
    if words.len() < 2 {
        return;
    }
    let flags = words[0];
    let input = words[1] != 0;

    if (flags & XCB_ICCCM_WM_HINT_INPUT) != 0 {
        t.window_mut(window).doesnt_accept_focus = !input;
    }

    // C: *urgency_hint = (xcb_icccm_wm_hints_get_urgency(&hints) != 0)
    if let Some(u) = urgency_hint.as_deref_mut() {
        *u = (flags & XCB_ICCCM_WM_HINT_X_URGENCY) != 0;
    }
}

/// C: src/window.c:413-419 (Motif constants)
pub const MWM_HINTS_FLAGS_FIELD: usize = 0;
pub const MWM_HINTS_DECORATIONS_FIELD: usize = 2;
pub const MWM_HINTS_DECORATIONS: u32 = 1 << 1;
pub const MWM_DECOR_ALL: u32 = 1 << 0;
pub const MWM_DECOR_BORDER: u32 = 1 << 1;
pub const MWM_DECOR_TITLE: u32 = 1 << 3;

/// C: src/window.c:421 border_style_from_motif_value
pub fn border_style_from_motif_value(value: u32) -> BorderStyle {
    if (value & MWM_DECOR_ALL) != 0 {
        if (value & MWM_DECOR_TITLE) != 0 {
            if (value & MWM_DECOR_BORDER) != 0 {
                return BorderStyle::BS_NONE;
            }
            return BorderStyle::BS_PIXEL;
        }
        return BorderStyle::BS_NORMAL;
    }
    if (value & MWM_DECOR_TITLE) != 0 {
        return BorderStyle::BS_NORMAL;
    }
    if (value & MWM_DECOR_BORDER) != 0 {
        return BorderStyle::BS_PIXEL;
    }
    BorderStyle::BS_NONE
}

/// Updates the MOTIF_WM_HINTS. Returns whether hints were found and sets the
/// resulting border style.
///
/// C: src/window.c:465 window_update_motif_hints
pub fn window_update_motif_hints(
    t: &mut Tree,
    window: WindowId,
    prop: Option<&PropertyReply>,
    motif_border_style: &mut BorderStyle,
) -> bool {
    let prop = match prop {
        Some(p) => p,
        None => return false,
    };
    if prop.value.is_empty() {
        return false;
    }

    let words = prop.words32();
    let get = |i: usize| words.get(i).copied().unwrap_or(0);
    if (get(MWM_HINTS_FLAGS_FIELD) & MWM_HINTS_DECORATIONS) != 0 {
        *motif_border_style = border_style_from_motif_value(get(MWM_HINTS_DECORATIONS_FIELD));
        let _ = (t, window);
        return true;
    }
    false
}

/// Updates the WM_CLIENT_MACHINE.
///
/// C: src/window.c:507 window_update_machine
pub fn window_update_machine(t: &mut Tree, window: WindowId, prop: Option<&PropertyReply>) {
    let prop = match prop {
        Some(p) if !p.value.is_empty() => p,
        _ => return,
    };
    let machine = prop.first_string().unwrap_or_default();
    t.window_mut(window).machine = Some(machine);
}

/// Updates the _NET_WM_ICON.
///
/// C: src/window.c:521 window_update_icon -- the cairo surface creation is
/// Phase 7; the icon selection loop is ported 1:1 and the chosen icon size is
/// stored in the OpaqueIcon handle (width << 16 | height).
pub fn window_update_icon(t: &mut Tree, window: WindowId, prop: Option<&PropertyReply>) {
    let prop = match prop {
        Some(p) => p,
        None => return,
    };
    // C: if (!prop || prop->type != XCB_ATOM_CARDINAL || prop->format != 32)
    if prop.type_ != 6 || prop.format != 32 {
        return;
    }
    let words = prop.words32();
    if words.is_empty() {
        return;
    }
    let mut width: u32 = 0;
    let mut height: u32 = 0;
    let mut chosen_len: u64 = 0;
    let mut idx: usize = 0;
    let prop_value_len = words.len() as u64 * 4;

    // C: while (prop_value_len > 8 && prop_value[0] && prop_value[1])
    while (prop_value_len > 8) && idx + 1 < words.len() && words[idx] != 0 && words[idx + 1] != 0 {
        let cur_width = words[idx];
        let cur_height = words[idx + 1];
        let cur_len = cur_width as u64 * cur_height as u64;
        let expected_len = (cur_len + 2) * 4;
        if expected_len > prop_value_len {
            break;
        }
        // C: pref_size = render_deco_height() - logical_px(2); stubbed to 16
        let at_least_preferred_size = cur_width >= 16 && cur_height >= 16;
        let smaller_than_current = cur_width < width || cur_height < height;
        let larger_than_current = cur_width > width || cur_height > height;
        let not_yet_at_preferred = width < 16 || height < 16;
        if chosen_len == 0
            || (at_least_preferred_size && (smaller_than_current || not_yet_at_preferred))
            || (!at_least_preferred_size && not_yet_at_preferred && larger_than_current)
        {
            chosen_len = cur_len;
            width = cur_width;
            height = cur_height;
        }
        if width == 16 && height == 16 {
            break;
        }
        idx += (expected_len / 4) as usize;
    }

    if chosen_len == 0 {
        return;
    }

    // C: win->name_x_changed = true; (trigger a redraw)
    t.window_mut(window).name_x_changed = true;
    t.window_mut(window).icon = Some(crate::data::OpaqueIcon {
        handle: ((width as u64) << 16) | (height as u64),
    });
    // TODO(Phase 7): premultiplied-alpha conversion + cairo surface.
}

/// The "%p" of a container in C log messages.
///
/// The port has no heap pointers; the ConId's ffi value is unique per process
/// (and stable for the lifetime of the container), which is what the C pointer
/// conveys in a log.
pub fn con_ptr(con: ConId) -> String {
    format!("{:#x}", con.data().as_ffi())
}

/// C: src/manage.c:17 _match_depth
/// Returns the old frame window id if the depth had to be adjusted.
fn _match_depth(t: &mut Tree, b: &mut dyn ConBackend, win: WindowId, con: ConId) -> u32 {
    let mut old_frame = XCB_WINDOW_NONE;
    if t.cons[con].depth != t.windows[win].depth {
        old_frame = t.cons[con].frame.map(|f| f.id).unwrap_or(XCB_WINDOW_NONE);
        t.con_mut(con).depth = t.windows[win].depth;
        // C: x_con_reframe(con) -- recreate the frame with the new depth.
        b.x_con_init(t, con);
    }
    old_frame
}

/// C: src/manage.c:31 _remove_matches
/// Remove all match criteria, the first swallowed window wins.
fn _remove_matches(t: &mut Tree, con: ConId) {
    t.con_mut(con).swallow_head.clear();
}

/// The swallow criterion that matched a window.
///
/// C: src/con.c:886 con_for_window's `Match **match_out` is a pointer *into*
/// the matched container's `swallow_head`, which lets the caller remove exactly
/// that criterion (C: src/manage.c:365 `TAILQ_REMOVE(&(nc->swallow_head), match,
/// matches)`). The port stores the position instead of a pointer.
#[derive(Debug, Clone)]
pub struct FoundMatch {
    /// Index of the criterion in the matched container's `swallow_head`.
    pub index: usize,
    /// The criterion itself (C: `*match_out`).
    pub match_: Match,
}

/// Walk the subtree rooted at 'start' and return the first container whose
/// swallow spec matches the window (con_for_window with the match out-param).
///
/// C: src/con.c:886 con_for_window
pub fn con_for_window_with_match(
    t: &Tree,
    start: Option<ConId>,
    window: Option<WindowId>,
) -> (Option<ConId>, Option<FoundMatch>) {
    fn visit(
        t: &Tree,
        con: ConId,
        window: Option<WindowId>,
        found: &mut (Option<ConId>, Option<FoundMatch>),
    ) {
        if found.0.is_some() {
            return;
        }
        let node = &t.cons[con];
        for (index, m) in node.swallow_head.iter().enumerate() {
            if match_matches_window(t, m, window) {
                found.0 = Some(con);
                found.1 = Some(FoundMatch {
                    index,
                    match_: m.clone(),
                });
                return;
            }
        }
        for &child in &node.nodes_head {
            visit(t, child, window, found);
        }
        for &child in &node.floating_head {
            visit(t, child, window, found);
        }
    }

    let mut found: (Option<ConId>, Option<FoundMatch>) = (None, None);
    if let Some(s) = start {
        visit(t, s, window, &mut found);
    }
    found
}

/// Go through all existing windows (if the window manager is restarted) and
/// manage them.
///
/// C: src/manage.c:44 manage_existing_windows
pub fn manage_existing_windows(t: &mut Tree, b: &mut dyn ConBackend, x: &mut dyn XConn, a: &Atoms) {
    // Get the tree of windows whose parent is the root window (= all)
    let root = x.root_screen().root;
    let children = x.query_tree(root);

    // Call manage_window with the attributes for every window
    for child in children {
        manage_window(t, b, x, a, child, true);
    }
}

/// Restores the geometry of each window by reparenting it to the root window
/// at the position of its frame.
///
/// C: src/manage.c:78 restore_geometry
pub fn restore_geometry(t: &mut Tree, x: &mut dyn XConn, _a: &Atoms) {
    use crate::backend::XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT;
    let cons: Vec<ConId> = t.all_cons.iter().copied().collect();
    let root = x.root_screen().root;
    for con in cons {
        if let Some(window) = t.cons[con].window {
            // C: con->window_rect.width += (2 * con->border_width);
            let bw = t.cons[con].border_width;
            let w = t.con_mut(con);
            w.window_rect.width = w.window_rect.width.wrapping_add((2 * bw) as u32);
            w.window_rect.height = w.window_rect.height.wrapping_add((2 * bw) as u32);
            let window_id = t.windows[window].id;
            let window_rect = t.cons[con].window_rect;
            let rect = t.cons[con].rect;
            xcb_set_window_rect(x, window_id, window_rect);
            x.reparent_window(window_id, root, rect.x as i16, rect.y as i16);
        }
    }

    // Strictly speaking, this line doesn't really belong here, but since we
    // are syncing, let's un-register as a window manager first
    x.change_window_attributes(root, &[(XCB_CW_EVENT_MASK, XCB_EVENT_MASK_SUBSTRUCTURE_REDIRECT)]);

    // Make sure our changes reach the X server, we restart/exit now
    x.flush();
}

/// C: src/x.c:303 window_supports_protocol
pub fn window_supports_protocol(_t: &Tree, x: &mut dyn XConn, a: &Atoms, window: u32, atom: u32) -> bool {
    let reply = x.get_property(window, a.A_WM_PROTOCOLS, 0 /* XCB_GET_PROPERTY_TYPE_ANY */, 0, 100);
    let reply = match reply {
        Some(r) => r,
        None => return false,
    };
    for supported in reply.words32() {
        if supported == atom {
            return true;
        }
    }
    false
}

/// Do some sanity checks and then reparent the window.
///
/// C: src/manage.c:106 manage_window
pub fn manage_window(t: &mut Tree, b: &mut dyn ConBackend, x: &mut dyn XConn, a: &Atoms, window: u32, needs_to_be_mapped: bool) {
    // C: src/manage.c:113 DLOG("window 0x%08x\n", window);
    crate::log::dlog_at(
        file!(),
        "manage_window",
        line!(),
        &format!("window {:#010x}\n", window),
    );

    // C: geomc = xcb_get_geometry(conn, d) -- fetched first, discarded when the
    // early-return paths fire.
    let geomc = x.get_geometry(window);

    // Check if the window is mapped (it could be not mapped when initializing
    // and calling manage_window() for every window)
    let attr: WindowAttrReply = match x.get_window_attributes(window) {
        Some(attr) => attr,
        None => return, // C: "Could not get attributes"
    };

    if needs_to_be_mapped && attr.map_state != 2 /* XCB_MAP_STATE_VIEWABLE */ {
        return; // C: xcb_discard_reply(conn, geomc.sequence)
    }

    // Don't manage clients with the override_redirect flag
    if attr.override_redirect {
        return;
    }

    // Check if the window is already managed
    if con_by_window_id(t, window).is_some() {
        return;
    }

    // Get the initial geometry (position, size, ...)
    let mut geom: GeometryReply = match geomc {
        Some(g) => g,
        None => return, // C: "could not get geometry"
    };

    // Set a temporary event mask for the new window, consisting only of
    // PropertyChange and StructureNotify.
    let values = [(
        XCB_CW_EVENT_MASK,
        crate::backend::XCB_EVENT_MASK_PROPERTY_CHANGE | crate::backend::XCB_EVENT_MASK_STRUCTURE_NOTIFY,
    )];
    x.change_window_attributes(window, &values);

    // C: the GetProperty cookie burst (16 properties)
    let wm_type_reply = x.get_property(window, a.A__NET_WM_WINDOW_TYPE, 0, 0, u32::MAX);
    let strut_reply = x.get_property(window, a.A__NET_WM_STRUT_PARTIAL, 0, 0, u32::MAX);
    let state_reply = x.get_property(window, a.A__NET_WM_STATE, 0, 0, u32::MAX);
    let utf8_title_reply = x.get_property(window, a.A__NET_WM_NAME, 0, 0, 128);
    let leader_reply = x.get_property(window, a.A_WM_CLIENT_LEADER, 0, 0, u32::MAX);
    let transient_reply = x.get_property(window, crate::data::XCB_ATOM_WM_TRANSIENT_FOR, 0, 0, u32::MAX);
    let title_reply = x.get_property(window, crate::data::XCB_ATOM_WM_NAME, 0, 0, 128);
    let class_reply = x.get_property(window, crate::data::XCB_ATOM_WM_CLASS, 0, 0, 128);
    let role_reply = x.get_property(window, a.A_WM_WINDOW_ROLE, 0, 0, 128);
    let startup_id_reply = x.get_property(window, a.A__NET_STARTUP_ID, 0, 0, 512);
    let wm_hints_reply = x.get_property(window, crate::data::XCB_ATOM_WM_HINTS, 0, 0, 10);
    let wm_normal_hints_reply = x.get_property(window, crate::data::XCB_ATOM_WM_NORMAL_HINTS, 0, 0, 24);
    let motif_wm_hints_reply = x.get_property(window, a.A__MOTIF_WM_HINTS, 0, 0, 5 * 8);
    let wm_user_time_reply = x.get_property(window, a.A__NET_WM_USER_TIME, 0, 0, u32::MAX);
    let wm_desktop_reply = x.get_property(window, a.A__NET_WM_DESKTOP, 0, 0, u32::MAX);
    let wm_machine_reply = x.get_property(window, crate::data::XCB_ATOM_WM_CLIENT_MACHINE, 0, 0, u32::MAX);
    let wm_icon_reply = x.get_property(window, a.A__NET_WM_ICON, 0, 0, u32::MAX);
    let _ = startup_id_reply;

    // C: cwindow = scalloc(...); cwindow->id = window; depth = get_visual_depth
    let win = t.windows.insert(crate::data::Window {
        id: window,
        depth: get_visual_depth(x, attr.visual),
        wm_desktop: NET_WM_DESKTOP_NONE,
        ..Default::default()
    });
    t.window_index.insert(window, win);

    // C: xcb_grab_buttons(conn, window, bindings_get_buttons_to_grab())
    // (src/manage.c:202-203) — without the real list the wheel bindings
    // (button4/5 with --whole-window) never reach the WM because the client
    // keeps the button events.
    let buttons = crate::bindings::bindings_get_buttons_to_grab(t);
    x.grab_buttons(window, &buttons);

    // update as much information as possible so far (some replies may be None)
    window_update_class(t, win, class_reply.as_ref());
    window_update_name_legacy(t, win, title_reply.as_ref());
    window_update_name(t, win, utf8_title_reply.as_ref());
    window_update_icon(t, win, wm_icon_reply.as_ref());
    window_update_leader(t, win, leader_reply.as_ref());
    window_update_transient_for(t, win, transient_reply.as_ref());
    window_update_strut_partial(t, win, strut_reply.as_ref());
    window_update_role(t, win, role_reply.as_ref());
    let mut urgency_hint = false;
    window_update_hints(t, win, wm_hints_reply.as_ref(), Some(&mut urgency_hint));
    let mut motif_border_style = BorderStyle::BS_NORMAL;
    let has_mwm_hints = window_update_motif_hints(t, win, motif_wm_hints_reply.as_ref(), &mut motif_border_style);
    window_update_normal_hints(t, win, wm_normal_hints_reply.as_ref(), Some(&mut geom));
    window_update_machine(t, win, wm_machine_reply.as_ref());

    // Get _NET_WM_DESKTOP if it was set.
    t.window_mut(win).wm_desktop = NET_WM_DESKTOP_NONE;
    if let Some(r) = wm_desktop_reply.as_ref() {
        if !r.value.is_empty() {
            if let Some(&v) = r.words32().first() {
                t.window_mut(win).wm_desktop = v;
            }
        }
    }

    // check if the window needs WM_TAKE_FOCUS
    let needs_take_focus = window_supports_protocol(t, x, a, window, a.A_WM_TAKE_FOCUS);
    t.window_mut(win).needs_take_focus = needs_take_focus;

    // read the preferred _NET_WM_WINDOW_TYPE atom
    let preferred_type = xcb_get_preferred_window_type(a, wm_type_reply.as_ref());
    // C stores the atom; the port stores the criterion id of the same atom so
    // that for_window [window_type=...] matches (see window_type_from_atom).
    t.window_mut(win).window_type = window_type_from_atom(a, preferred_type);

    // Where to start searching for a container that swallows the new one?
    let mut search_at = t.croot;

    if xcb_reply_contains_atom(wm_type_reply.as_ref(), a.A__NET_WM_WINDOW_TYPE_DOCK) {
        if let Some(output) = get_output_containing(t, geom.x.max(0) as u32, geom.y.max(0) as u32) {
            let _ = output_primary_name(t, output); // C: DLOG("Starting search at output %s")
            search_at = t.outputs[output].con;
        }

        // find out the desired position of this dock window
        let (top, bottom) = {
            let w = t.window(win);
            (w.reserved.top, w.reserved.bottom)
        };
        let dock = if top > 0 && bottom == 0 {
            // C: src/manage.c:257 DLOG("Top dock client\n");
            crate::log::dlog_at(file!(), "manage_window", line!(), "Top dock client\n");
            WindowDock::W_DOCK_TOP
        } else if top == 0 && bottom > 0 {
            // C: src/manage.c:261 DLOG("Bottom dock client\n");
            crate::log::dlog_at(file!(), "manage_window", line!(), "Bottom dock client\n");
            WindowDock::W_DOCK_BOTTOM
        } else {
            // C: src/manage.c:264 DLOG("Ignoring invalid reserved edges
            // (_NET_WM_STRUT_PARTIAL), using position as fallback:\n");
            crate::log::dlog_at(
                file!(),
                "manage_window",
                line!(),
                &format!(
                    "Ignoring invalid reserved edges (_NET_WM_STRUT_PARTIAL), using position as fallback: top = {}, bottom = {}\n",
                    top, bottom
                ),
            );
            // C: ignoring invalid reserved edges, using position as fallback
            let search_rect = search_at.map(|s| t.cons[s].rect).unwrap_or_default();
            if (geom.y as i32) < (search_rect.height as i32 / 2) {
                // C: DLOG("geom->y = %d < rect.height / 2 = %d, it is a top
                // dock client\n", ...)
                crate::log::dlog_at(
                    file!(),
                    "manage_window",
                    line!(),
                    &format!(
                        "geom->y = {} < rect.height / 2 = {}, it is a top dock client\n",
                        geom.y,
                        search_rect.height / 2
                    ),
                );
                WindowDock::W_DOCK_TOP
            } else {
                // C: DLOG("geom->y = %d >= rect.height / 2 = %d, it is a
                // bottom dock client\n", ...)
                crate::log::dlog_at(
                    file!(),
                    "manage_window",
                    line!(),
                    &format!(
                        "geom->y = {} >= rect.height / 2 = {}, it is a bottom dock client\n",
                        geom.y,
                        search_rect.height / 2
                    ),
                );
                WindowDock::W_DOCK_BOTTOM
            }
        };
        t.window_mut(win).dock = dock;
    }

    // See if any container swallows this new window
    t.window_mut(win).swallowed = false;
    let (nc0, match0) = con_for_window_with_match(t, search_at, Some(win));
    // C: src/manage.c:283 const bool match_from_restart_mode = (match && match->restart_mode);
    let match_from_restart_mode = match0.as_ref().map(|f| f.match_.restart_mode).unwrap_or(false);
    let mut nc = match nc0 {
        Some(nc) => nc,
        None => {
            // C: no swallow match -- assignment / wm_desktop / startup / focused
            let nc;
            let assignment = assignment_for(t, Some(win), A_TO_WORKSPACE)
                .or_else(|| assignment_for(t, Some(win), A_TO_WORKSPACE_NUMBER));
            if let Some(assign_idx) = assignment {
                let mut assigned_ws = None;
                if t.assignments[assign_idx].type_ == A_TO_WORKSPACE_NUMBER {
                    let name = t.assignments[assign_idx].dest.clone().unwrap_or_default();
                    let parsed_num = ws_name_to_number(&name);
                    assigned_ws = get_existing_workspace_by_num(t, parsed_num);
                }
                // C: A_TO_WORKSPACE type assignment or fallback from number
                let assigned_ws = match assigned_ws {
                    Some(ws) => ws,
                    None => workspace_get(t, b, &t.assignments[assign_idx].dest.clone().unwrap_or_default()),
                };
                let descend = con_descend_tiling_focused(t, assigned_ws);
                nc = if t.cons[descend].r#type == ConType::CT_WORKSPACE {
                    tree_open_con(t, b, Some(descend), Some(win))
                } else {
                    tree_open_con(t, b, t.cons[descend].parent, Some(win))
                };

                // set the urgency hint on the window if the workspace is not visible
                if !workspace_is_visible(t, assigned_ws) {
                    urgency_hint = true;
                }
            } else if t.windows[win].wm_desktop != NET_WM_DESKTOP_NONE
                && t.windows[win].wm_desktop != NET_WM_DESKTOP_ALL
            {
                // If _NET_WM_DESKTOP is set to a specific desktop, open it there.
                if let Some(wm_desktop_ws) = ewmh_get_workspace_by_index(t, t.windows[win].wm_desktop) {
                    let descend = con_descend_tiling_focused(t, wm_desktop_ws);
                    nc = if t.cons[descend].r#type == ConType::CT_WORKSPACE {
                        tree_open_con(t, b, Some(descend), Some(win))
                    } else {
                        tree_open_con(t, b, t.cons[descend].parent, Some(win))
                    };
                } else {
                    nc = tree_open_con(t, b, None, Some(win));
                }
            } else {
                // C: startup_ws case is handled in the startup phase; open at
                // the currently focused position.
                let focused = t.focused;
                if let Some(f) = focused {
                    if t.cons[f].r#type == ConType::CT_CON && con_accepts_window(t, f) {
                        nc = f;
                    } else {
                        nc = tree_open_con(t, b, None, Some(win));
                    }
                } else {
                    nc = tree_open_con(t, b, None, Some(win));
                }
            }

            if let Some(assign_idx) = assignment_for(t, Some(win), A_TO_OUTPUT) {
                let name = t.assignments[assign_idx].dest.clone().unwrap_or_default();
                crate::con::con_move_to_output_name(t, b, nc, &name, true);
            }
            nc
        }
    };

    // C: src/manage.c:275 DLOG("Initial geometry: (%d, %d, %d, %d)\n", ...)
    crate::log::dlog_at(
        file!(),
        "manage_window",
        line!(),
        &format!(
            "Initial geometry: ({}, {}, {}, {})\n",
            geom.x, geom.y, geom.width, geom.height
        ),
    );

    // C: src/manage.c:357-371 -- M_BELOW inserts the new window as a child of
    // the container which was matched (e.g. dock areas); otherwise the matched
    // container is *replaced* (stored layouts), in which case its criterion is
    // removed because it must only be valid once.
    if let Some(found) = match0 {
        if found.match_.insert_where == crate::data::MatchInsertWhere::M_BELOW {
            // C: src/manage.c:359 — nc = tree_open_con(nc, cwindow); The new
            // con is the one that holds the window; keeping the matched con
            // (the dockarea) here would turn the dockarea itself into the
            // container around the client window.
            nc = tree_open_con(t, b, Some(nc), Some(win));
        } else {
            // C: DLOG("Removing match %p from container %p\n", match, nc);
            crate::log::dlog_at(
                file!(),
                "manage_window",
                line!(),
                &format!(
                    "Removing match {} from container {}\n",
                    found.index,
                    con_ptr(nc)
                ),
            );
            // C: TAILQ_REMOVE(&(nc->swallow_head), match, matches); match_free(match);
            t.con_mut(nc).swallow_head.remove(found.index);
            t.window_mut(win).swallowed = true;
        }
    }

    // C: src/manage.c:373 DLOG("new container = %p\n", nc);
    crate::log::dlog_at(
        file!(),
        "manage_window",
        line!(),
        &format!("new container = {}\n", con_ptr(nc)),
    );

    // C: if (nc->window != NULL && nc->window != cwindow) -- placeholder handling
    if let Some(old_win) = t.cons[nc].window {
        if old_win != win && !restore_kill_placeholder(t, old_win) {
            // C: DLOG("Uh?! Container without a placeholder, but with a window,
            // has swallowed this to-be-managed window?!\n")
        } else {
            /* Remove remaining criteria, the first swallowed window wins. */
            _remove_matches(t, nc);
        }
        if old_win != win {
            window_free(t, old_win);
            let _old_frame = _match_depth(t, b, win, nc);
        }
    }
    t.con_mut(nc).window = Some(win);
    // C: src/manage.c:424 x_reinit(nc) — reset the cached frame state
    // (child_mapped = false, window_rect = 0) now that the client window is
    // the child of this con's frame. Without the reset the new client is never
    // mapped (child_mapped is still true from the placeholder/previous window).
    b.x_reinit(t, nc);

    // C: nc->border_width = geom->border_width;
    t.con_mut(nc).border_width = geom.border_width as i32;

    // C: x_set_name(nc, "[i3 con] container around %p")
    let name = format!("[i3 con] container around {:#x}", window);
    b.x_set_name(t, nc, &name);

    // handle fullscreen containers
    let ws = con_get_workspace(t, nc);
    let mut fs = con_get_fullscreen_covering_ws(t, ws);

    if xcb_reply_contains_atom(state_reply.as_ref(), a.A__NET_WM_STATE_FULLSCREEN) {
        // If this window is already fullscreen (after restarting!), skip
        // toggling fullscreen, that would drop it out of fullscreen mode.
        if fs != Some(nc) {
            if let Some(output) = get_output_with_dimensions(t, geom) {
                crate::con::con_move_to_output(t, b, nc, output, false);
            }
            crate::con::con_toggle_fullscreen(t, b, nc, crate::data::FullscreenMode::CF_OUTPUT as i32);
        }
        fs = None;
    }

    let mut set_focus = false;

    if fs.is_none() {
        // C: src/manage.c:445 DLOG("Not in fullscreen mode, focusing\n");
        crate::log::dlog_at(
            file!(),
            "manage_window",
            line!(),
            "Not in fullscreen mode, focusing\n",
        );
        if t.windows[win].dock == WindowDock::W_NODOCK {
            // Check that the workspace is visible and on the same output as
            // the current focused container.
            let current_output = t.focused.and_then(|f| con_get_output(t, f));
            let target_output = ws.and_then(|w| con_get_output(t, w));
            if ws.map(|w| workspace_is_visible(t, w)).unwrap_or(false) && current_output == target_output {
                // C: src/manage.c:433-439 -- a window that matched with
                // restart_mode == true must not steal the focus, otherwise the
                // focus stack we just restored from the layout gets reordered.
                if !match_from_restart_mode {
                    set_focus = true;
                } else {
                    crate::log::dlog_at(
                        file!(),
                        "manage_window",
                        line!(),
                        "not focusing, matched with restart_mode == true\n",
                    );
                }
            }
        }
    } else {
        // C: insert the new container in the focus stack *after* the currently
        // focused (fullscreen) con.
        if let Some(parent) = t.cons[nc].parent {
            let first = t.cons[parent].focus_head.front().copied();
            if first != Some(nc) {
                let c = t.con_mut(parent);
                c.focus_head.retain(|&x| x != nc);
                if let Some(f) = first {
                    let pos = c.focus_head.iter().position(|&x| x == f).map(|p| p + 1).unwrap_or(0);
                    c.focus_head.insert(pos, nc);
                }
            }
        }
    }

    // set floating if necessary
    let mut want_floating = false;
    let wmax = (t.windows[win].max_width, t.windows[win].max_height);
    let wmin = (t.windows[win].min_width, t.windows[win].min_height);
    if xcb_reply_contains_atom(wm_type_reply.as_ref(), a.A__NET_WM_WINDOW_TYPE_DIALOG)
        || xcb_reply_contains_atom(wm_type_reply.as_ref(), a.A__NET_WM_WINDOW_TYPE_UTILITY)
        || xcb_reply_contains_atom(wm_type_reply.as_ref(), a.A__NET_WM_WINDOW_TYPE_TOOLBAR)
        || xcb_reply_contains_atom(wm_type_reply.as_ref(), a.A__NET_WM_WINDOW_TYPE_NOTIFICATION)
        || xcb_reply_contains_atom(wm_type_reply.as_ref(), a.A__NET_WM_WINDOW_TYPE_SPLASH)
        || xcb_reply_contains_atom(state_reply.as_ref(), a.A__NET_WM_STATE_MODAL)
        || (wmax.0 > 0 && wmax.1 > 0 && wmin.1 == wmax.1 && wmin.0 == wmax.0)
    {
        want_floating = true;
    }

    if xcb_reply_contains_atom(state_reply.as_ref(), a.A__NET_WM_STATE_STICKY) {
        t.con_mut(nc).sticky = true;
    }

    // We ignore the hint for an internal workspace because windows in the
    // scratchpad also have this value.
    if t.windows[win].wm_desktop == NET_WM_DESKTOP_ALL
        && ws.map(|w| !con_is_internal(t, w)).unwrap_or(true)
    {
        t.con_mut(nc).sticky = true;
        want_floating = true;
    }

    if t.windows[win].transient_for != XCB_WINDOW_NONE
        || (t.windows[win].leader != XCB_WINDOW_NONE
            && t.windows[win].leader != t.windows[win].id
            && con_by_window_id(t, t.windows[win].leader).is_some())
    {
        want_floating = true;
    }

    // C: dock clients cannot be floating, that makes no sense
    if t.windows[win].dock != crate::data::WindowDock::W_NODOCK {
        want_floating = false;
    }

    // C: src/manage.c:520-522 — store the requested geometry. The width/height
    // gets raised to at least 75x50 when entering floating mode; the original
    // geometry stays here because dock clients use it.
    if t.cons[nc].geometry.width == 0 {
        let rect = crate::data::Rect::new(
            geom.x.max(0) as u32,
            geom.y.max(0) as u32,
            geom.width as u32,
            geom.height as u32,
        );
        t.con_mut(nc).geometry = rect;
    }

    if want_floating {
        if floating_enable(t, b, nc, true) {
            t.con_mut(nc).floating = crate::data::FloatingState::FLOATING_AUTO_ON;
        }
    }

    if has_mwm_hints {
        if want_floating {
            con_set_border_style(t, nc, motif_border_style, t.config.default_floating_border_width);
        } else {
            con_set_border_style(t, nc, motif_border_style, t.config.default_border_width);
        }
    }

    // explicitly set the border width to the default
    if t.cons[nc].current_border_width == -1 {
        t.con_mut(nc).current_border_width = if want_floating {
            t.config.default_floating_border_width
        } else {
            t.config.default_border_width
        };
    }

    // to avoid getting an UnmapNotify event due to reparenting, we temporarily
    // declare no interest in any state change event of this window
    x.change_window_attributes(window, &[(XCB_CW_EVENT_MASK, crate::data::XCB_ATOM_NONE)]);

    // C: xcb_reparent_window_checked(conn, window, nc->frame.id, 0, 0)
    let frame = t.cons[nc].frame.map(|f| f.id).unwrap_or(XCB_WINDOW_NONE);
    x.reparent_window(window, frame, 0, 0);

    // C: values[0] = CHILD_EVENT_MASK & ~XCB_EVENT_MASK_ENTER_WINDOW
    x.change_window_attributes(
        window,
        &[(
            XCB_CW_EVENT_MASK,
            crate::x::CHILD_EVENT_MASK & !crate::backend::XCB_EVENT_MASK_ENTER_WINDOW,
        )],
    );
    x.flush();

    // Put the client inside the save set.
    x.change_save_set(0 /* XCB_SET_MODE_INSERT */, window);

    // C: shape_supported -> select_input + query extents (Phase 7)
    x.shape_select_input(window, true);

    // Check if any assignments match
    run_assignments(t, b, Some(win));

    // 'ws' may be invalid because of the assignments
    let ws = con_get_workspace(t, nc);

    // If this window was put onto an invisible workspace (via assignments), we
    // render this workspace.
    let mut set_focus = set_focus;
    if let Some(ws) = ws {
        if !workspace_is_visible(t, ws) {
            // C: ws->rect = ws->parent->rect; render_con(ws);
            if let Some(parent) = t.cons[ws].parent {
                let prect = t.cons[parent].rect;
                t.con_mut(ws).rect = prect;
            }
            render_con(t, b, ws);
            set_focus = false;
        }
    }
    if let Some(root_con) = t.croot {
        render_con(t, b, root_con);
    }

    t.window_mut(win).managed_since = unix_time_now();

    // Send an event about window creation
    b.ipc_send_window_event(t, "new", nc);

    if set_focus && assignment_for(t, Some(win), A_NO_FOCUS).is_some() {
        // The first window on a workspace should always be focused.
        if con_num_windows(t, ws) == 1 {
            // C: ignoring no_focus
        } else {
            set_focus = false;
        }
    }

    if set_focus {
        // C: _NET_WM_USER_TIME set to 0 -> not focusing
        if let Some(r) = wm_user_time_reply.as_ref() {
            if !r.value.is_empty() {
                if let Some(&0) = r.words32().first() {
                    set_focus = false;
                }
            }
        }
    }

    // Even if the client doesn't want focus, we still need to focus the
    // container to not break focus workflows.
    if t.windows[win].doesnt_accept_focus && !t.windows[win].needs_take_focus {
        set_focus = false;
    }

    // Defer setting focus after the 'new' event has been sent
    if set_focus && t.cons[nc].mapped {
        // C: src/manage.c:661 DLOG("Now setting focus.\n");
        crate::log::dlog_at(file!(), "manage_window", line!(), "Now setting focus.\n");
        con_activate(t, b, nc);
    }

    tree_render(t, b);

    // C: if (old_frame != XCB_NONE) xcb_destroy_window(conn, old_frame)
    //    (destroyed after rendering; see _match_depth above).

    // Windows might get managed with the urgency hint already set.
    con_set_urgency(t, b, nc, urgency_hint);

    // Update _NET_WM_DESKTOP. We invalidate the cached value first.
    t.window_mut(win).wm_desktop = NET_WM_DESKTOP_NONE;
    ewmh_update_wm_desktop(t, x, a);

    // If a sticky window was mapped onto another workspace, pop it to the front.
    crate::output::output_push_sticky_windows(t, b, t.focused);

    // Not in C (it has no exit statement in manage_window): pairs with the
    // entry DLOG so that "-d all" shows where one manage_window() ends.
    crate::log::dlog_at(
        file!(),
        "manage_window",
        line!(),
        &format!("done, window {:#010x} is managed\n", window),
    );
}

/// C: get_visual_depth (src/window.c family) -- reverse lookup of the depth.
/// TODO(Phase 7): full depth iteration; the root depth matches the common case.
pub fn get_visual_depth(x: &mut dyn XConn, visual: u32) -> u16 {
    let screen = x.root_screen();
    if screen.root_visual == visual {
        return screen.root_depth;
    }
    24
}

/// C: src/randr.c get_output_with_dimensions (used by manage.c for fullscreen
/// windows): find the active output whose rect equals the given geometry.
pub fn get_output_with_dimensions(t: &Tree, geom: GeometryReply) -> Option<usize> {
    for (idx, output) in t.outputs.iter().enumerate() {
        if !output.active {
            continue;
        }
        if output.rect.x == geom.x.max(0) as u32
            && output.rect.y == geom.y.max(0) as u32
            && output.rect.width == geom.width as u32
            && output.rect.height == geom.height as u32
        {
            return Some(idx);
        }
    }
    None
}

/// C: unix time (src/manage.c: cwindow->managed_since = time(NULL))
pub fn unix_time_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// C: src/restore_layout.c:273 restore_kill_placeholder
///
/// Kill the placeholder window, if the given window is one of the placeholder
/// windows created by restore_open_placeholder_windows(). Returns true when a
/// placeholder was destroyed. C calls this with `nc->window->id` from
/// src/manage.c:380 and :725; everything else (reparenting / freeing the client
/// Window) is done by the callers, exactly like in C.
pub fn restore_kill_placeholder(t: &Tree, window: WindowId) -> bool {
    match t.windows.get(window) {
        Some(w) => crate::restore_layout::restore_kill_placeholder(w.id),
        None => {
            // C: the Window always exists when this is reached; be defensive
            // instead of panicking on a freed window id.
            crate::log::debuglog("restore_kill_placeholder: unknown window\n");
            false
        }
    }
}

/// C: src/manage.c:693 placeholder_for_con
pub fn placeholder_for_con(t: &Tree, con: ConId) -> Option<ConId> {
    // Make sure this window hasn't already been swallowed.
    let window = t.cons[con].window?;
    if t.windows[window].swallowed {
        return None;
    }
    let (nc, _m) = con_for_window_with_match(t, t.croot, Some(window));
    let nc = nc?;
    let nc_window = t.cons[nc].window?;
    if nc_window == window {
        return None;
    }
    // Make sure the placeholder that wants to swallow this window didn't spawn
    // after the window to follow current behavior.
    if t.windows[nc_window].managed_since > t.windows[window].managed_since {
        return None;
    }
    Some(nc)
}

/// C: src/manage.c:717 remanage_window
///
/// Remanages a window: performs a swallow check and runs assignments.
/// Returns the con for the window regardless if it updated.
pub fn remanage_window(t: &mut Tree, b: &mut dyn ConBackend, _x: &mut dyn XConn, _a: &Atoms, con: ConId) -> ConId {
    let nc = match placeholder_for_con(t, con) {
        Some(nc) => nc,
        None => {
            // The con is not updated, just run assignments
            let window = t.cons[con].window;
            run_assignments(t, b, window);
            return con;
        }
    };

    let con_window = t.cons[con].window.expect("con_has_managed_window");
    let nc_old_window = t.cons[nc].window.expect("placeholder has window");
    if !restore_kill_placeholder(t, nc_old_window) {
        // C: DLOG("Uh?! Container without a placeholder, ...")
    } else {
        _remove_matches(t, nc);
    }
    window_free(t, nc_old_window);

    let _old_frame = _match_depth(t, b, con_window, nc);

    // C: x_reparent_child(nc, con)
    b.x_reparent_child(t, nc, con);

    let moved_workspaces = con_get_workspace(t, nc) != con_get_workspace(t, con);

    con_merge_into(t, b, con, nc);

    // C: run_assignments(nc->window)
    let nc_window = t.cons[nc].window;
    run_assignments(t, b, nc_window);

    if moved_workspaces {
        // If the window is associated with a startup sequence, delete it so
        // child windows won't be created on the old workspace.
        if let Some(w) = nc_window {
            b.startup_sequence_delete_by_window(t, w);
        }
        // C: ewmh_update_wm_desktop() -- routed through the server after remanage.
    }

    if let Some(w) = nc_window {
        t.window_mut(w).swallowed = true;
    }
    nc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;

    fn atom_conn() -> (RecordingConn, Atoms) {
        let mut x = RecordingConn::new(0x50);
        let a = Atoms::intern_all(&mut x);
        (x, a)
    }

    #[test]
    fn window_update_class_two_strings() {
        // C: WM_CLASS property is "instance\0class\0"
        let mut t = Tree::new();
        let win = t.windows.insert(crate::data::Window::default());
        let mut value = b"instance".to_vec();
        value.push(0);
        value.extend_from_slice(b"XTerm");
        value.push(0);
        let prop = PropertyReply { type_: 0, format: 8, value };
        window_update_class(&mut t, win, Some(&prop));
        assert_eq!(t.windows[win].class_instance.as_deref(), Some("instance"));
        assert_eq!(t.windows[win].class_class.as_deref(), Some("XTerm"));
    }

    #[test]
    fn window_update_name_truncates_at_nul() {
        // C: "Truncate the name at the first zero byte. See #3515."
        let mut t = Tree::new();
        let win = t.windows.insert(crate::data::Window::default());
        let mut value = b"hello".to_vec();
        value.push(0);
        value.extend_from_slice(b"ignored");
        let prop = PropertyReply { type_: 0, format: 8, value };
        window_update_name(&mut t, win, Some(&prop));
        assert_eq!(t.windows[win].name.as_deref(), Some("hello"));
        assert!(t.windows[win].uses_net_wm_name);
    }

    #[test]
    fn window_update_name_legacy_ignored_when_net_wm_name() {
        let mut t = Tree::new();
        let win = t.windows.insert(crate::data::Window {
            uses_net_wm_name: true,
            ..Default::default()
        });
        let prop = PropertyReply { type_: 0, format: 8, value: b"legacy".to_vec() };
        window_update_name_legacy(&mut t, win, Some(&prop));
        assert!(t.windows[win].name.is_none(), "legacy update must be ignored");
    }

    #[test]
    fn window_update_strut_partial_applies() {
        let mut t = Tree::new();
        let win = t.windows.insert(crate::data::Window::default());
        let prop = PropertyReply {
            type_: 6,
            format: 32,
            value: [10u32, 20, 30, 40].iter().flat_map(|v| v.to_le_bytes()).collect(),
        };
        window_update_strut_partial(&mut t, win, Some(&prop));
        let r = t.windows[win].reserved;
        assert_eq!((r.left, r.right, r.top, r.bottom), (10, 20, 30, 40));
    }

    #[test]
    fn motif_border_styles() {
        // C: src/window.c:421 border_style_from_motif_value
        assert_eq!(border_style_from_motif_value(MWM_DECOR_ALL), BorderStyle::BS_NORMAL);
        assert_eq!(border_style_from_motif_value(MWM_DECOR_ALL | MWM_DECOR_TITLE), BorderStyle::BS_PIXEL);
        assert_eq!(border_style_from_motif_value(MWM_DECOR_ALL | MWM_DECOR_TITLE | MWM_DECOR_BORDER), BorderStyle::BS_NONE);
        assert_eq!(border_style_from_motif_value(MWM_DECOR_TITLE), BorderStyle::BS_NORMAL);
        assert_eq!(border_style_from_motif_value(MWM_DECOR_BORDER), BorderStyle::BS_PIXEL);
        assert_eq!(border_style_from_motif_value(0), BorderStyle::BS_NONE);
    }

    #[test]
    fn window_update_motif_hints_decorations_field() {
        let mut t = Tree::new();
        let win = t.windows.insert(crate::data::Window::default());
        let prop = PropertyReply {
            type_: 6,
            format: 32,
            value: [MWM_HINTS_DECORATIONS, 0, MWM_DECOR_ALL].iter().flat_map(|v| v.to_le_bytes()).collect(),
        };
        let mut style = BorderStyle::BS_NONE;
        assert!(window_update_motif_hints(&mut t, win, Some(&prop), &mut style));
        assert_eq!(style, BorderStyle::BS_NORMAL);
    }

    #[test]
    fn size_hints_parse_and_apply() {
        let mut t = Tree::new();
        let win = t.windows.insert(crate::data::Window::default());
        let flags = XCB_ICCCM_SIZE_HINT_P_MIN_SIZE | XCB_ICCCM_SIZE_HINT_P_MAX_SIZE
            | XCB_ICCCM_SIZE_HINT_P_RESIZE_INC | XCB_ICCCM_SIZE_HINT_P_ASPECT;
        // C: xcb_size_hints_t — flags, x y w h, min, max, inc, aspect, base,
        // gravity (18 CARD32 values, no padding).
        let words: Vec<i32> = vec![
            flags as i32,
            0, 0, 0, 0,      // x y w h
            320, 240,        // min
            640, 480,        // max
            16, 16,          // inc
            4, 3, 4, 3,      // aspect
            0, 0,            // base
            0,               // gravity
        ];
        let value: Vec<u8> = words.iter().flat_map(|v| v.to_le_bytes()).collect();
        let reply = PropertyReply { type_: 6, format: 32, value };
        let mut geom = GeometryReply::default();
        let changed = window_update_normal_hints(&mut t, win, Some(&reply), Some(&mut geom));
        assert!(changed);
        assert_eq!(t.windows[win].min_width, 320);
        assert_eq!(t.windows[win].max_width, 640);
        assert_eq!(t.windows[win].width_increment, 16);
        assert!((t.windows[win].min_aspect_ratio - 4.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn window_update_hints_urgency() {
        // C: XUrgencyHint is bit 8 in WM_HINTS flags.
        let mut t = Tree::new();
        let win = t.windows.insert(crate::data::Window::default());
        let prop = PropertyReply {
            type_: 6,
            format: 32,
            value: [(XCB_ICCCM_WM_HINT_X_URGENCY | XCB_ICCCM_WM_HINT_INPUT) as u32, 1]
                .iter().flat_map(|v| v.to_le_bytes()).collect(),
        };
        let mut urgency = false;
        window_update_hints(&mut t, win, Some(&prop), Some(&mut urgency));
        assert!(urgency);
        assert!(!t.windows[win].doesnt_accept_focus);
    }

    #[test]
    fn manage_window_ignores_override_redirect_and_unviewable() {
        let (mut x, a) = atom_conn();
        let mut t = Tree::new();
        let b = &mut NullBackend;
        // not viewable -> not managed
        x.attrs.insert(0x1, WindowAttrReply { map_state: 0, override_redirect: false, visual: 0x21 });
        manage_window(&mut t, b, &mut x, &a, 0x1, true);
        assert!(t.windows.is_empty());
        // override_redirect -> not managed
        x.attrs.insert(0x2, WindowAttrReply { map_state: 2, override_redirect: true, visual: 0x21 });
        manage_window(&mut t, b, &mut x, &a, 0x2, true);
        assert!(t.windows.is_empty());
    }
}
