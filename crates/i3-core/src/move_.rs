//! 1:1 port of src/move.c — moving containers into some direction.
//!
//! C: src/move.c

use crate::backend::ConBackend;
use crate::con::{
    con_detach, con_force_split_parents_redraw, con_fullscreen_permits_focusing,
    con_get_workspace, con_is_floating, con_is_leaf, con_num_children, con_orientation,
    con_parent_with_orientation, con_descend_direction, con_focus, ConId,
};
use crate::data::{ConType, Direction, Layout, Position};
use crate::floating::floating_disable;
use crate::output::{get_output_for_con, get_output_next, output_get_content, OutputId};
use crate::data::OutputCloseFar;
use crate::tree::{tree_flatten, Tree};
use crate::util::orientation_from_direction;
use crate::workspace::{workspace_is_visible, ws_force_orientation, workspace_show, workspace_attach_to};

/// C: src/move.c:20 `lowest_common_ancestor` (static)
///
/// Returns the lowest container in the tree that has both a and b as
/// descendants.
fn lowest_common_ancestor(t: &Tree, a: ConId, b: ConId) -> Option<ConId> {
    let mut parent_a = Some(a);
    while let Some(pa) = parent_a {
        let mut parent_b = Some(b);
        while let Some(pb) = parent_b {
            if pa == pb {
                return Some(pa);
            }
            parent_b = t.cons[pb].parent;
        }
        parent_a = t.cons[pa].parent;
    }
    panic!("lowest_common_ancestor: no common ancestor");
}

/// C: src/move.c:36 `child_containing_con_recursively` (static)
///
/// Returns the direct child of ancestor that contains con.
fn child_containing_con_recursively(t: &Tree, ancestor: ConId, con: ConId) -> Option<ConId> {
    let mut child = Some(con);
    while let Some(c) = child {
        if t.cons[c].parent == Some(ancestor) {
            break;
        }
        child = t.cons[c].parent;
        if child.is_some() {
            let cc = child.expect("checked");
            if t.cons[cc].parent.is_none() {
                panic!("child_containing_con_recursively: broken hierarchy");
            }
        }
    }
    child
}

/// C: src/move.c:50 `is_focused_descendant` (static)
///
/// Returns true if the given container is the focused descendant of
/// ancestor, recursively.
fn is_focused_descendant(t: &Tree, con: ConId, ancestor: ConId) -> bool {
    let mut current = con;
    while current != ancestor {
        let parent = t.cons[current].parent.expect("is_focused_descendant: no parent");
        if t.cons[parent].focus_head.front() != Some(&current) {
            return false;
        }
        current = parent;
        if t.cons[current].parent.is_none() && current != ancestor {
            panic!("is_focused_descendant: broken hierarchy");
        }
    }
    true
}

/// C: src/move.c:66 `insert_con_into`
///
/// This function detaches 'con' from its parent and inserts it either before
/// or after 'target'.
pub fn insert_con_into(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, target: ConId, position: Position) {
    let parent = t.cons[target].parent.expect("insert_con_into: target has no parent");
    /* We need to preserve the old con->parent. While it might still be used to
     * insert the entry before/after it, we call the on_remove_child callback
     * afterwards which might then close the con if it is empty. */
    let old_parent = t.cons[con].parent;

    /* We compare the focus order of the children of the lowest common
     * ancestor. If con or its ancestor is before target's ancestor then con
     * should be placed before the target in the focus stack. */
    let lca = match lowest_common_ancestor(t, con, parent) {
        None => return,
        Some(lca) => lca,
    };
    if lca == con {
        /* Container is being inserted into one of its descendants. */
        return;
    }

    let con_ancestor = child_containing_con_recursively(t, lca, con).expect("insert_con_into: no con ancestor");
    let target_ancestor = child_containing_con_recursively(t, lca, target).expect("insert_con_into: no target ancestor");
    let moves_focus_from_ancestor = is_focused_descendant(t, con, con_ancestor);
    let focus_before;

    /* Determine if con is going to be placed before or after target in the
     * parent's focus stack. */
    if con_ancestor == target_ancestor {
        /* Happens when the target is con's old parent. Eg with layout
         * V [ A H [ B C ] ], if we move C up. Target will be H. */
        focus_before = moves_focus_from_ancestor;
    } else {
        /* Look at the focus stack order of the children of the lowest common
         * ancestor. */
        let mut current: Option<ConId> = None;
        for &cur in &t.cons[lca].focus_head {
            if cur == con_ancestor || cur == target_ancestor {
                current = Some(cur);
                break;
            }
        }
        focus_before = current == Some(con_ancestor);
    }

    /* If con is the focused container in our old ancestor we place the new
     * ancestor before the old ancestor in the focus stack. */
    if moves_focus_from_ancestor && focus_before {
        let place = t.focus_prev(con_ancestor);
        t.focus_remove(lca, target_ancestor);
        match place {
            Some(place) => t.focus_insert_after(place, target_ancestor),
            None => t.focus_insert_head(lca, target_ancestor),
        }
    }

    con_detach(t, con);
    let con_parent = t.cons[con].parent;
    if let Some(con_parent) = con_parent {
        crate::con::con_fix_percent(t, con_parent);
    }

    let mut con = con;
    /* When moving to a workspace, we respect the user's configured
     * workspace_layout */
    if t.cons[parent].r#type == ConType::CT_WORKSPACE {
        let split = workspace_attach_to(t, b, parent);
        if split != parent {
            /* Got a new split con, using that one instead */
            t.cons[con].parent = Some(split);
            crate::con::con_attach(t, b, con, split, false);
            t.cons[con].percent = 0.0;
            crate::con::con_fix_percent(t, split);
            con = split;
            con_detach(t, con);
        }
    }

    t.cons[con].parent = Some(parent);

    if parent == lca {
        if focus_before {
            /* Example layout: H [ A B* ], we move A up/down. 'target' will be H. */
            t.focus_insert_before(target, con);
        } else {
            /* Example layout: H [ A B* ], we move A up/down. 'target' will be H. */
            t.focus_insert_after(target, con);
        }
    } else {
        if focus_before {
            /* Example layout: V [ H [ A B ] C* ], we move C up. 'target' will be A. */
            t.focus_insert_head(parent, con);
        } else {
            /* Example layout: V [ H [ A* B ] C ], we move C up. 'target' will be A. */
            t.focus_insert_tail(parent, con);
        }
    }

    if position == Position::BEFORE {
        t.nodes_insert_before(target, con);
    } else if position == Position::AFTER {
        t.nodes_insert_after(target, con);
    }

    /* Pretend the con was just opened with regards to size percent values.
     * Since the con is moved to a completely different con, the old value
     * does not make sense anyways. */
    t.cons[con].percent = 0.0;
    crate::con::con_fix_percent(t, parent);

    if let Some(old_parent) = old_parent {
        crate::con::con_on_remove_child(t, b, old_parent);
    }
}

/// C: src/move.c:206 `attach_to_workspace` (static)
///
/// This function detaches 'con' from its parent and puts it in the given
/// workspace. Position is determined by the direction of movement into the
/// workspace container.
fn attach_to_workspace(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, ws: ConId, direction: Direction) {
    con_detach(t, con);
    let old_parent = t.cons[con].parent;
    t.cons[con].parent = Some(ws);

    if direction == Direction::D_RIGHT || direction == Direction::D_DOWN {
        t.cons[ws].nodes_head.push_front(con);
    } else {
        t.cons[ws].nodes_head.push_back(con);
    }
    t.cons[ws].focus_head.push_back(con);

    /* Pretend the con was just opened with regards to size percent values.
     * Since the con is moved to a completely different con, the old value
     * does not make sense anyways. */
    t.cons[con].percent = 0.0;
    crate::con::con_fix_percent(t, ws);

    if let Some(old_parent) = old_parent {
        crate::con::con_fix_percent(t, old_parent);
        crate::con::con_on_remove_child(t, b, old_parent);
    }
}

/// C: src/move.c:228 `move_to_output_directed` (static)
///
/// Moves the given container to the closest output in the given direction if
/// such an output exists.
fn move_to_output_directed(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, direction: Direction) {
    let current_output = get_output_for_con(t, con);
    let current_output = match current_output {
        None => return,
        Some(o) => o,
    };
    let output = get_output_next(t, direction, current_output, OutputCloseFar::CLOSEST_OUTPUT);

    let output = match output {
        None => return,
        Some(o) => o,
    };

    let output_con = t.outputs[output].con;
    let content = match output_con.and_then(|c| output_get_content(t, c)) {
        None => return,
        Some(c) => c,
    };
    let mut ws: Option<ConId> = None;
    for &child in &t.cons[content].nodes_head {
        if workspace_is_visible(t, child) {
            ws = Some(child);
            break;
        }
    }

    let ws = match ws {
        None => return,
        Some(ws) => ws,
    };

    let old_ws = con_get_workspace(t, con);
    let moves_focus = t.focused == Some(con);
    attach_to_workspace(t, b, con, ws, direction);
    if moves_focus {
        /* workspace_show will not correctly update the active workspace because
         * the focused container, con, is now a child of ws. To work around
         * this and still produce the correct workspace focus events (see
         * 517-regress-move-direction-ipc.t) we need to temporarily set focused
         * to the old workspace.
         *
         * The following happen:
         * 1. Focus con to push it on the top of the focus stack in its new
         * workspace
         * 2. Set focused to the old workspace to force workspace_show to
         * execute
         * 3. workspace_show will descend focus and target our con for
         * focusing. This also ensures that the mouse warps correctly.
         * See: #3518. */
        con_focus(t, b, con);
        t.focused = old_ws;
        workspace_show(t, b, ws);
        con_focus(t, b, con);
    }

    /* force re-painting the indicators */
    t.cons[con].deco_render_params = None;

    b.ipc_send_window_event(t, "move", con);
    let croot = t.croot.expect("move_to_output_directed: no croot");
    tree_flatten(t, b, croot);
    b.ewmh_update_wm_desktop(t);
}

/// C: src/move.c:269 `tree_move`
///
/// Moves the given container in the given direction.
pub fn tree_move(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, direction: Direction) {
    let position;
    let target: ConId;

    /* 1: get the first parent with the same orientation */

    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        /* Not moving workspace */
        return;
    }

    if t.cons[con].fullscreen_mode == crate::data::FullscreenMode::CF_GLOBAL {
        /* Not moving fullscreen global container */
        return;
    }

    if t.cons[con].fullscreen_mode == crate::data::FullscreenMode::CF_OUTPUT
        || (t.cons[t.cons[con].parent.expect("tree_move: no parent")].r#type == ConType::CT_WORKSPACE
            && con_num_children(t, t.cons[con].parent.expect("tree_move: no parent")) == 1)
    {
        /* This is the only con on this workspace */
        move_to_output_directed(t, b, con, direction);
        return;
    }

    let o = orientation_from_direction(direction);

    let mut same_orientation = con_parent_with_orientation(t, con, o);
    /* The do {} while is used to 'restart' at this point with a different
     * same_orientation, see the very last lines before the end of this block */
    loop {
        /* There is no parent container with the same orientation */
        if same_orientation.is_none() {
            if con_is_floating(t, con) {
                /* this is a floating con, we just disable floating */
                floating_disable(t, b, con);
                crate::assignments::run_assignments(t, b, t.cons[con].window);
                return;
            }
            if crate::con::con_inside_floating(t, con).is_some() {
                /* 'con' should be moved out of a floating container */
                let ws = con_get_workspace(t, con).expect("tree_move: no workspace");
                attach_to_workspace(t, b, con, ws, direction);
                /* goto end */
                t.cons[con].deco_render_params = None;
                b.ipc_send_window_event(t, "move", con);
                let croot = t.croot.expect("tree_move: no croot");
                tree_flatten(t, b, croot);
                b.ewmh_update_wm_desktop(t);
                return;
            }
            /* Force-changing orientation */
            let ws = con_get_workspace(t, con).expect("tree_move: no workspace");
            ws_force_orientation(t, b, ws, o);
            same_orientation = con_parent_with_orientation(t, con, o);
        }

        /* easy case: the move is within this container */
        if same_orientation == t.cons[con].parent {
            let swap = if direction == Direction::D_LEFT || direction == Direction::D_UP {
                t.nodes_prev(con)
            } else {
                t.nodes_next(con)
            };
            if let Some(swap) = swap {
                if !con_is_leaf(t, swap) {
                    /* Moving into our bordering branch */
                    target = con_descend_direction(t, swap, direction);
                    position = if con_orientation(t, t.cons[target].parent.expect("tree_move: target has no parent")) != o
                        || direction == Direction::D_UP
                        || direction == Direction::D_LEFT
                    {
                        Position::AFTER
                    } else {
                        Position::BEFORE
                    };
                    insert_con_into(t, b, con, target, position);
                    /* goto end */
                    t.cons[con].deco_render_params = None;
                    b.ipc_send_window_event(t, "move", con);
                    let croot = t.croot.expect("tree_move: no croot");
                    tree_flatten(t, b, croot);
                    b.ewmh_update_wm_desktop(t);
                    return;
                }

                /* Swapping with sibling. */
                let swap_parent = t.cons[swap].parent.expect("tree_move: swap has no parent");
                if direction == Direction::D_LEFT || direction == Direction::D_UP {
                    /* TAILQ_SWAP(swap, con, …) */
                    let (a, bb) = (swap, con);
                    let ia = t.cons[swap_parent].nodes_head.iter().position(|&c| c == a).expect("tree_move: swap missing");
                    let ib = t.cons[swap_parent].nodes_head.iter().position(|&c| c == bb).expect("tree_move: con missing");
                    t.cons[swap_parent].nodes_head.swap(ia, ib);
                } else {
                    /* TAILQ_SWAP(con, swap, …) */
                    let (a, bb) = (con, swap);
                    let ia = t.cons[swap_parent].nodes_head.iter().position(|&c| c == a).expect("tree_move: con missing");
                    let ib = t.cons[swap_parent].nodes_head.iter().position(|&c| c == bb).expect("tree_move: swap missing");
                    t.cons[swap_parent].nodes_head.swap(ia, ib);
                }

                /* redraw parents to ensure all parent split container titles
                 * are updated correctly */
                con_force_split_parents_redraw(t, con);

                b.ipc_send_window_event(t, "move", con);
                return;
            }

            if t.cons[con].parent == con_get_workspace(t, con) {
                /* If we couldn't find a place to move it on this workspace, try
                 * to move it to a workspace on a different output */
                move_to_output_directed(t, b, con, direction);
                return;
            }

            /* If there was no con with which we could swap the current one,
             * search again, but starting one level higher. */
            let parent = t.cons[con].parent.expect("tree_move: no parent");
            same_orientation = con_parent_with_orientation(t, parent, o);
        }

        /* C: src/move.c:351 `} while (same_orientation == NULL);` — the loop is
         * only repeated while no parent with the same orientation has been
         * found (i.e. after ws_force_orientation() failed to create one). As
         * soon as we found one, the code below has to move `con` into
         * `same_orientation`, which happens *outside* the loop. */
        if same_orientation.is_some() {
            break;
        }
    }

    let same_orientation = match same_orientation {
        None => return,
        Some(so) => so,
    };

    /* this time, we have to move to another container */
    /* This is the container *above* 'con' (an ancestor of con) which is inside
     * 'same_orientation' */
    let mut above = con;
    while t.cons[above].parent != Some(same_orientation) {
        above = t.cons[above].parent.expect("tree_move: broken hierarchy");
    }

    /* Enforce the fullscreen focus restrictions. */
    let above_parent = t.cons[above].parent.expect("tree_move: above has no parent");
    if !con_fullscreen_permits_focusing(t, Some(above_parent)) {
        /* Cannot move out of fullscreen container */
        return;
    }

    let next = if direction == Direction::D_UP || direction == Direction::D_LEFT {
        t.nodes_prev(above)
    } else {
        t.nodes_next(above)
    };

    match next {
        Some(next) if !con_is_leaf(t, next) => {
            /* Moving into the bordering branch of our adjacent container */
            target = con_descend_direction(t, next, direction);
            position = if con_orientation(t, t.cons[target].parent.expect("tree_move: target has no parent")) != o
                || direction == Direction::D_UP
                || direction == Direction::D_LEFT
            {
                Position::AFTER
            } else {
                Position::BEFORE
            };
            insert_con_into(t, b, con, target, position);
        }
        None if {
            let parent = t.cons[con].parent.expect("tree_move: no parent");
            let grandparent = t.cons[parent].parent;
            grandparent.map(|g| t.cons[g].r#type == ConType::CT_WORKSPACE).unwrap_or(false)
                && t.cons[parent].layout != Layout::L_DEFAULT
                && con_num_children(t, parent) == 1
        } =>
        {
            /* Con is the lone child of a non-default layout container at the
             * edge of the workspace. Treat it as though the workspace is its
             * parent and move it to the next output. */
            move_to_output_directed(t, b, con, direction);
        }
        _ => {
            /* Moving into container above */
            position = if direction == Direction::D_UP || direction == Direction::D_LEFT { Position::BEFORE } else { Position::AFTER };
            insert_con_into(t, b, con, above, position);
        }
    }

    /* end: force re-painting the indicators */
    t.cons[con].deco_render_params = None;

    b.ipc_send_window_event(t, "move", con);
    let croot = t.croot.expect("tree_move: no croot");
    tree_flatten(t, b, croot);
    b.ewmh_update_wm_desktop(t);
}

// OutputId import anchor (C: move.c uses Output*).
#[allow(unused)]
fn _output_id_anchor(_o: OutputId) {}
