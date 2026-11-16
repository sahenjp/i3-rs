//! 1:1 port of src/con.c — functions which deal with containers directly
//! (creating containers, searching containers, getting specific properties
//! from containers, …).
//!
//! C: src/con.c

use crate::backend::ConBackend;
use crate::data::{
    ConType, Direction, FullscreenMode, Layout, Orientation, Rect, FloatingState, BorderStyle,
    Con, KillWindow, MarkMode, WindowId,
};
use crate::floating::{floating_fix_coordinates, floating_raise_con};
use crate::match_::match_matches_window;
use crate::tree::{tree_close_internal, tree_flatten, Tree};
use crate::util::{rect_add, rect_sub, strcasecmp_nullable};
use crate::workspace::{
    workspace_attach_to, workspace_encapsulate, workspace_get, workspace_is_visible,
    workspace_show, workspace_update_urgent_flag,
};
use crate::output::output_get_content;

/// C: src/con.c:28 `con_force_split_parents_redraw`
///
/// force parent split containers to be redrawn
pub fn con_force_split_parents_redraw(t: &mut Tree, con: ConId) {
    let mut parent = Some(con);

    while let Some(p) = parent {
        if t.cons[p].r#type == ConType::CT_WORKSPACE || t.cons[p].r#type == ConType::CT_DOCKAREA {
            break;
        }
        if !con_is_leaf(t, p) {
            t.cons[p].deco_render_params = None;
        }

        parent = t.cons[p].parent;
    }
}

/// C: src/con.c:38 `con_new_skeleton`
///
/// Create a new container (and attach it to the given parent, if not None).
/// This function only initializes the data structures.
pub fn con_new_skeleton(t: &mut Tree, b: &mut dyn ConBackend, parent: Option<ConId>, window: Option<WindowId>) -> ConId {
    let mut new = Con::default();
    new.r#type = ConType::CT_CON;
    new.window = window;
    new.border_style = t.config.default_border;
    new.max_user_border_style = t.config.default_border;
    new.current_border_width = -1;
    new.window_icon_padding = -1;
    new.depth = match window {
        Some(w) => t.windows[w].depth,
        None => t.root_depth,
    };

    let new = t.cons.insert(new);
    t.all_cons.push_back(new);

    if let Some(parent) = parent {
        con_attach(t, b, new, parent, false);
    }

    new
}

/// C: src/con.c:72 `con_new`
///
/// A wrapper for con_new_skeleton, to retain the old con_new behaviour.
pub fn con_new(t: &mut Tree, b: &mut dyn ConBackend, parent: Option<ConId>, window: Option<WindowId>) -> ConId {
    let new = con_new_skeleton(t, b, parent, window);
    b.x_con_init(t, new);
    new
}

/// C: src/con.c:80 `con_free`
///
/// Frees the specified container.
pub fn con_free(t: &mut Tree, con: ConId) {
    if let Some(idx) = t.all_cons.iter().position(|&c| c == con) {
        t.all_cons.remove(idx);
    }
    t.cons[con].swallow_head.clear();
    t.cons[con].marks_head.clear();
    t.cons.remove(con);
}

/// C: src/con.c:124 `_con_attach`
fn _con_attach(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, parent: ConId, previous: Option<ConId>, ignore_focus: bool) {
    t.cons[con].parent = Some(parent);
    let mut current = previous;
    /* The owners of the nodes_head/focus_head (retargeted when the parent is
     * a workspace with a non-default workspace_layout). */
    let mut nodes_owner = parent;
    let mut focus_owner = parent;

    /* Workspaces are handled differently: they need to be inserted at the
     * right position. */
    let mut direct_add_to_focus_head = false;
    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        let num = t.cons[con].num;
        if num == -1 || t.cons[nodes_owner].nodes_head.is_empty() {
            t.nodes_insert_tail(nodes_owner, con);
        } else {
            let first = t.cons[nodes_owner].nodes_head[0];
            if num < t.cons[first].num {
                /* we need to insert the container at the beginning */
                t.nodes_insert_head(nodes_owner, con);
            } else {
                let mut cur: Option<ConId> = Some(first);
                loop {
                    let c = match cur {
                        Some(c) => c,
                        None => break,
                    };
                    if t.cons[c].num != -1 && num >= t.cons[c].num {
                        cur = t.nodes_next(c);
                    } else {
                        break;
                    }
                }
                /* we need to insert con before current, if current is not None */
                match cur {
                    Some(c) => t.nodes_insert_before(c, con),
                    None => t.nodes_insert_tail(nodes_owner, con),
                }
            }
        }
        direct_add_to_focus_head = true;
    } else if t.cons[parent].r#type == ConType::CT_DOCKAREA {
        /* Insert dock client, sorting alphanumerically by class and then
         * instance name. This makes dock client order deterministic. As a side
         * effect, bars without a custom bar id will be sorted according to
         * their declaration order in the config file. See #3491. */
        let win = t.cons[con].window.expect("_con_attach: dockarea con without window");
        let con_cc = t.windows[win].class_class.clone();
        let con_ci = t.windows[win].class_instance.clone();

        let mut current: Option<ConId> = None;
        let mut insert_before: Option<ConId> = None;
        for &loop_ in &t.cons[nodes_owner].nodes_head {
            let lwin = t.cons[loop_].window.expect("_con_attach: dockarea child without window");
            let mut result = strcasecmp_nullable(con_cc.as_deref(), t.windows[lwin].class_class.as_deref());
            if result == std::cmp::Ordering::Equal {
                result = strcasecmp_nullable(con_ci.as_deref(), t.windows[lwin].class_instance.as_deref());
            }
            if result == std::cmp::Ordering::Less {
                current = Some(loop_);
                insert_before = Some(loop_);
                break;
            }
        }
        if current.is_some() {
            /* NB: C uses TAILQ_INSERT_BEFORE(loop, …) here (identical to
             * current because the loop broke immediately). */
            t.nodes_insert_before(insert_before.expect("dockarea insert_before"), con);
        } else {
            t.nodes_insert_tail(nodes_owner, con);
        }
        direct_add_to_focus_head = true;
    }

    if !direct_add_to_focus_head {
        if t.cons[con].r#type == ConType::CT_FLOATING_CON {
            t.floating_insert_tail(parent, con);
        } else {
            if !ignore_focus {
                /* Get the first tiling container in focus stack */
                for &loop_ in &t.cons[parent].focus_head {
                    if t.cons[loop_].r#type == ConType::CT_FLOATING_CON {
                        continue;
                    }
                    current = Some(loop_);
                    break;
                }
            }

            /* When the container is not a split container (but contains a window)
             * and is attached to a workspace, we check if the user configured a
             * workspace_layout. This is done in workspace_attach_to, which will
             * provide us with the container to which we should attach (either the
             * workspace or a new split container with the configured
             * workspace_layout). */
            if t.cons[con].window.is_some()
                && t.cons[parent].r#type == ConType::CT_WORKSPACE
                && t.cons[parent].workspace_layout != Layout::L_DEFAULT
            {
                let target = workspace_attach_to(t, b, parent);

                /* Attach the original con to this new split con instead */
                nodes_owner = target;
                focus_owner = target;
                t.cons[con].parent = Some(target);
                current = None;
            }

            /* Insert the container after the tiling container, if found.
             * When adding to a CT_OUTPUT, just append one after another. */
            if current.is_some() && t.cons[parent].r#type != ConType::CT_OUTPUT {
                t.nodes_insert_after(current.expect("_con_attach: current"), con);
            } else {
                t.nodes_insert_tail(nodes_owner, con);
            }
        }
    }

    /* add_to_focus_head: We insert to the TAIL because con_focus() will
     * correct this. This way, we have the option to insert Cons without
     * having to focus them. */
    t.focus_insert_tail(focus_owner, con);
    con_force_split_parents_redraw(t, con);
}

/// C: src/con.c:230 `con_attach`
///
/// Attaches the given container to the given parent. This happens when moving
/// a container or when inserting a new container at a specific place in the
/// tree.
///
/// ignore_focus is to just insert the Con at the end (useful when creating a
/// new split container *around* some containers, that is, detaching and
/// attaching them in order without wanting to mess with the focus in between).
pub fn con_attach(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, parent: ConId, ignore_focus: bool) {
    _con_attach(t, b, con, parent, None, ignore_focus);
}

/// C: src/con.c:247 `con_detach`
///
/// Detaches the given container from its current parent.
pub fn con_detach(t: &mut Tree, con: ConId) {
    con_force_split_parents_redraw(t, con);
    let parent = t.cons[con].parent.expect("con_detach: con has no parent");
    if t.cons[con].r#type == ConType::CT_FLOATING_CON {
        t.floating_remove(parent, con);
        t.focus_remove(parent, con);
    } else {
        t.nodes_remove(parent, con);
        t.focus_remove(parent, con);
    }
}

/// C: src/con.c:262 `con_focus`
///
/// Sets input focus to the given container. Will be updated in X11 in the
/// next run of x_push_changes().
pub fn con_focus(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    // C: src/con.c:252 DLOG("con_focus = %p\n", con);
    crate::log::dlog_at(
        file!(),
        "con_focus",
        line!(),
        &format!("con_focus = {}\n", crate::manage::con_ptr(con)),
    );

    let parent = t.cons[con].parent.expect("con_focus: con has no parent");

    /* 1: set focused-pointer to the new con */
    /* 2: exchange the position of the container in focus stack of the parent all the way up */
    t.focus_remove(parent, con);
    t.focus_insert_head(parent, con);
    if t.cons[parent].parent.is_some() {
        con_focus(t, b, parent);
    }

    t.focused = Some(con);
    /* We can't blindly reset non-leaf containers since they might have
     * other urgent children. Therefore we only reset leafs and propagate
     * the changes upwards via con_update_parents_urgency() which does proper
     * checks before resetting the urgency. */
    if t.cons[con].urgent && con_is_leaf(t, con) {
        con_set_urgency(t, b, con, false);
        con_update_parents_urgency(t, con);
        if let Some(ws) = con_get_workspace(t, con) {
            workspace_update_urgent_flag(t, b, ws);
        }
        b.ipc_send_window_event(t, "urgent", con);
    }
}

/// C: src/con.c:291 `con_raise` (static)
///
/// Raise container to the top if it is floating or inside some floating
/// container.
fn con_raise(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    if let Some(floating) = con_inside_floating(t, con) {
        floating_raise_con(t, b, floating);
    }
}

/// C: src/con.c:301 `con_activate`
///
/// Sets input focus to the given container and raises it to the top.
pub fn con_activate(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    con_focus(t, b, con);
    con_raise(t, b, con);
}

/// C: src/con.c:309 `con_activate_unblock`
///
/// Activates the container like in con_activate but removes fullscreen
/// restrictions and properly warps the pointer if needed.
pub fn con_activate_unblock(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    let ws = con_get_workspace(t, con);
    let previous_focus = t.focused;
    let fullscreen_on_ws = match ws {
        Some(ws) => crate::con::con_get_fullscreen_covering_ws(t, Some(ws)),
        None => None,
    };

    if let Some(fson) = fullscreen_on_ws {
        if fson != con && !con_has_parent(t, Some(con), fson) {
            con_disable_fullscreen(t, b, fson);
        }
    }

    con_activate(t, b, con);

    /* If the container is not on the current workspace, workspace_show() will
     * switch to a different workspace and (if enabled) trigger a mouse pointer
     * warp to the currently focused container (!) on the target workspace.
     *
     * Therefore, before calling workspace_show(), we make sure that 'con' will
     * be focused on the workspace. However, we cannot just con_focus(con)
     * because then the pointer will not be warped at all (the code thinks we
     * are already there).
     *
     * So we focus 'con' to make it the currently focused window of the target
     * workspace, then revert focus. */
    if let Some(previous_focus) = previous_focus {
        if ws != con_get_workspace(t, previous_focus) {
            con_activate(t, b, previous_focus);
            /* Now switch to the workspace, then focus */
            workspace_show(t, b, ws.expect("con_activate_unblock: no workspace"));
            con_activate(t, b, con);
        }
    }
}

/// C: src/con.c:353 `con_close`
///
/// Closes the given container.
pub fn con_close(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, kill_window: KillWindow) {
    /* We never close output or root containers. */
    if t.cons[con].r#type == ConType::CT_OUTPUT || t.cons[con].r#type == ConType::CT_ROOT {
        return;
    }

    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        let children: Vec<ConId> = t.cons[con].focus_head.iter().copied().collect();
        for child in children {
            tree_close_internal(t, b, child, kill_window, false);
        }
        return;
    }

    tree_close_internal(t, b, con, kill_window, false);
}

/// C: src/con.c:383 `con_is_leaf`
///
/// Returns true when this node is a leaf node (has no children).
pub fn con_is_leaf(t: &Tree, con: ConId) -> bool {
    t.cons[con].nodes_head.is_empty()
}

/// C: src/con.c:388 `con_has_managed_window`
///
/// Returns true when this con is a leaf node with a managed X11 window (e.g.,
/// excluding dock containers).
pub fn con_has_managed_window(t: &Tree, con: ConId) -> bool {
    t.cons[con].window.is_some()
        && t.cons[con].window.map(|w| t.windows[w].id != crate::data::XCB_WINDOW_NONE).unwrap_or(false)
        && con_get_workspace(t, con).is_some()
}

/// C: src/con.c:396 `con_has_children`
///
/// Returns true if this node has regular or floating children.
pub fn con_has_children(t: &Tree, con: ConId) -> bool {
    !con_is_leaf(t, con) || !t.cons[con].floating_head.is_empty()
}

/// C: src/con.c:404 `con_is_split`
///
/// Returns true if a container should be considered split.
pub fn con_is_split(t: &Tree, con: ConId) -> bool {
    if con_is_leaf(t, con) {
        return false;
    }

    match t.cons[con].layout {
        Layout::L_DOCKAREA | Layout::L_OUTPUT => false,
        _ => true,
    }
}

/// C: src/con.c:423 `con_is_hidden`
///
/// This will only return true for containers which have some parent with
/// a tabbed / stacked parent of which they are not the currently focused child.
pub fn con_is_hidden(t: &Tree, con: ConId) -> bool {
    let mut current = Some(con);

    /* ascend to the workspace level and memorize the highest-up container
     * which is stacked or tabbed. */
    while let Some(cur) = current {
        if t.cons[cur].r#type == ConType::CT_WORKSPACE {
            break;
        }
        let parent = t.cons[cur].parent;
        if let Some(parent) = parent {
            if t.cons[parent].layout == Layout::L_TABBED || t.cons[parent].layout == Layout::L_STACKED {
                if t.cons[parent].focus_head.front() != Some(&cur) {
                    return true;
                }
            }
        }

        current = parent;
    }

    false
}

/// C: src/con.c:447 `con_is_maximized`
///
/// Returns true if the container is maximized in the given orientation.
pub fn con_is_maximized(t: &Tree, con: ConId, orientation: Orientation) -> bool {
    /* Fullscreen containers are considered maximized. */
    if t.cons[con].fullscreen_mode != FullscreenMode::CF_NONE {
        return true;
    }

    /* Look up the container layout which corresponds to the given
     * orientation. */
    let layout = match orientation {
        Orientation::HORIZ => Layout::L_SPLITH,
        Orientation::VERT => Layout::L_SPLITV,
        Orientation::NO_ORIENTATION => panic!("con_is_maximized: invalid orientation"),
    };

    /* Go through all parents, stopping once we reach the workspace node. */
    let mut current = con;
    loop {
        let parent = t.cons[current].parent;
        if parent.is_none() || t.cons[current].r#type == ConType::CT_WORKSPACE {
            /* We are done searching. We found no reason that the container
             * should not be considered maximized. */
            return true;
        }
        let parent = parent.expect("con_is_maximized: parent");

        if t.cons[parent].layout == layout && con_num_children(t, parent) > 1 {
            /* The parent has a split in the indicated direction, which
             * means none of its children are maximized in that direction. */
            return false;
        }

        /* Floating containers and their children are not considered
         * maximized. */
        if t.cons[parent].r#type == ConType::CT_FLOATING_CON {
            return false;
        }

        current = parent;
    }
}

/// C: src/con.c:505 `con_is_sticky`
///
/// Returns whether the container or any of its children is sticky.
pub fn con_is_sticky(t: &Tree, con: ConId) -> bool {
    if t.cons[con].sticky {
        return true;
    }

    for &child in &t.cons[con].nodes_head {
        if con_is_sticky(t, child) {
            return true;
        }
    }

    false
}

/// C: src/con.c:526 `con_accepts_window`
///
/// Returns true if this node accepts a window (if the node swallows windows,
/// it might already have swallowed enough and cannot hold any more).
pub fn con_accepts_window(t: &Tree, con: ConId) -> bool {
    /* 1: workspaces never accept direct windows */
    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        return false;
    }

    if con_is_split(t, con) {
        return false;
    }

    /* TODO: if this is a swallowing container, we need to check its max_clients */
    t.cons[con].window.is_none()
}

/// C: src/con.c:547 `con_get_output`
///
/// Gets the output container (first container with CT_OUTPUT in hierarchy)
/// this node is on.
pub fn con_get_output(t: &Tree, con: ConId) -> Option<ConId> {
    let mut result = Some(con);
    while let Some(r) = result {
        if t.cons[r].r#type == ConType::CT_OUTPUT {
            return Some(r);
        }
        result = t.cons[r].parent;
    }
    /* We must be able to get an output because focus can never be set higher
     * in the tree (root node cannot be focused). */
    None
}

/// C: src/con.c:561 `con_get_workspace`
///
/// Gets the workspace container this node is on.
pub fn con_get_workspace(t: &Tree, con: ConId) -> Option<ConId> {
    let mut result = Some(con);
    while let Some(r) = result {
        if t.cons[r].r#type == ConType::CT_WORKSPACE {
            return Some(r);
        }
        result = t.cons[r].parent;
    }
    None
}

/// C: src/con.c:575 `con_parent_with_orientation`
///
/// Searches parents of the given 'con' until it reaches one with the
/// specified 'orientation'. Aborts when it comes across a floating_con.
pub fn con_parent_with_orientation(t: &Tree, con: ConId, orientation: Orientation) -> Option<ConId> {
    let mut parent = t.cons[con].parent;
    if t.cons[parent?].r#type == ConType::CT_FLOATING_CON {
        return None;
    }
    loop {
        let p = parent?;
        if con_orientation(t, p) == orientation {
            return parent;
        }
        /* Need to go one level further up */
        parent = t.cons[p].parent;
        /* Abort when we reach a floating con, or an output con */
        if let Some(pp) = parent {
            let pp_parent = t.cons[pp].parent;
            if t.cons[pp].r#type == ConType::CT_FLOATING_CON
                || t.cons[pp].r#type == ConType::CT_OUTPUT
                || pp_parent.map(|ppp| t.cons[ppp].r#type == ConType::CT_OUTPUT).unwrap_or(false)
            {
                parent = None;
            }
        }
        if parent.is_none() {
            break;
        }
    }
    parent
}

/// C: src/con.c:606 `con_get_fullscreen_con`
///
/// Returns the first fullscreen node below this node (breadth-first).
pub fn con_get_fullscreen_con(t: &Tree, con: ConId, fullscreen_mode: FullscreenMode) -> Option<ConId> {
    let mut bfs_head: std::collections::VecDeque<ConId> = std::collections::VecDeque::new();
    bfs_head.push_back(con);

    while let Some(current) = bfs_head.pop_front() {
        if current != con && t.cons[current].fullscreen_mode == fullscreen_mode {
            return Some(current);
        }

        for &child in &t.cons[current].nodes_head {
            bfs_head.push_back(child);
        }

        for &child in &t.cons[current].floating_head {
            bfs_head.push_back(child);
        }
    }

    None
}

/// C: src/con.c:655 `con_get_fullscreen_covering_ws`
///
/// Returns the fullscreen node that covers the given workspace if it exists.
/// This is either a CF_GLOBAL fullscreen container anywhere or a CF_OUTPUT
/// fullscreen container in the workspace.
pub fn con_get_fullscreen_covering_ws(t: &Tree, ws: Option<ConId>) -> Option<ConId> {
    let ws = ws?;
    let croot = t.croot?;
    let fs = con_get_fullscreen_con(t, croot, FullscreenMode::CF_GLOBAL);
    if fs.is_none() {
        return con_get_fullscreen_con(t, ws, FullscreenMode::CF_OUTPUT);
    }
    fs
}

/// C: src/con.c:670 `con_is_internal`
///
/// Returns true if the container is internal, such as __i3_scratch.
pub fn con_is_internal(t: &Tree, con: ConId) -> bool {
    t.cons[con]
        .name
        .as_ref()
        .map(|n| n.starts_with("__"))
        .unwrap_or(false)
}

/// C: src/con.c:678 `con_is_floating`
///
/// Returns true if the node is floating.
pub fn con_is_floating(t: &Tree, con: ConId) -> bool {
    t.cons[con].floating >= FloatingState::FLOATING_AUTO_ON
}

/// C: src/con.c:687 `con_is_docked`
///
/// Returns true if the container is a docked container.
pub fn con_is_docked(t: &Tree, con: ConId) -> bool {
    let parent = match t.cons[con].parent {
        None => return false,
        Some(p) => p,
    };

    if t.cons[parent].r#type == ConType::CT_DOCKAREA {
        return true;
    }

    con_is_docked(t, parent)
}

/// C: src/con.c:701 `con_inside_floating`
///
/// Checks if the given container is either floating or inside some floating
/// container. It returns the FLOATING_CON container.
pub fn con_inside_floating(t: &Tree, con: ConId) -> Option<ConId> {
    if t.cons[con].r#type == ConType::CT_FLOATING_CON {
        return Some(con);
    }

    if t.cons[con].floating >= FloatingState::FLOATING_AUTO_ON {
        return t.cons[con].parent;
    }

    if t.cons[con].r#type == ConType::CT_WORKSPACE || t.cons[con].r#type == ConType::CT_OUTPUT {
        return None;
    }

    con_inside_floating(t, t.cons[con].parent?)
}

/// C: src/con.c:724 `con_inside_focused`
///
/// Checks if the given container is inside a focused container.
pub fn con_inside_focused(t: &Tree, con: ConId) -> bool {
    if t.focused == Some(con) {
        return true;
    }
    match t.cons[con].parent {
        None => false,
        Some(parent) => con_inside_focused(t, parent),
    }
}

/// C: src/con.c:738 `con_has_parent`
///
/// Checks if the container has the given parent as an actual parent.
pub fn con_has_parent(t: &Tree, con: Option<ConId>, parent: ConId) -> bool {
    let current = match con {
        None => return false,
        Some(con) => t.cons[con].parent,
    };
    match current {
        None => return false,
        Some(c) => {
            if c == parent {
                return true;
            }
        }
    }

    con_has_parent(t, current, parent)
}

/// C: src/con.c:752 `con_by_window_id`
///
/// Returns the container with the given client window ID or None if no such
/// container exists. (Implemented via the reverse window index, then a scan
/// for the con holding that WindowId.)
pub fn con_by_window_id(t: &Tree, window: u32) -> Option<ConId> {
    let window_id = t.window_index.get(&window).copied()?;
    for &con in &t.all_cons {
        if t.cons[con].window == Some(window_id) {
            return Some(con);
        }
    }
    None
}

/// C: src/con.c:765 `con_by_con_id`
///
/// Returns the container with the given container ID or None if no such
/// container exists.
pub fn con_by_con_id(t: &Tree, target: ConId) -> Option<ConId> {
    for &con in &t.all_cons {
        if con == target {
            return Some(con);
        }
    }
    None
}

/// C: src/con.c:783 `con_exists`
///
/// Returns true if the given container (still) exists.
/// This can be used, e.g., to make sure a container hasn't been closed in the meantime.
pub fn con_exists(t: &Tree, con: ConId) -> bool {
    con_by_con_id(t, con).is_some()
}

/// C: src/con.c:793 `con_by_frame_id`
///
/// Returns the container with the given frame ID or None if no such container
/// exists.
pub fn con_by_frame_id(t: &Tree, frame: u32) -> Option<ConId> {
    for &con in &t.all_cons {
        if t.cons[con].frame.map(|f| f.id == frame).unwrap_or(false) {
            return Some(con);
        }
    }
    None
}

/// C: src/con.c:808 `con_by_mark`
///
/// Returns the container with the given mark or None if no such container
/// exists.
pub fn con_by_mark(t: &Tree, mark: &str) -> Option<ConId> {
    for &con in &t.all_cons {
        if con_has_mark(t, con, mark) {
            return Some(con);
        }
    }
    None
}

/// C: src/con.c:822 `con_find_transient_for_window`
///
/// Start from a container and traverse the transient_for linked list. Returns
/// true if target window is found in the list. Protects againsts potential
/// cycles.
pub fn con_find_transient_for_window(t: &Tree, start: ConId, target: u32) -> bool {
    let mut transient_con = Some(start);
    let mut count =
        con_num_windows(t, Some(t.croot.expect("con_find_transient_for_window: no croot"))) as i32;
    while let Some(tc) = transient_con {
        let window = match t.cons[tc].window {
            Some(w) => w,
            None => break,
        };
        if t.windows[window].transient_for == crate::data::XCB_WINDOW_NONE {
            break;
        }
        if t.windows[window].transient_for == target {
            return true;
        }
        let next_transient = con_by_window_id(t, t.windows[window].transient_for);
        if next_transient.is_none() {
            break;
        }
        /* Some clients (e.g. x11-ssh-askpass) actually set WM_TRANSIENT_FOR to
         * their own window id, so break instead of looping endlessly. */
        if Some(tc) == next_transient {
            break;
        }
        transient_con = next_transient;

        if count <= 0 {
            /* Avoid cycles, see #4404 */
            break;
        }
        count -= 1;
    }
    false
}

/// C: src/con.c:856 `con_has_mark`
///
/// Returns true if and only if the given containers holds the mark.
pub fn con_has_mark(t: &Tree, con: ConId, mark: &str) -> bool {
    for current in &t.cons[con].marks_head {
        if current.name == mark {
            return true;
        }
    }
    false
}

/// C: src/con.c:871 `con_mark_toggle`
///
/// Toggles the mark on a container.
/// If the container already has this mark, the mark is removed.
/// Otherwise, the mark is assigned to the container.
pub fn con_mark_toggle(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, mark: &str, mode: MarkMode) {
    if con_has_mark(t, con, mark) {
        con_unmark(t, b, Some(con), Some(mark));
    } else {
        con_mark(t, b, con, mark, mode);
    }
}

/// C: src/con.c:885 `con_mark`
///
/// Assigns a mark to the container.
pub fn con_mark(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, mark: &str, mode: MarkMode) {
    con_unmark(t, b, None, Some(mark));
    if mode == MarkMode::MM_REPLACE {
        while let Some(first) = t.cons[con].marks_head.front().cloned() {
            let name = first.name.clone();
            con_unmark(t, b, Some(con), Some(&name));
        }
    }

    t.cons[con].marks_head.push_back(crate::data::mark_t { name: mark.to_string() });
    b.ipc_send_window_event(t, "mark", con);

    t.cons[con].mark_changed = true;
}

/// C: src/con.c:911 `con_unmark`
///
/// Removes marks from containers.
/// If con is None, all containers are considered.
/// If name is None, this removes all existing marks.
/// Otherwise, it will only remove the given mark (if it is present).
pub fn con_unmark(t: &mut Tree, b: &mut dyn ConBackend, con: Option<ConId>, name: Option<&str>) {
    match name {
        None => {
            /* Unmarking all containers. */
            let all_cons: Vec<ConId> = t.all_cons.iter().copied().collect();
            for current in all_cons {
                if con.is_some() && Some(current) != con {
                    continue;
                }

                if t.cons[current].marks_head.is_empty() {
                    continue;
                }

                while !t.cons[current].marks_head.is_empty() {
                    t.cons[current].marks_head.pop_front();

                    b.ipc_send_window_event(t, "mark", current);
                }

                t.cons[current].mark_changed = true;
            }
        }
        Some(name) => {
            let current = match con {
                None => con_by_mark(t, name),
                Some(c) => Some(c),
            };
            let current = match current {
                None => return,
                Some(c) => c,
            };

            t.cons[current].mark_changed = true;

            if let Some(idx) = t.cons[current].marks_head.iter().position(|m| m.name == name) {
                t.cons[current].marks_head.remove(idx);

                b.ipc_send_window_event(t, "mark", current);
            }
        }
    }
}

/// C: src/con.c:962 `con_for_window`
///
/// Returns the first container below 'con' which wants to swallow this
/// window. (The C `store_match` out-parameter is TODO: the matched Match is
/// not returned yet.)
pub fn con_for_window(t: &Tree, con: ConId, window: Option<WindowId>) -> Option<ConId> {
    let children: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in children {
        for match_ in &t.cons[child].swallow_head {
            if !match_matches_window(t, match_, window) {
                continue;
            }
            return Some(child);
        }
        if let Some(result) = con_for_window(t, child, window) {
            return Some(result);
        }
    }

    let floating: Vec<ConId> = t.cons[con].floating_head.iter().copied().collect();
    for child in floating {
        for match_ in &t.cons[child].swallow_head {
            if !match_matches_window(t, match_, window) {
                continue;
            }
            return Some(child);
        }
        if let Some(result) = con_for_window(t, child, window) {
            return Some(result);
        }
    }

    None
}

/// C: src/con.c:1017 `num_focus_heads` (static)
fn num_focus_heads(t: &Tree, con: ConId) -> usize {
    t.cons[con].focus_head.len()
}

/// C: src/con.c:1027 `get_focus_order`
///
/// Iterate over the container's focus stack and return an array with the
/// containers inside it, ordered from higher focus order to lowest.
pub fn get_focus_order(t: &Tree, con: ConId) -> Vec<ConId> {
    let _ = num_focus_heads(t, con);
    t.cons[con].focus_head.iter().copied().collect()
}

/// C: src/con.c:1049 `set_focus_order`
///
/// Clear the container's focus stack and re-add it using the provided
/// container array. The function doesn't check if the provided array contains
/// the same containers with the previous focus stack but will not add
/// floating containers in the new focus stack if container is not a
/// workspace.
pub fn set_focus_order(t: &mut Tree, con: ConId, focus_order: &[ConId]) {
    let mut focus_heads = t.cons[con].focus_head.len();
    t.cons[con].focus_head.clear();

    let mut idx = 0usize;
    while idx < focus_heads {
        let fc = match focus_order.get(idx) {
            /* NB: the C code increments focus_heads when skipping and can
             * read past the array end (unreachable in practice). */
            None => break,
            Some(&fc) => fc,
        };
        if t.cons[con].r#type != ConType::CT_WORKSPACE && con_inside_floating(t, fc).is_some() {
            focus_heads += 1;
            idx += 1;
            continue;
        }

        t.cons[con].focus_head.push_back(fc);
        idx += 1;
    }
}

/// C: src/con.c:1080 `con_num_children`
///
/// Returns the number of children of this container.
pub fn con_num_children(t: &Tree, con: ConId) -> usize {
    t.cons[con].nodes_head.len()
}

/// C: src/con.c:1092 `con_num_visible_children`
///
/// Returns the number of visible non-floating children of this container.
/// For example, if the container contains a hsplit which has two children,
/// this will return 2 instead of 1.
pub fn con_num_visible_children(t: &Tree, con: Option<ConId>) -> usize {
    let con = match con {
        None => return 0,
        Some(c) => c,
    };

    let mut children = 0usize;
    let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for current in nodes {
        /* Visible leaf nodes are a child. */
        if !con_is_hidden(t, current) && con_is_leaf(t, current) {
            children += 1;
        }
        /* All other containers need to be recursed. */
        else {
            children += con_num_visible_children(t, Some(current));
        }
    }

    children
}

/// C: src/con.c:1113 `con_num_windows`
///
/// Count the number of windows (i.e., leaf containers).
pub fn con_num_windows(t: &Tree, con: Option<ConId>) -> usize {
    let con = match con {
        None => return 0,
        Some(c) => c,
    };

    if con_has_managed_window(t, con) {
        return 1;
    }

    let mut num = 0usize;
    let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for current in nodes {
        num += con_num_windows(t, Some(current));
    }

    let floating: Vec<ConId> = t.cons[con].floating_head.iter().copied().collect();
    for current in floating {
        num += con_num_windows(t, Some(current));
    }

    num
}

/// C: src/con.c:1131 `con_fix_percent`
///
/// Updates the percent attribute of the children of the given container. This
/// function needs to be called when a window is added or removed from a
/// container.
pub fn con_fix_percent(t: &mut Tree, con: ConId) {
    let children = con_num_children(t, con);

    /* calculate how much we have distributed and how many containers with a
     * percentage set we have */
    let mut total = 0.0f64;
    let mut children_with_percent = 0usize;
    let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in &nodes {
        if t.cons[*child].percent > 0.0 {
            total += t.cons[*child].percent;
            children_with_percent += 1;
        }
    }

    /* if there were children without a percentage set, set to a value that
     * will make those children proportional to all others */
    if children_with_percent != children {
        for child in &nodes {
            if t.cons[*child].percent <= 0.0 {
                if children_with_percent == 0 {
                    t.cons[*child].percent = 1.0;
                    total += 1.0;
                } else {
                    /* NB: this is C's running-total behaviour (bug kept). */
                    let p = total / children_with_percent as f64;
                    t.cons[*child].percent = p;
                    total += p;
                }
            }
        }
    }

    /* if we got a zero, just distribute the space equally, otherwise
     * distribute according to the proportions we got */
    if total == 0.0 {
        for child in &nodes {
            t.cons[*child].percent = 1.0 / children as f64;
        }
    } else if total != 1.0 {
        for child in &nodes {
            t.cons[*child].percent /= total;
        }
    }
}

/// C: src/con.c:1173 `con_toggle_fullscreen`
///
/// Toggles fullscreen mode for the given container. If there already is a
/// fullscreen container on this workspace, fullscreen will be disabled and
/// then enabled for the container the user wants to have in fullscreen mode.
pub fn con_toggle_fullscreen(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, fullscreen_mode: i32) {
    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        return;
    }

    if t.cons[con].fullscreen_mode == FullscreenMode::CF_NONE {
        let mode = match fullscreen_mode {
            2 => FullscreenMode::CF_GLOBAL,
            _ => FullscreenMode::CF_OUTPUT,
        };
        con_enable_fullscreen(t, b, con, mode);
    } else {
        con_disable_fullscreen(t, b, con);
    }
}

/// C: src/con.c:1194 `con_set_fullscreen_mode` (static)
///
/// Sets the specified fullscreen mode for the given container, sends the
/// "fullscreen_mode" event and changes the XCB fullscreen property of the
/// container's window, if any.
fn con_set_fullscreen_mode(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, fullscreen_mode: FullscreenMode) {
    t.cons[con].fullscreen_mode = fullscreen_mode;

    /* Send an ipc window "fullscreen_mode" event */
    b.ipc_send_window_event(t, "fullscreen_mode", con);

    /* update _NET_WM_STATE if this container has a window */
    let window = match t.cons[con].window {
        None => return,
        Some(w) => w,
    };

    if fullscreen_mode != FullscreenMode::CF_NONE {
        b.x_set_net_wm_state_fullscreen(t, t.windows[window].id, true);
    } else {
        b.x_set_net_wm_state_fullscreen(t, t.windows[window].id, false);
    }
}

/// C: src/con.c:1234 `con_enable_fullscreen`
///
/// Enables fullscreen mode for the given container, if necessary.
///
/// If the container's mode is already CF_OUTPUT or CF_GLOBAL, the container is
/// kept fullscreen but its mode is set to CF_GLOBAL and CF_OUTPUT,
/// respectively.
///
/// Other fullscreen containers will be disabled first, if they hide the new
/// one.
pub fn con_enable_fullscreen(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, fullscreen_mode: FullscreenMode) {
    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        return;
    }

    assert!(fullscreen_mode == FullscreenMode::CF_GLOBAL || fullscreen_mode == FullscreenMode::CF_OUTPUT);

    if t.cons[con].fullscreen_mode == fullscreen_mode {
        return;
    }

    let con_ws = con_get_workspace(t, con);

    /* Disable any fullscreen container that would conflict the new one. */
    let croot = t.croot.expect("con_enable_fullscreen: no croot");
    let mut fullscreen = con_get_fullscreen_con(t, croot, FullscreenMode::CF_GLOBAL);
    if fullscreen.is_none() {
        if let Some(con_ws) = con_ws {
            fullscreen = con_get_fullscreen_con(t, con_ws, FullscreenMode::CF_OUTPUT);
        }
    }
    if let Some(fullscreen) = fullscreen {
        con_disable_fullscreen(t, b, fullscreen);
    }

    /* Set focus to new fullscreen container. Unless in global fullscreen mode
     * and on another workspace restore focus afterwards.
     * Switch to the container's workspace if mode is global. */
    let focused = t.focused.expect("con_enable_fullscreen: no focused con");
    let cur_ws = con_get_workspace(t, focused);
    let old_focused = t.focused;
    if fullscreen_mode == FullscreenMode::CF_GLOBAL && cur_ws != con_ws {
        workspace_show(t, b, con_ws.expect("con_enable_fullscreen: no workspace"));
    }
    con_activate(t, b, con);
    if fullscreen_mode != FullscreenMode::CF_GLOBAL && cur_ws != con_ws {
        if let Some(old_focused) = old_focused {
            con_activate(t, b, old_focused);
        }
    }

    con_set_fullscreen_mode(t, b, con, fullscreen_mode);
}

/// C: src/con.c:1297 `con_disable_fullscreen`
///
/// Disables fullscreen mode for the given container regardless of the mode,
/// if necessary.
pub fn con_disable_fullscreen(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        return;
    }

    if t.cons[con].fullscreen_mode == FullscreenMode::CF_NONE {
        return;
    }

    con_set_fullscreen_mode(t, b, con, FullscreenMode::CF_NONE);
}

/// C: src/con.c:1333 `_con_move_to_con` (static)
#[allow(clippy::too_many_arguments)]
fn _con_move_to_con(t: &mut Tree, b: &mut dyn ConBackend, mut con: ConId, target: ConId, behind_focused: bool, fix_coordinates: bool, dont_warp: bool, ignore_focus: bool, fix_percentage: bool) -> bool {
    let mut orig_target = target;

    /* Prevent moving if this would violate the fullscreen focus restrictions. */
    let target_ws = con_get_workspace(t, target);
    if !ignore_focus && !con_fullscreen_permits_focusing(t, target_ws) {
        return false;
    }

    if con_is_floating(t, con) {
        con = t.cons[con].parent.expect("_con_move_to_con: floating con has no parent");
    }

    let source_ws = con_get_workspace(t, con);

    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        /* Re-parent all of the old workspace's floating windows. */
        let source_ws = source_ws.expect("_con_move_to_con: no source ws");
        let target_ws = target_ws.expect("_con_move_to_con: no target ws");
        while let Some(child) = t.cons[source_ws].floating_head.front().copied() {
            con_move_to_workspace(t, b, child, target_ws, true, true, false);
        }

        /* If there are no non-floating children, ignore the workspace. */
        if con_is_leaf(t, con) {
            return false;
        }

        con = match workspace_encapsulate(t, b, con) {
            Some(c) => c,
            None => return false,
        };
    }

    /* Save the urgency state so that we can restore it. */
    let urgent = t.cons[con].urgent;

    /* Save the current workspace. So we can call workspace_show() by the end
     * of this function. */
    let focused = t.focused.expect("_con_move_to_con: no focused con");
    let current_ws = con_get_workspace(t, focused);

    let source_output = con_get_output(t, con);
    let dest_output = con_get_output(t, target_ws.expect("_con_move_to_con: no target ws"));

    /* 1: save the container which is going to be focused after the current
     * container is moved away */
    let mut focus_next: Option<ConId> = None;
    if !ignore_focus && source_ws == current_ws && target_ws != source_ws {
        let source_ws = source_ws.expect("_con_move_to_con: no source ws");
        let mut fn_ = con_descend_focused(t, source_ws);
        if fn_ == con || con_has_parent(t, Some(fn_), con) {
            fn_ = con_next_focused(t, con);
        }
        focus_next = Some(fn_);
    }

    /* 2: we go up one level, but only when target is a normal container */
    let mut target = target;
    if t.cons[target].r#type != ConType::CT_WORKSPACE {
        target = t.cons[target].parent.expect("_con_move_to_con: target has no parent");
    }

    /* 3: if the original target is the direct child of a floating container, we
     * can't move con next to it - floating containers have only one child - so
     * we get the workspace instead. */
    if t.cons[target].r#type == ConType::CT_FLOATING_CON {
        orig_target = target;
        target = t.cons[target].parent.expect("_con_move_to_con: floating target has no parent");
    }

    if t.cons[con].r#type == ConType::CT_FLOATING_CON {
        let ws = con_get_workspace(t, target).expect("_con_move_to_con: no ws for floating");
        target = ws;
    }

    if source_output != dest_output {
        /* Take the relative coordinates of the current output, then add them
         * to the coordinate space of the correct output */
        if fix_coordinates && t.cons[con].r#type == ConType::CT_FLOATING_CON {
            let source_output = source_output.expect("_con_move_to_con: no source output");
            let dest_output = dest_output.expect("_con_move_to_con: no dest output");
            let old_rect = t.cons[source_output].rect;
            let new_rect = t.cons[dest_output].rect;
            floating_fix_coordinates(t, con, &old_rect, &new_rect);
        }
    }

    /* If moving a fullscreen container and the destination already has a
     * fullscreen window on it, un-fullscreen the target's fullscreen con.
     * con->fullscreen_mode is not enough in some edge cases:
     * 1. con is CT_FLOATING_CON, child is fullscreen.
     * 2. con is the parent of a fullscreen container, can be triggered by
     * moving the parent with command criteria. */
    let target_ws2 = con_get_workspace(t, target_ws.expect("_con_move_to_con: no target ws"));
    let mut fullscreen = match target_ws2 {
        Some(ws) => con_get_fullscreen_con(t, ws, FullscreenMode::CF_OUTPUT),
        None => None,
    };
    let con_has_fullscreen = t.cons[con].fullscreen_mode != FullscreenMode::CF_NONE
        || con_get_fullscreen_con(t, con, FullscreenMode::CF_GLOBAL).is_some()
        || con_get_fullscreen_con(t, con, FullscreenMode::CF_OUTPUT).is_some();
    if con_has_fullscreen && fullscreen.is_some() {
        let fs = fullscreen.expect("con_has_fullscreen && fullscreen.is_some()");
        con_toggle_fullscreen(t, b, fs, FullscreenMode::CF_OUTPUT as i32);
        fullscreen = None;
    }

    /* 4: re-attach the con to the parent of this focused container */
    let parent = t.cons[con].parent.expect("_con_move_to_con: con has no parent");
    con_detach(t, con);
    _con_attach(t, b, con, target, if behind_focused { None } else { Some(orig_target) }, !behind_focused);

    /* 5: fix the percentages */
    if fix_percentage {
        con_fix_percent(t, parent);
        t.cons[con].percent = 0.0;
        con_fix_percent(t, target);
    }

    /* 6: focus the con on the target workspace, but only within that
     * workspace, that is, don't move focus away if the target workspace is
     * invisible.
     * We don't focus the con for i3 pseudo workspaces like __i3_scratch and
     * we don't focus when there is a fullscreen con on that workspace. We
     * also don't do it if the caller requested to ignore focus. */
    let target_ws = con_get_workspace(t, target_ws.expect("_con_move_to_con: no target ws"));
    if !ignore_focus && !con_is_internal(t, target_ws.expect("_con_move_to_con: internal ws")) && fullscreen.is_none() {
        let dest_output = dest_output.expect("_con_move_to_con: no dest output");
        /* We need to save the focused workspace on the output in case the
         * new workspace is hidden and it's necessary to immediately switch
         * back to the originally-focused workspace. */
        let old_focus_ws = output_get_content(t, dest_output).and_then(|c| t.cons[c].focus_head.front().copied());
        let old_focus = t.focused;
        con_activate(t, b, con_descend_focused(t, con));

        if old_focus_ws == current_ws && old_focus.map(|o| t.cons[o].r#type != ConType::CT_WORKSPACE).unwrap_or(false) {
            /* Restore focus to the currently focused container. */
            con_activate(t, b, old_focus.expect("_con_move_to_con: old_focus"));
        } else if con_get_workspace_of_focused(t) != old_focus_ws {
            /* Restore focus if the output's focused workspace has changed. */
            if let Some(old_focus_ws) = old_focus_ws {
                con_focus(t, b, con_descend_focused(t, old_focus_ws));
            }
        }
    }

    /* 7: when moving to another workspace, we leave the focus on the current
     * workspace. (see also #809) */
    if !ignore_focus {
        if let Some(current_ws) = current_ws {
            workspace_show(t, b, current_ws);
        }
        if dont_warp {
            b.x_set_warp_to(t, None);
        }
    }

    /* Set focus only if con was on current workspace before moving.
     * Otherwise we would give focus to some window on different workspace. */
    if let Some(focus_next) = focus_next {
        con_activate(t, b, con_descend_focused(t, focus_next));
    }

    /* 8. If anything within the container is associated with a startup sequence,
     * delete it so child windows won't be created on the old workspace. */
    if !con_is_leaf(t, con) {
        let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
        for child in nodes {
            if t.cons[child].window.is_none() {
                continue;
            }
            b.startup_sequence_delete_by_window(t, t.cons[child].window.expect("checked above"));
        }
    }

    if let Some(window) = t.cons[con].window {
        b.startup_sequence_delete_by_window(t, window);
    }

    /* 9. If the container was marked urgent, move the urgency hint. */
    if urgent {
        if let Some(source_ws) = source_ws {
            workspace_update_urgent_flag(t, b, source_ws);
        }
        con_set_urgency(t, b, con, true);
    }

    /* Ensure the container will be redrawn. */
    t.cons[con].deco_render_params = None;

    con_on_remove_child(t, b, parent);

    b.ipc_send_window_event(t, "move", con);
    b.ewmh_update_wm_desktop(t);
    true
}

/// Helper: con_get_workspace(focused) as Option.
fn con_get_workspace_of_focused(t: &Tree) -> Option<ConId> {
    t.focused.and_then(|f| con_get_workspace(t, f))
}

/// C: src/con.c:1492 `con_move_to_target`
pub fn con_move_to_target(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, target: ConId) -> bool {
    /* For target containers in the scratchpad, we just send the window to the scratchpad. */
    let scratch = workspace_get(t, b, "__i3_scratch");
    if con_get_workspace(t, target) == Some(scratch) {
        crate::scratchpad::scratchpad_move(t, b, con);
        return true;
    }

    /* For floating target containers, we just send the window to the same workspace. */
    if con_is_floating(t, target) {
        let ws = con_get_workspace(t, target).expect("con_move_to_target: no ws");
        con_move_to_workspace(t, b, con, ws, true, false, false);
        return true;
    }

    if t.cons[target].r#type == ConType::CT_WORKSPACE && con_is_leaf(t, target) {
        con_move_to_workspace(t, b, con, target, true, false, false);
        return true;
    }

    /* For split containers, we use the currently focused container within it.
     * This allows setting marks on, e.g., tabbed containers which will move
     * con to a new tab behind the focused tab. */
    let mut target = target;
    if con_is_split(t, target) {
        target = t.cons[target].focus_head.front().copied().expect("con_move_to_target: split target without focus");
    }

    if con == target || con_has_parent(t, Some(target), con) {
        return false;
    }

    _con_move_to_con(t, b, con, target, false, true, false, false, true)
}

/// C: src/con.c:1522 `con_move_to_mark`
///
/// Moves the given container to the given mark.
pub fn con_move_to_mark(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, mark: &str) -> bool {
    let target = match con_by_mark(t, mark) {
        None => return false,
        Some(target) => target,
    };

    con_move_to_target(t, b, con, target)
}

/// C: src/con.c:1545 `con_move_to_workspace`
///
/// Moves the given container to the currently focused container on the given
/// workspace.
///
/// The fix_coordinates flag will translate the current coordinates (offset
/// from the monitor position basically) to appropriate coordinates on the
/// destination workspace.
///
/// The dont_warp flag disables pointer warping and will be set when this
/// function is called while dragging a floating window.
///
/// If ignore_focus is set, the container will be moved without modifying
/// focus at all.
pub fn con_move_to_workspace(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, workspace: ConId, fix_coordinates: bool, dont_warp: bool, ignore_focus: bool) {
    assert!(t.cons[workspace].r#type == ConType::CT_WORKSPACE);

    let source_ws = con_get_workspace(t, con);
    if Some(workspace) == source_ws {
        /* Not moving, already there */
        return;
    }

    let target = con_descend_focused(t, workspace);
    _con_move_to_con(t, b, con, target, true, fix_coordinates, dont_warp, ignore_focus, true);
}

/// C: src/con.c:1573 `con_move_to_output`
///
/// Moves the given container to the currently focused container on the
/// visible workspace on the given output.
pub fn con_move_to_output(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, output: usize, fix_coordinates: bool) {
    let output_con = t.outputs[output].con.expect("con_move_to_output: output has no con");
    let content = crate::output::output_get_content(t, output_con);
    let mut ws = None;
    if let Some(content) = content {
        for &child in &t.cons[content].nodes_head {
            if workspace_is_visible(t, child) {
                ws = Some(child);
                break;
            }
        }
    }
    let ws = ws.expect("con_move_to_output: no visible workspace on output");
    con_move_to_workspace(t, b, con, ws, fix_coordinates, false, false);
}

/// C: src/con.c:1586 `con_move_to_output_name`
///
/// Moves the given container to the currently focused container on the
/// visible workspace on the output specified by the given name.
pub fn con_move_to_output_name(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, name: &str, fix_coordinates: bool) -> bool {
    let current_output = crate::output::get_output_for_con(t, con);
    let output = crate::output::get_output_from_string(t, current_output, name);
    let output = match output {
        None => return false,
        Some(o) => o,
    };

    con_move_to_output(t, b, con, output, fix_coordinates);
    true
}

/// C: src/con.c:1609 `con_orientation`
///
/// Returns the orientation of the given container (for stacked containers,
/// vertical orientation is used regardless of the actual orientation of the
/// container).
pub fn con_orientation(t: &Tree, con: ConId) -> Orientation {
    match t.cons[con].layout {
        /* stacking containers behave like they are in vertical orientation */
        Layout::L_SPLITV | Layout::L_STACKED => Orientation::VERT,
        /* tabbed containers behave like they are in vertical orientation */
        Layout::L_SPLITH | Layout::L_TABBED => Orientation::HORIZ,
        Layout::L_DEFAULT => panic!("Someone called con_orientation() on a con with L_DEFAULT, this is a bug in the code."),
        Layout::L_DOCKAREA | Layout::L_OUTPUT => panic!("con_orientation() called on dockarea/output container"),
    }
}

/// C: src/con.c:1640 `con_next_focused`
///
/// Returns the container which will be focused next when the given container
/// is not available anymore. Called in tree_close_internal and
/// con_move_to_workspace to properly restore focus.
pub fn con_next_focused(t: &Tree, con: ConId) -> ConId {
    let mut con = con;
    /* dock clients cannot be focused, so we focus the workspace instead */
    let parent = t.cons[con].parent.expect("con_next_focused: no parent");
    if t.cons[parent].r#type == ConType::CT_DOCKAREA {
        let grandparent = t.cons[parent].parent.expect("con_next_focused: no grandparent");
        let content = crate::output::output_get_content(t, grandparent).expect("con_next_focused: no content");
        return con_descend_focused(t, content);
    }
    if con_is_floating(t, con) {
        con = t.cons[con].parent.expect("con_next_focused: floating con has no parent");
    }

    let parent = t.cons[con].parent.expect("con_next_focused: no parent");
    /* if 'con' is not the first entry in the focus stack, use the first one as
     * it's currently focused already */
    let mut next = match t.cons[parent].focus_head.front() {
        None => panic!("con_next_focused: empty focus_head"),
        Some(&n) => n,
    };
    if next != con {
        /* try to focus the next container on the same level as this one or fall
         * back to its parent */
    } else {
        next = match t.focus_next(con) {
            Some(n) => n,
            None => parent,
        };
    }

    /* now go down the focus stack as far as possible, excluding the current container */
    while !t.cons[next].focus_head.is_empty() && t.cons[next].focus_head.front() != Some(&con) {
        next = t.cons[next].focus_head[0];
    }

    if t.cons[con].r#type == ConType::CT_FLOATING_CON && next != parent {
        next = con_descend_focused(t, next);
    }

    next
}

/// C: src/con.c:1680 `con_descend_focused`
///
/// Returns the focused con inside this client, descending the tree as far as
/// possible. This comes in handy when attaching a con to a workspace at the
/// currently focused position, for example.
pub fn con_descend_focused(t: &Tree, con: ConId) -> ConId {
    let mut next = con;
    while Some(next) != t.focused && !t.cons[next].focus_head.is_empty() {
        next = t.cons[next].focus_head[0];
    }
    next
}

/// C: src/con.c:1694 `con_descend_tiling_focused`
///
/// Works like con_descend_focused but considers only tiling cons.
pub fn con_descend_tiling_focused(t: &Tree, con: ConId) -> ConId {
    let mut next = con;
    if Some(next) == t.focused {
        return next;
    }
    loop {
        let before = next;
        for &child in &t.cons[next].focus_head {
            if t.cons[child].r#type == ConType::CT_FLOATING_CON {
                continue;
            }

            next = child;
            break;
        }
        if !(before != next && Some(next) != t.focused) {
            break;
        }
    }
    next
}

/// C: src/con.c:1721 `con_descend_direction`
///
/// Returns the leftmost, rightmost, etc. container in sub-tree. For example,
/// if direction is D_LEFT, then we return the rightmost container and if
/// direction is D_RIGHT, we return the leftmost container.
pub fn con_descend_direction(t: &Tree, con: ConId, direction: Direction) -> ConId {
    let mut most: Option<ConId> = None;
    let orientation = con_orientation(t, con);

    if direction == Direction::D_LEFT || direction == Direction::D_RIGHT {
        if orientation == Orientation::HORIZ {
            /* If the direction is horizontal, we can use either the first
             * (D_RIGHT) or the last con (D_LEFT) */
            most = if direction == Direction::D_RIGHT {
                t.cons[con].nodes_head.front().copied()
            } else {
                t.cons[con].nodes_head.back().copied()
            };
        } else if orientation == Orientation::VERT {
            /* Wrong orientation. We use the last focused con. Within that con,
             * we recurse to chose the left/right con or at least the last
             * focused one. */
            for &current in &t.cons[con].focus_head {
                if t.cons[current].r#type != ConType::CT_FLOATING_CON {
                    most = Some(current);
                    break;
                }
            }
        } else {
            /* If the con has no orientation set, it's not a split container
             * but a container with a client window, so stop recursing */
            return con;
        }
    }

    if direction == Direction::D_UP || direction == Direction::D_DOWN {
        if orientation == Orientation::VERT {
            /* If the direction is vertical, we can use either the first
             * (D_DOWN) or the last con (D_UP) */
            most = if direction == Direction::D_UP {
                t.cons[con].nodes_head.back().copied()
            } else {
                t.cons[con].nodes_head.front().copied()
            };
        } else if orientation == Orientation::HORIZ {
            /* Wrong orientation. We use the last focused con. Within that con,
             * we recurse to chose the top/bottom con or at least the last
             * focused one. */
            for &current in &t.cons[con].focus_head {
                if t.cons[current].r#type != ConType::CT_FLOATING_CON {
                    most = Some(current);
                    break;
                }
            }
        } else {
            /* If the con has no orientation set, it's not a split container
             * but a container with a client window, so stop recursing */
            return con;
        }
    }

    match most {
        None => con,
        Some(most) => con_descend_direction(t, most, direction),
    }
}

/// C: src/con.c:1793 `has_outer_gaps` (static)
fn has_outer_gaps(gaps: crate::data::gaps_t) -> bool {
    gaps.top > 0 || gaps.right > 0 || gaps.bottom > 0 || gaps.left > 0
}

/// C: src/con.c:1801 `con_draw_decoration_into_frame`
///
/// Returns whether the window decoration (title bar) should be drawn into the
/// X11 frame window of this container (default) or into the X11 frame window
/// of the parent container (for stacked/tabbed containers).
pub fn con_draw_decoration_into_frame(t: &Tree, con: ConId) -> bool {
    con_is_leaf(t, con)
        && con_border_style(t, con) == BorderStyle::BS_NORMAL
        && (t.cons[con].parent.is_none()
            || (t.cons[t.cons[con].parent.expect("con_draw_decoration_into_frame: no parent")].layout != Layout::L_TABBED
                && t.cons[t.cons[con].parent.expect("con_draw_decoration_into_frame: no parent")].layout != Layout::L_STACKED))
}

/// C: src/con.c:1814 `con_border_style_rect_without_title` (static)
fn con_border_style_rect_without_title(t: &Tree, con: ConId) -> Rect {
    use crate::data::{ADJ_LEFT_SCREEN_EDGE, ADJ_LOWER_SCREEN_EDGE, ADJ_RIGHT_SCREEN_EDGE, ADJ_UPPER_SCREEN_EDGE, HEBM_SMART, HEBM_SMART_NO_GAPS};
    use crate::gaps::calculate_effective_gaps;

    let ws = con_get_workspace(t, con);
    if (t.config.hide_edge_borders == HEBM_SMART && con_num_visible_children(t, ws) <= 1)
        || (t.config.hide_edge_borders == HEBM_SMART_NO_GAPS
            && con_num_visible_children(t, ws) <= 1
            && !has_outer_gaps(calculate_effective_gaps(t, con)))
    {
        if !con_is_floating(t, con) {
            return Rect::new(0, 0, 0, 0);
        }
    }

    let borders_to_hide;
    let mut border_width = t.cons[con].current_border_width;
    let mut result;
    if t.cons[con].current_border_width < 0 {
        if con_is_floating(t, con) {
            border_width = t.config.default_floating_border_width;
        } else {
            border_width = t.config.default_border_width;
        }
    }
    /* Shortcut to avoid calling con_adjacent_borders() on dock containers. */
    let border_style = con_border_style(t, con);
    if border_style == BorderStyle::BS_NONE {
        return Rect::new(0, 0, 0, 0);
    }
    if border_style == BorderStyle::BS_NORMAL {
        result = Rect::new(border_width as u32, 0, (-(2 * border_width)) as u32, (-(border_width)) as u32);
    } else {
        result = Rect::new(border_width as u32, border_width as u32, (-(2 * border_width)) as u32, (-(2 * border_width)) as u32);
    }

    /* If hide_edge_borders is set to no_gaps and it did not pass the no border check, show all borders */
    if t.config.hide_edge_borders == HEBM_SMART_NO_GAPS {
        /* NB: C does `con_adjacent_borders(con) & HEBM_NONE` here, which is a
         * bitwise-and with 0 — always ADJ_NONE (bug kept). */
        borders_to_hide = con_adjacent_borders(t, con) & crate::data::HEBM_NONE;
    } else {
        borders_to_hide = AdjacentBitAnd::bitand(con_adjacent_borders(t, con), t.config.hide_edge_borders);
    }

    if (borders_to_hide & ADJ_LEFT_SCREEN_EDGE).0 != 0 {
        result.x = result.x.wrapping_sub(border_width as u32);
        result.width = result.width.wrapping_add(border_width as u32);
    }
    if (borders_to_hide & ADJ_RIGHT_SCREEN_EDGE).0 != 0 {
        result.width = result.width.wrapping_add(border_width as u32);
    }
    if (borders_to_hide & ADJ_UPPER_SCREEN_EDGE).0 != 0 && border_style != BorderStyle::BS_NORMAL {
        result.y = result.y.wrapping_sub(border_width as u32);
        result.height = result.height.wrapping_add(border_width as u32);
    }
    if (borders_to_hide & ADJ_LOWER_SCREEN_EDGE).0 != 0 {
        result.height = result.height.wrapping_add(border_width as u32);
    }
    result
}

/// Helper for `Adjacent & HideEdgeBordersMode` (C's implicit enum mixing).
struct AdjacentBitAnd;
impl AdjacentBitAnd {
    fn bitand(a: crate::data::Adjacent, h: crate::data::HideEdgeBordersMode) -> crate::data::Adjacent {
        crate::data::Adjacent(a.0 & h.0)
    }
}

/// C: src/con.c:1870 `con_border_style_rect`
///
/// Returns a "relative" Rect which contains the amount of pixels that need
/// to be added to the original Rect to get the final position (obviously the
/// amount of pixels for normal, 1pixel and borderless are different).
pub fn con_border_style_rect(t: &Tree, con: ConId) -> Rect {
    let mut result = con_border_style_rect_without_title(t, con);
    if con_border_style(t, con) == BorderStyle::BS_NORMAL && con_draw_decoration_into_frame(t, con) {
        let deco_height = crate::render::render_deco_height(t);
        result.y = result.y.wrapping_add(deco_height as u32);
        result.height = result.height.wrapping_sub(deco_height as u32);
    }
    result
}

/// C: src/con.c:1889 `con_adjacent_borders`
///
/// Returns adjacent borders of the window. We need this if hide_edge_borders
/// is enabled.
pub fn con_adjacent_borders(t: &Tree, con: ConId) -> crate::data::Adjacent {
    use crate::data::{ADJ_LEFT_SCREEN_EDGE, ADJ_LOWER_SCREEN_EDGE, ADJ_RIGHT_SCREEN_EDGE, ADJ_UPPER_SCREEN_EDGE, Adjacent};
    let mut result = Adjacent(0);
    /* Floating windows are never adjacent to any other window, so
       don't hide their border(s). This prevents bug #998. */
    if con_is_floating(t, con) {
        return result;
    }

    let workspace = match con_get_workspace(t, con) {
        None => return result,
        Some(ws) => ws,
    };

    let con_rect = t.cons[con].rect;
    let ws_rect = t.cons[workspace].rect;
    if con_rect.x == ws_rect.x {
        result |= ADJ_LEFT_SCREEN_EDGE;
    }
    if con_rect.x.wrapping_add(con_rect.width) == ws_rect.x.wrapping_add(ws_rect.width) {
        result |= ADJ_RIGHT_SCREEN_EDGE;
    }
    if con_rect.y == ws_rect.y {
        result |= ADJ_UPPER_SCREEN_EDGE;
    }
    if con_rect.y.wrapping_add(con_rect.height) == ws_rect.y.wrapping_add(ws_rect.height) {
        result |= ADJ_LOWER_SCREEN_EDGE;
    }
    result
}

/// C: src/con.c:1911 `con_border_style`
///
/// Use this function to get a container's border style. This is important
/// because when inside a stack, the border style is always BS_NORMAL.
/// For tabbed mode, the same applies, with one exception: when the container
/// is borderless and the only element in the tabbed container, the border is
/// not rendered.
///
/// For children of a CT_DOCKAREA, the border style is always none.
pub fn con_border_style(t: &Tree, con: ConId) -> BorderStyle {
    if t.cons[con].fullscreen_mode == FullscreenMode::CF_OUTPUT || t.cons[con].fullscreen_mode == FullscreenMode::CF_GLOBAL {
        return BorderStyle::BS_NONE;
    }

    if let Some(parent) = t.cons[con].parent {
        if t.cons[parent].layout == Layout::L_STACKED {
            return if con_num_children(t, parent) == 1 { t.cons[con].border_style } else { BorderStyle::BS_NORMAL };
        }

        if t.cons[parent].layout == Layout::L_TABBED && t.cons[con].border_style != BorderStyle::BS_NORMAL {
            return if con_num_children(t, parent) == 1 { t.cons[con].border_style } else { BorderStyle::BS_NORMAL };
        }

        if t.cons[parent].r#type == ConType::CT_DOCKAREA {
            return BorderStyle::BS_NONE;
        }
    }

    t.cons[con].border_style
}

/// C: src/con.c:1952 `con_set_border_style`
///
/// Sets the given border style on con, correctly keeping the position/size of
/// a floating window.
pub fn con_set_border_style(t: &mut Tree, con: ConId, mut border_style: BorderStyle, border_width: i32) {
    if border_style > t.cons[con].max_user_border_style {
        border_style = t.cons[con].max_user_border_style;
    }

    /* Handle the simple case: non-floating containerns */
    if !con_is_floating(t, con) {
        t.cons[con].border_style = border_style;
        t.cons[con].current_border_width = border_width;
        return;
    }

    /* For floating containers, we want to keep the position/size of the
     * *window* itself. We first add the border pixels to con->rect to make
     * con->rect represent the absolute position of the window (same for
     * parent). Then, we change the border style and subtract the new border
     * pixels. For the parent, we do the same also for the decoration. */
    let parent = t.cons[con].parent.expect("con_set_border_style: no parent");
    let bsr = con_border_style_rect(t, con);

    t.cons[con].rect = rect_add(t.cons[con].rect, bsr);
    t.cons[parent].rect = rect_add(t.cons[parent].rect, bsr);

    /* Change the border style, get new border/decoration values. */
    t.cons[con].border_style = border_style;
    t.cons[con].current_border_width = border_width;
    let bsr = con_border_style_rect(t, con);

    t.cons[con].rect = rect_sub(t.cons[con].rect, bsr);
    t.cons[parent].rect = rect_sub(t.cons[parent].rect, bsr);
}

/// C: src/con.c:1996 `con_set_layout`
///
/// This function changes the layout of a given container. Use it to handle
/// special cases like changing a whole workspace to stacked/tabbed (creates a
/// new split container before).
pub fn con_set_layout(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, layout: Layout) {
    let mut con = con;

    /* Users can focus workspaces, but not any higher in the hierarchy.
     * Focus on the workspace is a special case, since in every other case, the
     * user means "change the layout of the parent split container". */
    if t.cons[con].r#type != ConType::CT_WORKSPACE {
        con = t.cons[con].parent.expect("con_set_layout: no parent");
    }

    /* We fill in last_split_layout when switching to a different layout
     * since there are many places in the code that don't use
     * con_set_layout(). */
    if t.cons[con].layout == Layout::L_SPLITH || t.cons[con].layout == Layout::L_SPLITV {
        t.cons[con].last_split_layout = t.cons[con].layout;
    }

    /* When the container type is CT_WORKSPACE, the user wants to change the
     * whole workspace into stacked/tabbed mode. To do this and still allow
     * intuitive operations (like level-up and then opening a new window), we
     * need to create a new split container. */
    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        if con_num_children(t, con) == 0 {
            let ws_layout = if layout == Layout::L_STACKED || layout == Layout::L_TABBED { layout } else { Layout::L_DEFAULT };
            t.cons[con].workspace_layout = ws_layout;
            t.cons[con].layout = layout;
        } else if layout == Layout::L_STACKED || layout == Layout::L_TABBED || layout == Layout::L_SPLITV || layout == Layout::L_SPLITH {
            /* 1: create a new split container */
            let new = con_new(t, b, None, None);
            t.cons[new].parent = Some(con);

            /* 2: Set the requested layout on the split container and mark it as
             * split. */
            t.cons[new].layout = layout;
            t.cons[new].last_split_layout = t.cons[con].last_split_layout;

            /* 3: move the existing cons of this workspace below the new con */
            let focus_order = get_focus_order(t, con);

            while let Some(child) = t.cons[con].nodes_head.front().copied() {
                con_detach(t, child);
                con_attach(t, b, child, new, true);
            }

            set_focus_order(t, new, &focus_order);

            /* 4: attach the new split container to the workspace */
            con_attach(t, b, new, con, false);

            let croot = t.croot.expect("con_set_layout: no croot");
            tree_flatten(t, b, croot);
            con_force_split_parents_redraw(t, con);
            return;
        }
    }

    if t.cons[con].r#type != ConType::CT_WORKSPACE
        && t.cons[t.cons[con].parent.expect("con_set_layout: no parent")].r#type != ConType::CT_WORKSPACE
        && con_num_children(t, con) == 1
        && con_num_children(t, t.cons[con].parent.expect("con_set_layout: no parent")) == 1
    {
        /* Special case: Avoid creating redundant containers (#3001):
         * split h / v (tree_split()) will avoid creating new containers when
         * the target container is already a single child in L_SPLITH /
         * L_SPLITV. However, if the layout is tabbed / stacked, a new split is
         * created. This means, however, that when the user continuously
         * switches between split h/v and tabbed / stacked, an endless series
         * of 1-child containers will be created. Since a single level of split
         * containers on top of tabbed / stacked containers are useful, we want
         * to avoid this situation here.
         * Example of past behaviour: S[V[w]] -> S[S[w]] -> S[S[V[w]]] -> …
         * Example of desired behaviour: S[V[w]] -> S[w] -> S[v[w]] -> …
         * Therefore, when both the current & parent containers have a single
         * child, we just close the redundant middle container and proceed with
         * the parent. */
        let parent = t.cons[con].parent.expect("con_set_layout: no parent");
        let child = t.cons[con].nodes_head[0];
        con_detach(t, child);
        con_attach(t, b, child, parent, true);
        t.cons[parent].last_split_layout = t.cons[con].last_split_layout;
        tree_close_internal(t, b, con, KillWindow::DONT_KILL_WINDOW, true);
        con = parent;
    }

    if layout == Layout::L_DEFAULT {
        /* Special case: the layout formerly known as "default" (in combination
         * with an orientation). Since we switched to splith/splitv layouts,
         * using the "default" layout (which "only" should happen when using
         * legacy configs) is using the last split layout (either splith or
         * splitv) in order to still do the same thing. */
        t.cons[con].layout = t.cons[con].last_split_layout;
        /* In case last_split_layout was not initialized… */
        if t.cons[con].layout == Layout::L_DEFAULT {
            t.cons[con].layout = Layout::L_SPLITH;
        }
    } else {
        t.cons[con].layout = layout;
    }
    con_force_split_parents_redraw(t, con);
}

/// C: src/con.c:2083 `con_toggle_layout`
///
/// This function toggles the layout of a given container. toggle_mode can be
/// either 'default' (toggle only between stacked/tabbed/last_split_layout),
/// 'split' (toggle only between splitv/splith) or 'all' (toggle between all
/// layouts).
pub fn con_toggle_layout(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, toggle_mode: &str) {
    let mut parent = con;
    /* Users can focus workspaces, but not any higher in the hierarchy.
     * Focus on the workspace is a special case, since in every other case,
     * the user means "change the layout of the parent split container". */
    if t.cons[con].r#type != ConType::CT_WORKSPACE {
        parent = t.cons[con].parent.expect("con_toggle_layout: no parent");
    }

    let delim = " ";

    if toggle_mode.eq_ignore_ascii_case("split") || toggle_mode.contains(delim) {
        /* L_DEFAULT is used as a placeholder value to distinguish if
         * the first layout has already been saved. (it can never be L_DEFAULT) */
        let mut new_layout = Layout::L_DEFAULT;
        let mut current_layout_found = false;
        /* C: strtok over a duplicate of toggle_mode */
        let cur_toks: Vec<&str> = toggle_mode.split(' ').collect();

        for cur_tok in cur_toks {
            // C: src/con.c:2129 'for (layout_t layout; ...)' — no initializer in C either.
            let mut layout;
            if cur_tok.eq_ignore_ascii_case("split") {
                /* Toggle between splits. When the current layout is not a split
                 * layout, we just switch back to last_split_layout. Otherwise, we
                 * change to the opposite split layout. */
                if t.cons[parent].layout != Layout::L_SPLITH && t.cons[parent].layout != Layout::L_SPLITV {
                    layout = t.cons[parent].last_split_layout;
                    /* In case last_split_layout was not initialized… */
                    if layout == Layout::L_DEFAULT {
                        layout = Layout::L_SPLITH;
                    }
                } else {
                    layout = if t.cons[parent].layout == Layout::L_SPLITH { Layout::L_SPLITV } else { Layout::L_SPLITH };
                }
            } else {
                match crate::util::layout_from_name(cur_tok) {
                    Some(l) if l != Layout::L_DEFAULT => layout = l,
                    _ => {
                        /* The token was not recognized and has been skipped. */
                        continue;
                    }
                }
            }

            /* If none of the specified layouts match the current,
             * fall back to the first layout in the list */
            if new_layout == Layout::L_DEFAULT {
                new_layout = layout;
            }

            /* We found the active layout in the last iteration, so
             * now let's activate the current layout (next in list) */
            if current_layout_found {
                new_layout = layout;
                break;
            }

            if t.cons[parent].layout == layout {
                current_layout_found = true;
            }
        }

        if new_layout != Layout::L_DEFAULT {
            con_set_layout(t, b, con, new_layout);
        }
    } else if toggle_mode.eq_ignore_ascii_case("all") || toggle_mode.eq_ignore_ascii_case("default") {
        if t.cons[parent].layout == Layout::L_STACKED {
            con_set_layout(t, b, con, Layout::L_TABBED);
        } else if t.cons[parent].layout == Layout::L_TABBED {
            if toggle_mode.eq_ignore_ascii_case("all") {
                con_set_layout(t, b, con, Layout::L_SPLITH);
            } else {
                con_set_layout(t, b, con, t.cons[parent].last_split_layout);
            }
        } else if t.cons[parent].layout == Layout::L_SPLITH || t.cons[parent].layout == Layout::L_SPLITV {
            if toggle_mode.eq_ignore_ascii_case("all") {
                /* When toggling through all modes, we toggle between
                 * splith/splitv, whereas normally we just directly jump to
                 * stacked. */
                if t.cons[parent].layout == Layout::L_SPLITH {
                    con_set_layout(t, b, con, Layout::L_SPLITV);
                } else {
                    con_set_layout(t, b, con, Layout::L_STACKED);
                }
            } else {
                con_set_layout(t, b, con, Layout::L_STACKED);
            }
        }
    }
}

/// C: src/con.c:2197 `con_on_remove_child` (the sole on_remove_child
/// implementation; the C function pointer is replaced by a direct call).
///
/// Callback which will be called when removing a child from the given con.
/// Kills the container if it is empty and replaces it with the child if there
/// is exactly one child.
pub fn con_on_remove_child(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    /* Every container 'above' (in the hierarchy) the workspace content should
     * not be closed when the last child was removed */
    if t.cons[con].r#type == ConType::CT_OUTPUT
        || t.cons[con].r#type == ConType::CT_ROOT
        || t.cons[con].r#type == ConType::CT_DOCKAREA
        || t.cons[con].parent.map(|p| t.cons[p].r#type == ConType::CT_OUTPUT).unwrap_or(false)
    {
        return;
    }

    /* For workspaces, close them only if they're not visible anymore */
    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        if t.cons[con].focus_head.is_empty() && !workspace_is_visible(t, con) {
            /* C marshals the "empty" workspace event here; the payload
             * marshalling is the IPC layer's job. */
            b.ipc_send_workspace_event(t, "empty", con, None);
            tree_close_internal(t, b, con, KillWindow::DONT_KILL_WINDOW, false);
        }
        return;
    }

    con_force_split_parents_redraw(t, con);
    let urgent = con_has_urgent_child(t, con);
    t.cons[con].urgent = urgent;
    con_update_parents_urgency(t, con);

    /* TODO: check if this container would swallow any other client and
     * don't close it automatically. */
    let children = con_num_children(t, con);
    if children == 0 {
        tree_close_internal(t, b, con, KillWindow::DONT_KILL_WINDOW, false);
    }
}

/// C: src/con.c:2258 `con_minimum_size`
///
/// Determines the minimum size of the given con by looking at its children
/// (for split/stacked/tabbed cons). Will be called when resizing floating
/// cons.
pub fn con_minimum_size(t: &Tree, con: ConId) -> Rect {
    if con_is_leaf(t, con) {
        /* leaf node, returning 75x50 */
        return Rect::new(0, 0, 75, 50);
    }

    if t.cons[con].r#type == ConType::CT_FLOATING_CON {
        /* floating con */
        let child = t.cons[con].nodes_head[0];
        return con_minimum_size(t, child);
    }

    if t.cons[con].layout == Layout::L_STACKED || t.cons[con].layout == Layout::L_TABBED {
        let mut max_width = 0u32;
        let mut max_height = 0u32;
        let mut deco_height = 0u32;
        let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
        for child in nodes {
            let min = con_minimum_size(t, child);
            deco_height = deco_height.wrapping_add(t.cons[child].deco_rect.height);
            max_width = std::cmp::max(max_width, min.width);
            max_height = std::cmp::max(max_height, min.height);
        }
        return Rect::new(0, 0, max_width, max_height.wrapping_add(deco_height));
    }

    /* For horizontal/vertical split containers we sum up the width (h-split)
     * or height (v-split) and use the maximum of the height (h-split) or width
     * (v-split) as minimum size. */
    if con_is_split(t, con) {
        let mut width = 0u32;
        let mut height = 0u32;
        let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
        for child in nodes {
            let min = con_minimum_size(t, child);
            if t.cons[con].layout == Layout::L_SPLITH {
                width = width.wrapping_add(min.width);
                height = std::cmp::max(height, min.height);
            } else {
                height = height.wrapping_add(min.height);
                width = std::cmp::max(width, min.width);
            }
        }
        return Rect::new(0, 0, width, height);
    }

    panic!("con_minimum_size: unhandled case, type = {:?}, layout = {:?}, split = {}", t.cons[con].r#type, t.cons[con].layout, con_is_split(t, con));
}

/// C: src/con.c:2303 `con_fullscreen_permits_focusing`
///
/// Returns true if changing the focus to con would be allowed considering
/// the fullscreen focus constraints.
pub fn con_fullscreen_permits_focusing(t: &Tree, con: Option<ConId>) -> bool {
    /* No focus, no problem. */
    let focused = match t.focused {
        None => return true,
        Some(f) => f,
    };

    /* Find the first fullscreen ascendent. */
    let mut fs = Some(focused);
    while let Some(f) = fs {
        if t.cons[f].fullscreen_mode != FullscreenMode::CF_NONE {
            break;
        }
        fs = t.cons[f].parent;
    }

    /* fs must be non-None since the workspace con doesn't have CF_NONE and
     * there always has to be a workspace con in the hierarchy. */
    let fs = fs.expect("con_fullscreen_permits_focusing: no fullscreen ancestor");
    /* The most common case is we hit the workspace level. In this
     * situation, changing focus is also harmless. */
    if t.cons[fs].r#type == ConType::CT_WORKSPACE {
        return true;
    }

    /* Allow it if the container itself is the fullscreen container. */
    if con == Some(fs) {
        return true;
    }

    /* If fullscreen is per-output, the focus being in a different workspace is
     * sufficient to guarantee that change won't leave fullscreen in bad shape. */
    let con_ws = con.and_then(|c| con_get_workspace(t, c));
    if t.cons[fs].fullscreen_mode == FullscreenMode::CF_OUTPUT && con_ws != con_get_workspace(t, fs) {
        return true;
    }

    /* Allow it only if the container to be focused is contained within the
     * current fullscreen container. */
    con_has_parent(t, con, fs)
}

/// C: src/con.c:2361 `con_has_urgent_child`
///
/// Checks if the given container has an urgent child.
pub fn con_has_urgent_child(t: &Tree, con: ConId) -> bool {
    if con_is_leaf(t, con) {
        return t.cons[con].urgent;
    }

    /* We are not interested in floating windows since they can only be
     * attached to a workspace → nodes_head instead of focus_head */
    let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in nodes {
        if con_has_urgent_child(t, child) {
            return true;
        }
    }

    false
}

/// C: src/con.c:2385 `con_update_parents_urgency`
///
/// Make all parent containers urgent if con is urgent or clear the urgent
/// flag of all parent containers if there are no more urgent children left.
pub fn con_update_parents_urgency(t: &mut Tree, con: ConId) {
    let mut parent = t.cons[con].parent;

    /* Urgency hints should not be set on any container higher up in the
     * hierarchy than the workspace level. Unfortunately, since the content
     * container has type == CT_CON, that's not easy to verify in the loop
     * below, so we need another condition to catch that case: */
    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        return;
    }

    let new_urgency_value = t.cons[con].urgent;
    while let Some(p) = parent {
        if t.cons[p].r#type == ConType::CT_WORKSPACE || t.cons[p].r#type == ConType::CT_DOCKAREA {
            break;
        }
        if new_urgency_value {
            t.cons[p].urgent = true;
        } else {
            /* We can only reset the urgency when the parent
             * has no other urgent children */
            if !con_has_urgent_child(t, p) {
                t.cons[p].urgent = false;
            }
        }
        parent = t.cons[p].parent;
    }
}

/// C: sys/time.h `gettimeofday` (only the urgency timestamp uses it).
fn gettimeofday() -> crate::data::timeval {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    crate::data::timeval {
        tv_sec: now.as_secs() as i64,
        tv_usec: now.subsec_micros() as i64,
    }
}

/// C: src/con.c:2410 `con_set_urgency`
///
/// Set urgency flag to the container, all the parent containers and the
/// workspace.
pub fn con_set_urgency(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, urgent: bool) {
    if urgent && t.focused == Some(con) {
        /* Ignoring urgency flag for current client */
        return;
    }

    let old_urgent = t.cons[con].urgent;

    if t.cons[con].urgency_timer.is_none() {
        t.cons[con].urgent = urgent;
    }

    if let Some(window) = t.cons[con].window {
        if t.cons[con].urgent {
            /* C: src/con.c:2432 gettimeofday(&con->window->urgent, NULL).
             * The timestamp orders the 'urgent=latest' / 'urgent=oldest'
             * criteria (src/match.c:139), so it has to be a real time —
             * a fixed non-zero value would make every urgent window compare
             * equal and match_matches_window() would reject them all. */
            t.windows[window].urgent = gettimeofday();
        } else {
            t.windows[window].urgent.tv_sec = 0;
            t.windows[window].urgent.tv_usec = 0;
        }
    }

    con_update_parents_urgency(t, con);

    /* Set the urgency flag on the workspace, if a workspace could be found
     * (for dock clients, that is not the case). */
    if let Some(ws) = con_get_workspace(t, con) {
        workspace_update_urgent_flag(t, b, ws);
    }

    if t.cons[con].urgent != old_urgent {
        b.ipc_send_window_event(t, "urgent", con);
    }
}

/// C: src/con.c:2449 `con_get_tree_representation`
///
/// Create a string representing the subtree under con.
pub fn con_get_tree_representation(t: &Tree, con: ConId) -> String {
    /* end of recursion */
    if con_is_leaf(t, con) {
        if t.cons[con].window.is_none() {
            return "nowin".to_string();
        }

        let window = t.cons[con].window.expect("checked above");
        match &t.windows[window].class_instance {
            None => "noinstance".to_string(),
            Some(ci) => ci.clone(),
        }
    } else {
        /* 1) add the Layout type to buf */
        let mut buf = match t.cons[con].layout {
            Layout::L_DEFAULT => "D[".to_string(),
            Layout::L_SPLITV => "V[".to_string(),
            Layout::L_SPLITH => "H[".to_string(),
            Layout::L_TABBED => "T[".to_string(),
            Layout::L_STACKED => "S[".to_string(),
            _ => panic!("con_get_tree_representation: code not updated to account for new layout type"),
        };

        /* 2) append representation of children */
        let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
        for child in nodes {
            let child_txt = con_get_tree_representation(t, child);
            let first = t.cons[con].nodes_head.front().copied() == Some(child);
            let sep = if first { "" } else { " " };
            buf = format!("{}{}{}", buf, sep, child_txt);
        }

        /* 3) close the brackets */
        format!("{}]", buf)
    }
}

/// C: src/con.c:2505 `con_parse_title_format`
///
/// TODO: the real function escapes pango markup and consults the font
/// config; the placeholder substitution is ported, markup escaping is the
/// X/cairo layer's job.
pub fn con_parse_title_format(t: &Tree, con: ConId) -> String {
    let title_format = t.cons[con].title_format.clone().expect("con_parse_title_format: no title_format");

    let (title, class, instance, machine) = match t.cons[con].window {
        None => (
            con_get_tree_representation(t, con),
            "i3-frame".to_string(),
            "i3-frame".to_string(),
            String::new(),
        ),
        Some(window) => (
            t.windows[window].name.clone().unwrap_or_default(),
            t.windows[window].class_class.clone().unwrap_or_default(),
            t.windows[window].class_instance.clone().unwrap_or_default(),
            t.windows[window].machine.clone().unwrap_or_default(),
        ),
    };

    title_format
        .replace("%title", &title)
        .replace("%class", &class)
        .replace("%instance", &instance)
        .replace("%machine", &machine)
}

/// C: src/con.c:2581 `con_swap`
///
/// Swaps the two containers.
pub fn con_swap(t: &mut Tree, b: &mut dyn ConBackend, first: ConId, second: ConId) -> bool {
    if t.cons[first].r#type != ConType::CT_CON {
        return false;
    }

    if t.cons[second].r#type != ConType::CT_CON {
        return false;
    }

    if first == second {
        return false;
    }

    if con_has_parent(t, Some(first), second) || con_has_parent(t, Some(second), first) {
        return false;
    }

    let ws1 = con_get_workspace(t, first);
    let ws2 = con_get_workspace(t, second);
    let focused = t.focused;
    let mut restore_focus: Option<ConId> = None;
    if ws1 == ws2 && ws1 == focused.and_then(|f| con_get_workspace(t, f)) {
        /* Preserve focus in the current workspace. */
        restore_focus = focused;
    } else if focused == Some(first) || focused.map(|f| con_has_parent(t, Some(f), first)).unwrap_or(false) {
        restore_focus = Some(second);
    } else if focused == Some(second) || focused.map(|f| con_has_parent(t, Some(f), second)).unwrap_or(false) {
        restore_focus = Some(first);
    }

    /* SWAP_CONS_IN_TREE(nodes_head, nodes); */
    {
        let head1 = t.cons[first].parent.expect("con_swap: first has no parent");
        let head2 = t.cons[second].parent.expect("con_swap: second has no parent");
        t.swap_in_list(head1, head2, first, second, true);
    }
    /* SWAP_CONS_IN_TREE(focus_head, focused); */
    {
        let head1 = t.cons[first].parent.expect("con_swap: first has no parent");
        let head2 = t.cons[second].parent.expect("con_swap: second has no parent");
        t.swap_in_list(head1, head2, first, second, false);
    }
    /* SWAP(first->parent, second->parent, Con *); */
    {
        let p1 = t.cons[first].parent;
        let p2 = t.cons[second].parent;
        t.cons[first].parent = p2;
        t.cons[second].parent = p1;
    }

    /* Floating nodes are children of CT_FLOATING_CONs, they are listed in
     * nodes_head and focus_head like all other containers. Thus, we don't need
     * to do anything special other than swapping the floating status and the
     * relevant rects. */
    let first_floating = t.cons[first].floating;
    let second_floating = t.cons[second].floating;
    t.cons[first].floating = second_floating;
    t.cons[second].floating = first_floating;
    let first_rect = t.cons[first].rect;
    let second_rect = t.cons[second].rect;
    t.cons[first].rect = second_rect;
    t.cons[second].rect = first_rect;
    let first_window_rect = t.cons[first].window_rect;
    let second_window_rect = t.cons[second].window_rect;
    t.cons[first].window_rect = second_window_rect;
    t.cons[second].window_rect = first_window_rect;

    /* We need to copy each other's percentages to ensure that the geometry
     * doesn't change during the swap. */
    let first_percent = t.cons[first].percent;
    let second_percent = t.cons[second].percent;
    t.cons[first].percent = second_percent;
    t.cons[second].percent = first_percent;

    if let Some(restore_focus) = restore_focus {
        con_focus(t, b, restore_focus);
    }

    /* Update new parents' & workspaces' urgency. */
    let first_urgent = t.cons[first].urgent;
    let second_urgent = t.cons[second].urgent;
    con_set_urgency(t, b, first, first_urgent);
    con_set_urgency(t, b, second, second_urgent);

    /* Exchange fullscreen modes, can't use SWAP because we need to call the
     * correct functions. */
    let second_fullscreen_mode = t.cons[second].fullscreen_mode;
    if t.cons[first].fullscreen_mode == FullscreenMode::CF_NONE {
        con_disable_fullscreen(t, b, second);
    } else {
        con_enable_fullscreen(t, b, second, t.cons[first].fullscreen_mode);
    }
    if second_fullscreen_mode == FullscreenMode::CF_NONE {
        con_disable_fullscreen(t, b, first);
    } else {
        con_enable_fullscreen(t, b, first, second_fullscreen_mode);
    }

    /* We don't actually need this since percentages-wise we haven't changed
     * anything, but we'll better be safe than sorry and just make sure as we'd
     * otherwise crash i3. */
    let first_parent = t.cons[first].parent.expect("con_swap: first has no parent");
    let second_parent = t.cons[second].parent.expect("con_swap: second has no parent");
    con_fix_percent(t, first_parent);
    con_fix_percent(t, second_parent);

    t.cons[first].deco_render_params = None;
    t.cons[second].deco_render_params = None;
    con_force_split_parents_redraw(t, first);
    con_force_split_parents_redraw(t, second);

    true
}

/// C: src/con.c:2716 `con_rect_size_in_orientation`
///
/// Returns container's rect size depending on its orientation.
/// i.e. its width when horizontal, its height when vertical.
pub fn con_rect_size_in_orientation(t: &Tree, con: ConId) -> u32 {
    if con_orientation(t, con) == Orientation::HORIZ {
        t.cons[con].rect.width
    } else {
        t.cons[con].rect.height
    }
}

/// C: src/con.c:2729 `con_merge_into`
///
/// Merges container specific data that should move with the window (e.g.
/// marks, title format, and the window itself) into another container, and
/// closes the old container.
pub fn con_merge_into(t: &mut Tree, b: &mut dyn ConBackend, old: ConId, new: ConId) {
    t.cons[new].window = t.cons[old].window;
    t.cons[old].window = None;

    if t.cons[old].title_format.is_some() {
        t.cons[new].title_format = t.cons[old].title_format.take();
    }

    if t.cons[old].sticky_group.is_some() {
        t.cons[new].sticky_group = t.cons[old].sticky_group.take();
    }

    t.cons[new].sticky = t.cons[old].sticky;

    let old_urgent = t.cons[old].urgent;
    con_set_urgency(t, b, new, old_urgent);

    let marks: Vec<crate::data::mark_t> = t.cons[old].marks_head.drain(..).collect();
    for mark in marks {
        t.cons[new].marks_head.push_back(mark);
        b.ipc_send_window_event(t, "mark", new);
    }
    t.cons[new].mark_changed = !t.cons[old].marks_head.is_empty();
    t.cons[old].marks_head.clear();

    tree_close_internal(t, b, old, KillWindow::DONT_KILL_WINDOW, false);
}

/// C: src/con.c:2752 `con_inside_stacked_or_tabbed`
///
/// Returns true if the container is within any stacked/tabbed split
/// container.
pub fn con_inside_stacked_or_tabbed(t: &Tree, con: ConId) -> bool {
    let parent = match t.cons[con].parent {
        None => return false,
        Some(p) => p,
    };
    if t.cons[parent].layout == Layout::L_STACKED || t.cons[parent].layout == Layout::L_TABBED {
        return true;
    }
    con_inside_stacked_or_tabbed(t, parent)
}

// ConId re-export: tree.rs and friends import ConId from this module
// (C: include/data.h `Con *`).
pub use crate::data::ConId;
