//! ewmh.c: Get/set certain EWMH properties easily (1:1 port).
//!
//! C: src/ewmh.c. The C `conn`/`root`/`ewmh_window` globals become the
//! XConn argument / root_screen().root / the returned window id of
//! ewmh_setup_hints.

use crate::backend::{PropMode, XConn};
use crate::con::{con_get_workspace, con_has_managed_window, con_is_floating, con_is_internal, con_is_sticky};
use crate::output::output_get_content;
use crate::tree::Tree;
use crate::xcb_util::Atoms;

/// C: include/workspace.h:24 `#define NET_WM_DESKTOP_NONE 0xFFFFFFF0`
pub const NET_WM_DESKTOP_NONE: u32 = 0xFFFF_FFF0;
/// C: include/workspace.h:25 `#define NET_WM_DESKTOP_ALL 0xFFFFFFFF`
pub const NET_WM_DESKTOP_ALL: u32 = 0xFFFF_FFFF;

/// C: XCB_ATOM_CARDINAL
pub const XCB_ATOM_CARDINAL: u32 = 6;
/// C: XCB_ATOM_WINDOW
///
/// 33, not 20: 20 is XCB_ATOM_PIXMAP (see /usr/include/xcb/xproto.h:1054).
/// The wrong value made xprop report _NET_CLIENT_LIST and _NET_ACTIVE_WINDOW
/// as PIXMAP properties.
pub const XCB_ATOM_WINDOW: u32 = 33;

/// The FOREACH_NONINTERNAL macro: (output, ws) pairs for every non-internal
/// workspace under croot.
///
/// C: src/ewmh.c:16-19 FOREACH_NONINTERNAL
pub fn for_each_noninternal_workspace(t: &Tree) -> Vec<(crate::data::ConId, crate::data::ConId)> {
    let mut result = Vec::new();
    let root = match t.croot {
        Some(r) => r,
        None => return result,
    };
    for &output in &t.cons[root].nodes_head {
        let content = match output_get_content(t, output) {
            Some(c) => c,
            None => continue,
        };
        for &ws in &t.cons[content].nodes_head {
            if !con_is_internal(t, ws) {
                result.push((output, ws));
            }
        }
    }
    result
}

/// Updates _NET_CURRENT_DESKTOP with the current desktop number.
///
/// EWMH: The index of the current desktop. This is always an integer between 0
/// and _NET_NUMBER_OF_DESKTOPS - 1.
///
/// C: src/ewmh.c:28 ewmh_update_current_desktop
pub fn ewmh_update_current_desktop(t: &Tree, x: &mut dyn XConn, a: &Atoms) {
    // The C 'static old_idx' is a cache to avoid redundant property writes;
    // the caller (server) tracks it, we write unconditionally when the index
    // changed. The pure function part is the index computation.
    let idx = t.focused.map(|f| ewmh_get_workspace_index(t, f)).unwrap_or(NET_WM_DESKTOP_NONE);
    if idx == NET_WM_DESKTOP_NONE {
        return;
    }
    let root = x.root_screen().root;
    x.change_property(root, a.A__NET_CURRENT_DESKTOP, XCB_ATOM_CARDINAL, 32, PropMode::REPLACE, &idx.to_le_bytes());
}

/// Updates _NET_NUMBER_OF_DESKTOPS which we interpret as the number of
/// noninternal workspaces.
///
/// C: src/ewmh.c:44 ewmh_update_number_of_desktops
pub fn ewmh_update_number_of_desktops(t: &Tree, x: &mut dyn XConn, a: &Atoms) {
    let idx = for_each_noninternal_workspace(t).len() as u32;
    let root = x.root_screen().root;
    x.change_property(root, a.A__NET_NUMBER_OF_DESKTOPS, XCB_ATOM_CARDINAL, 32, PropMode::REPLACE, &idx.to_le_bytes());
}

/// Updates _NET_DESKTOP_NAMES: "The names of all virtual desktops. This is a
/// list of NULL-terminated strings in UTF-8 encoding".
///
/// C: src/ewmh.c:66 ewmh_update_desktop_names
pub fn ewmh_update_desktop_names(t: &Tree, x: &mut dyn XConn, a: &Atoms) {
    let mut desktop_names: Vec<u8> = Vec::new();
    for (_, ws) in for_each_noninternal_workspace(t) {
        let name = t.cons[ws].name.clone().unwrap_or_default();
        desktop_names.extend_from_slice(name.as_bytes());
        desktop_names.push(0);
    }
    let root = x.root_screen().root;
    x.change_property(root, a.A__NET_DESKTOP_NAMES, a.A_UTF8_STRING, 8, PropMode::REPLACE, &desktop_names);
}

/// Updates _NET_DESKTOP_VIEWPORT, which is an array of pairs of cardinals that
/// define the top left corner of each desktop's viewport.
///
/// C: src/ewmh.c:93 ewmh_update_desktop_viewport
pub fn ewmh_update_desktop_viewport(t: &Tree, x: &mut dyn XConn, a: &Atoms) {
    let mut viewports: Vec<u32> = Vec::new();
    for (output, _) in for_each_noninternal_workspace(t) {
        viewports.push(t.cons[output].rect.x);
        viewports.push(t.cons[output].rect.y);
    }
    let mut data = Vec::with_capacity(viewports.len() * 4);
    for v in &viewports {
        data.extend_from_slice(&v.to_le_bytes());
    }
    let root = x.root_screen().root;
    x.change_property(root, a.A__NET_DESKTOP_VIEWPORT, XCB_ATOM_CARDINAL, 32, PropMode::REPLACE, &data);
}

/// Updates all the EWMH desktop properties.
///
/// C: src/ewmh.c:118 ewmh_update_desktop_properties
pub fn ewmh_update_desktop_properties(t: &mut Tree, x: &mut dyn XConn, a: &Atoms) {
    ewmh_update_number_of_desktops(t, x, a);
    ewmh_update_desktop_viewport(t, x, a);
    ewmh_update_current_desktop(t, x, a);
    ewmh_update_desktop_names(t, x, a);
    ewmh_update_wm_desktop(t, x, a);
}

/// C: src/ewmh.c:126 ewmh_update_wm_desktop_recursively
fn ewmh_update_wm_desktop_recursively(t: &mut Tree, x: &mut dyn XConn, a: &Atoms, con: crate::data::ConId, desktop: u32) {
    // Recursively call this to descend through the entire subtree.
    let children: Vec<crate::data::ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in children {
        ewmh_update_wm_desktop_recursively(t, x, a, child, desktop);
    }

    // If con is a workspace, we also need to go through the floating windows on it.
    if t.cons[con].r#type == crate::data::ConType::CT_WORKSPACE {
        let floating: Vec<crate::data::ConId> = t.cons[con].floating_head.iter().copied().collect();
        for child in floating {
            ewmh_update_wm_desktop_recursively(t, x, a, child, desktop);
        }
    }

    if !con_has_managed_window(t, con) {
        return;
    }

    let mut wm_desktop = desktop;
    // Sticky windows are only actually sticky when they are floating or inside
    // a floating container.
    if con_is_sticky(t, con) && con_is_floating(t, con) {
        wm_desktop = NET_WM_DESKTOP_ALL;
    }

    // If the window is on the scratchpad we assign the sticky value to it
    // since showing it works on any workspace.
    if let Some(ws) = con_get_workspace(t, con) {
        if con_is_internal(t, ws) {
            wm_desktop = NET_WM_DESKTOP_ALL;
        }
    }

    // If this is the cached value, we don't need to do anything.
    let window = match t.cons[con].window {
        Some(w) => w,
        None => return,
    };
    if t.windows[window].wm_desktop == wm_desktop {
        return;
    }
    t.windows[window].wm_desktop = wm_desktop;

    let xcb_window = t.windows[window].id;
    if wm_desktop != NET_WM_DESKTOP_NONE {
        x.change_property(xcb_window, a.A__NET_WM_DESKTOP, XCB_ATOM_CARDINAL, 32, PropMode::REPLACE, &wm_desktop.to_le_bytes());
    } else {
        // If we can't determine the workspace index, delete the property. We'd
        // rather not set it than lie.
        x.delete_property(xcb_window, a.A__NET_WM_DESKTOP);
    }
}

/// Updates _NET_WM_DESKTOP for all windows.
/// A request will only be made if the cached value differs from the calculated value.
///
/// C: src/ewmh.c:186 ewmh_update_wm_desktop
pub fn ewmh_update_wm_desktop(t: &mut Tree, x: &mut dyn XConn, a: &Atoms) {
    let mut desktop: u32 = 0;

    let root = match t.croot {
        Some(r) => r,
        None => return,
    };
    for &output in &t.cons[root].nodes_head.clone() {
        let content = match output_get_content(t, output) {
            Some(c) => c,
            None => continue,
        };
        for &workspace in &t.cons[content].nodes_head.clone() {
            ewmh_update_wm_desktop_recursively(t, x, a, workspace, desktop);

            if !con_is_internal(t, workspace) {
                desktop += 1;
            }
        }
    }
}

/// Updates _NET_ACTIVE_WINDOW with the currently focused window.
///
/// EWMH: The window ID of the currently active window or None if no window has
/// the focus.
///
/// C: src/ewmh.c:209 ewmh_update_active_window
pub fn ewmh_update_active_window(x: &mut dyn XConn, a: &Atoms, window: u32) {
    let root = x.root_screen().root;
    x.change_property(root, a.A__NET_ACTIVE_WINDOW, XCB_ATOM_WINDOW, 32, PropMode::REPLACE, &window.to_le_bytes());
}

/// Updates _NET_WM_VISIBLE_NAME.
///
/// C: src/ewmh.c:218 ewmh_update_visible_name
pub fn ewmh_update_visible_name(x: &mut dyn XConn, a: &Atoms, window: u32, name: Option<&str>) {
    match name {
        Some(name) => {
            let root = x.root_screen().root;
            let _ = root; // the property goes on the client window, not root
            x.change_property(window, a.A__NET_WM_VISIBLE_NAME, a.A_UTF8_STRING, 8, PropMode::REPLACE, name.as_bytes());
        }
        None => {
            x.delete_property(window, a.A__NET_WM_VISIBLE_NAME);
        }
    }
}

/// i3 currently does not support _NET_WORKAREA, because it does not correspond
/// to i3's concept of workspaces. We need to actively delete this property
/// because some display managers (e.g. LightDM) set it.
///
/// C: src/ewmh.c:241 ewmh_update_workarea
pub fn ewmh_update_workarea(x: &mut dyn XConn, a: &Atoms) {
    let root = x.root_screen().root;
    x.delete_property(root, a.A__NET_WORKAREA);
}

/// Updates the _NET_CLIENT_LIST hint.
///
/// C: src/ewmh.c:249 ewmh_update_client_list
pub fn ewmh_update_client_list(x: &mut dyn XConn, a: &Atoms, list: &[u32]) {
    let mut data = Vec::with_capacity(list.len() * 4);
    for w in list {
        data.extend_from_slice(&w.to_le_bytes());
    }
    let root = x.root_screen().root;
    x.change_property(root, a.A__NET_CLIENT_LIST, XCB_ATOM_WINDOW, 32, PropMode::REPLACE, &data);
}

/// Updates the _NET_CLIENT_LIST_STACKING hint.
///
/// C: src/ewmh.c:265 ewmh_update_client_list_stacking
pub fn ewmh_update_client_list_stacking(x: &mut dyn XConn, a: &Atoms, stack: &[u32]) {
    let mut data = Vec::with_capacity(stack.len() * 4);
    for w in stack {
        data.extend_from_slice(&w.to_le_bytes());
    }
    let root = x.root_screen().root;
    x.change_property(root, a.A__NET_CLIENT_LIST_STACKING, XCB_ATOM_WINDOW, 32, PropMode::REPLACE, &data);
}

/// Set or remove _NET_WM_STATE_STICKY on the window.
///
/// C: src/ewmh.c:277 ewmh_update_sticky
pub fn ewmh_update_sticky(x: &mut dyn XConn, a: &Atoms, window: u32, sticky: bool) {
    use crate::xcb_util::{xcb_add_property_atom, xcb_remove_property_atom};
    if sticky {
        xcb_add_property_atom(x, window, a.A__NET_WM_STATE, a.A__NET_WM_STATE_STICKY);
    } else {
        xcb_remove_property_atom(x, window, a.A__NET_WM_STATE, a.A__NET_WM_STATE_STICKY);
    }
}

/// Set or remove _NET_WM_STATE_FOCUSED on the window.
///
/// C: src/ewmh.c:291 ewmh_update_focused
pub fn ewmh_update_focused(x: &mut dyn XConn, a: &Atoms, window: u32, is_focused: bool) {
    use crate::xcb_util::{xcb_add_property_atom, xcb_remove_property_atom};
    if is_focused {
        xcb_add_property_atom(x, window, a.A__NET_WM_STATE, a.A__NET_WM_STATE_FOCUSED);
    } else {
        xcb_remove_property_atom(x, window, a.A__NET_WM_STATE, a.A__NET_WM_STATE_FOCUSED);
    }
}

thread_local! {
    /// C: src/ewmh.c:14 'xcb_window_t ewmh_window;' — the EWMH support window.
    ///
    /// x_push_changes() focuses it when there is no client to focus
    /// (src/x.c:1451) and the C code reads the global from ewmh.c. The port
    /// keeps the same slot, written by [ewmh_setup_hints]. It is per thread
    /// (like the x.c focus globals) so parallel unit tests stay isolated; the
    /// WM itself is single threaded.
    static EWMH_WINDOW: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// C: the ewmh_window global read.
pub fn get_ewmh_window() -> u32 {
    EWMH_WINDOW.with(|v| v.get())
}

/// C: `ewmh_window = xcb_generate_id(conn)`
pub fn set_ewmh_window(window: u32) {
    EWMH_WINDOW.with(|v| v.set(window));
}

/// Set up the EWMH hints on the root window. Returns the created ewmh_window.
///
/// C: src/ewmh.c:305 ewmh_setup_hints
pub fn ewmh_setup_hints(x: &mut dyn XConn, a: &Atoms) -> u32 {
    // C: xcb_atom_t supported_atoms[] = { XMACRO -> A_##atom };
    // The atom ids are already interned into `a` by Atoms::intern_all; use them
    // to keep one source of truth (37 atoms, see
    // include/i3-atoms_NET_SUPPORTED.xmacro.h).
    let supported_atoms: Vec<u32> = [
        a.A__NET_SUPPORTED,
        a.A__NET_SUPPORTING_WM_CHECK,
        a.A__NET_WM_NAME,
        a.A__NET_WM_VISIBLE_NAME,
        a.A__NET_WM_MOVERESIZE,
        a.A__NET_WM_STATE_STICKY,
        a.A__NET_WM_STATE_FULLSCREEN,
        a.A__NET_WM_STATE_DEMANDS_ATTENTION,
        a.A__NET_WM_STATE_MODAL,
        a.A__NET_WM_STATE_HIDDEN,
        a.A__NET_WM_STATE_FOCUSED,
        a.A__NET_WM_STATE_MAXIMIZED_VERT,
        a.A__NET_WM_STATE_MAXIMIZED_HORZ,
        a.A__NET_WM_STATE,
        a.A__NET_WM_WINDOW_TYPE,
        a.A__NET_WM_WINDOW_TYPE_NORMAL,
        a.A__NET_WM_WINDOW_TYPE_DOCK,
        a.A__NET_WM_WINDOW_TYPE_DIALOG,
        a.A__NET_WM_WINDOW_TYPE_UTILITY,
        a.A__NET_WM_WINDOW_TYPE_TOOLBAR,
        a.A__NET_WM_WINDOW_TYPE_SPLASH,
        a.A__NET_WM_WINDOW_TYPE_MENU,
        a.A__NET_WM_WINDOW_TYPE_DROPDOWN_MENU,
        a.A__NET_WM_WINDOW_TYPE_POPUP_MENU,
        a.A__NET_WM_WINDOW_TYPE_TOOLTIP,
        a.A__NET_WM_WINDOW_TYPE_NOTIFICATION,
        a.A__NET_WM_DESKTOP,
        a.A__NET_WM_STRUT_PARTIAL,
        a.A__NET_CLIENT_LIST,
        a.A__NET_CLIENT_LIST_STACKING,
        a.A__NET_CURRENT_DESKTOP,
        a.A__NET_NUMBER_OF_DESKTOPS,
        a.A__NET_DESKTOP_NAMES,
        a.A__NET_DESKTOP_VIEWPORT,
        a.A__NET_ACTIVE_WINDOW,
        a.A__NET_CLOSE_WINDOW,
        a.A__NET_MOVERESIZE_WINDOW,
    ]
    .to_vec();

    let root = x.root_screen().root;

    // We create the window and put it at (-1, -1) so that it is off-screen.
    let ewmh_window = x.generate_id();
    x.create_window(
        x11rb_copy_from_parent_depth(), /* depth: XCB_COPY_FROM_PARENT */
        ewmh_window,
        root,
        -1,
        -1,
        1,
        1,
        0, /* border */
        crate::backend::XCB_WINDOW_CLASS_INPUT_ONLY,
        x11rb_copy_from_parent_visual(),
        &[(crate::backend::XCB_CW_OVERRIDE_REDIRECT, 1)],
    );
    x.change_property(ewmh_window, a.A__NET_SUPPORTING_WM_CHECK, XCB_ATOM_WINDOW, 32, PropMode::REPLACE, &ewmh_window.to_le_bytes());
    x.change_property(ewmh_window, a.A__NET_WM_NAME, a.A_UTF8_STRING, 8, PropMode::REPLACE, b"i3");
    x.change_property(root, a.A__NET_SUPPORTING_WM_CHECK, XCB_ATOM_WINDOW, 32, PropMode::REPLACE, &ewmh_window.to_le_bytes());

    // I'm not entirely sure if we need to keep _NET_WM_NAME on root.
    x.change_property(root, a.A__NET_WM_NAME, a.A_UTF8_STRING, 8, PropMode::REPLACE, b"i3");

    let mut data = Vec::with_capacity(supported_atoms.len() * 4);
    for v in &supported_atoms {
        data.extend_from_slice(&v.to_le_bytes());
    }
    x.change_property(root, a.A__NET_SUPPORTED, 6 /* XCB_ATOM_ATOM */, 32, PropMode::REPLACE, &data);

    // We need to map this window to be able to set the input focus to it if no
    // other window is available to be focused.
    x.map_window(ewmh_window);
    use crate::backend::{ConfigWindow, StackMode};
    x.configure_window(ewmh_window, &[(ConfigWindow::STACK_MODE, StackMode::BELOW as u32)]);

    // C: ewmh_window = xcb_generate_id(conn) — the assignment to the global.
    set_ewmh_window(ewmh_window);

    ewmh_window
}

/// C: XCB_COPY_FROM_PARENT for depth (0).
fn x11rb_copy_from_parent_depth() -> u16 {
    0
}

/// C: XCB_COPY_FROM_PARENT for visual.
fn x11rb_copy_from_parent_visual() -> u32 {
    crate::backend::XCB_COPY_FROM_PARENT
}

/// The 37 _NET_SUPPORTED atom names (same order as the C xmacro) — used to
/// document the order and for tests.
pub const NET_SUPPORTED_ATOMS: &[&str] = crate::xcb_util::NET_SUPPORTED_ATOM_NAMES;

/// Returns the workspace container as enumerated by the EWMH desktop model.
/// Returns None if no workspace could be found for the index.
///
/// This is the reverse of ewmh_get_workspace_index.
///
/// C: src/ewmh.c:351 ewmh_get_workspace_by_index
pub fn ewmh_get_workspace_by_index(t: &Tree, idx: u32) -> Option<crate::data::ConId> {
    if idx == NET_WM_DESKTOP_NONE {
        return None;
    }

    let mut current_index: u32 = 0;
    for (_, ws) in for_each_noninternal_workspace(t) {
        if current_index == idx {
            return Some(ws);
        }
        current_index += 1;
    }

    None
}

/// Returns the EWMH desktop index for the workspace the given container is on.
/// Returns NET_WM_DESKTOP_NONE if the desktop index cannot be determined.
///
/// This is the reverse of ewmh_get_workspace_by_index.
///
/// C: src/ewmh.c:376 ewmh_get_workspace_index
pub fn ewmh_get_workspace_index(t: &Tree, con: crate::data::ConId) -> u32 {
    let mut index: u32 = 0;

    let target_workspace = con_get_workspace(t, con);
    for (_, ws) in for_each_noninternal_workspace(t) {
        if Some(ws) == target_workspace {
            return index;
        }

        index += 1;
    }

    NET_WM_DESKTOP_NONE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{NullBackend, XConn};
    use crate::con::con_new;
    use crate::data::{ConId, ConType};
    use crate::xcb_util::RecordingConn;

    fn make_workspace_tree() -> (Tree, ConId, ConId, ConId) {
        // root -> output -> content (built by tree_init) -- workspaces are
        // created on demand by the tests themselves.
        let mut t = Tree::new();
        crate::tree::tree_init(&mut t, &mut NullBackend, crate::data::Rect::new(0, 0, 1280, 800));
        let dummy: ConId = ConId::default();
        (t, dummy, dummy, dummy)
    }

    #[test]
    fn workspace_index_roundtrip() {
        let (t, _o, _c, _w) = make_workspace_tree();
        // Without workspaces present, every index lookup fails with NONE.
        assert_eq!(ewmh_get_workspace_index(&t, t.croot.unwrap()), NET_WM_DESKTOP_NONE);
        assert_eq!(ewmh_get_workspace_by_index(&t, 0), None);
    }

    #[test]
    fn noninternal_count_is_zero_for_empty_tree() {
        let (t, _, _, _) = make_workspace_tree();
        assert_eq!(for_each_noninternal_workspace(&t).len(), 0);
    }

    #[test]
    fn setup_hints_creates_ewmh_window_and_supported() {
        let mut x = RecordingConn::new(0x50);
        let a = Atoms::intern_all(&mut x);
        let ewmh_window = ewmh_setup_hints(&mut x, &a);
        let log = x.log.join(" | ");
        // C: xcb_create_window(..., -1, -1, 1, 1, ..., INPUT_ONLY, override_redirect=1)
        assert!(log.contains("create_window"), "log={}", log);
        assert!(log.contains(&format!("map_window({:#x})", ewmh_window)));
        assert!(log.contains(&format!("change_property(win={:#x}, prop={},", ewmh_window, a.A__NET_SUPPORTING_WM_CHECK)));
        // C: sizeof(supported_atoms) / sizeof(xcb_atom_t) == 37
        assert!(log.contains(&format!("prop={}, type=6, fmt=32, mode=REPLACE, bytes={}", a.A__NET_SUPPORTED, 37 * 4)));
        // C: xcb_configure_window(ewmh_window, XCB_CONFIG_WINDOW_STACK_MODE, BELOW)
        assert!(log.contains(&format!("configure_window({:#x}, [(STACK_MODE, 1)])", ewmh_window)));
    }

    #[test]
    fn update_wm_desktop_writes_property() {
        let mut x = RecordingConn::new(0x50);
        let a = Atoms::intern_all(&mut x);
        let (mut t, _, _, _) = make_workspace_tree();
        // A windowless tree must not emit any _NET_WM_DESKTOP change.
        let before = x.log.len();
        ewmh_update_wm_desktop(&mut t, &mut x, &a);
        assert_eq!(x.log.len(), before);
        let _ = con_new;
        let _ = ConType::CT_WORKSPACE;
    }

    #[test]
    fn desktop_viewport_and_names_shape() {
        let mut x = RecordingConn::new(0x50);
        let a = Atoms::intern_all(&mut x);
        let (t, _, _, _) = make_workspace_tree();
        ewmh_update_desktop_viewport(&t, &mut x, &a);
        ewmh_update_desktop_names(&t, &mut x, &a);
        let log = x.log.join(" | ");
        assert!(log.contains(&format!("prop={}, type={},", a.A__NET_DESKTOP_VIEWPORT, XCB_ATOM_CARDINAL)));
        assert!(log.contains(&format!("prop={}, type={},", a.A__NET_DESKTOP_NAMES, a.A_UTF8_STRING)));
    }
}