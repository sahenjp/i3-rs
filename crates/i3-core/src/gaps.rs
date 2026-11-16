//! 1:1 port of src/gaps.c — gaps logic: whether to display gaps at all,
//! and how big they should be.
//!
//! C: src/gaps.c

use crate::con::{con_get_workspace, con_inside_floating, con_inside_stacked_or_tabbed, con_is_leaf, con_num_children, con_num_visible_children, con_orientation, ConId};
use crate::data::{ConType, Direction, Layout, SmartGaps, GapsMask, GAPS_BOTTOM, GAPS_INNER, GAPS_LEFT, GAPS_RIGHT, GAPS_TOP};
use crate::tree::Tree;
use crate::util::{name_is_digits, ws_name_to_number};

/// C: src/gaps.c:16 `calculate_effective_gaps`
///
/// Calculates the effective gap sizes for a container.
pub fn calculate_effective_gaps(t: &Tree, con: ConId) -> crate::data::gaps_t {
    let workspace = match con_get_workspace(t, con) {
        None => return crate::data::gaps_t::default(),
        Some(ws) => ws,
    };

    let one_child = con_num_visible_children(t, Some(workspace)) <= 1
        || (con_num_children(t, workspace) == 1
            && {
                let first = t.cons[workspace].nodes_head[0];
                t.cons[first].layout == Layout::L_TABBED || t.cons[first].layout == Layout::L_STACKED
            });

    if t.config.smart_gaps == SmartGaps::SMART_GAPS_ON && one_child {
        return crate::data::gaps_t::default();
    }

    let mut gaps = crate::data::gaps_t {
        inner: workspace_gaps_inner(t, workspace) + t.config.gaps.inner,
        top: 0,
        right: 0,
        bottom: 0,
        left: 0,
    };

    if t.config.smart_gaps != SmartGaps::SMART_GAPS_INVERSE_OUTER || one_child {
        gaps.top = workspace_gaps_top(t, workspace) + t.config.gaps.top;
        gaps.right = workspace_gaps_right(t, workspace) + t.config.gaps.right;
        gaps.bottom = workspace_gaps_bottom(t, workspace) + t.config.gaps.bottom;
        gaps.left = workspace_gaps_left(t, workspace) + t.config.gaps.left;
    }

    gaps
}

// helpers mirroring C's direct struct member reads (workspace->gaps.X)
fn workspace_gaps_inner(t: &Tree, ws: ConId) -> i32 {
    t.cons[ws].gaps.inner
}
fn workspace_gaps_top(t: &Tree, ws: ConId) -> i32 {
    t.cons[ws].gaps.top
}
fn workspace_gaps_right(t: &Tree, ws: ConId) -> i32 {
    t.cons[ws].gaps.right
}
fn workspace_gaps_bottom(t: &Tree, ws: ConId) -> i32 {
    t.cons[ws].gaps.bottom
}
fn workspace_gaps_left(t: &Tree, ws: ConId) -> i32 {
    t.cons[ws].gaps.left
}

/// C: src/gaps.c:54 `gaps_should_inset_con`
///
/// Decides whether the container should be inset.
pub fn gaps_should_inset_con(t: &Tree, con: ConId, children: i32) -> bool {
    /* No parent? None of the conditionals below can be true. */
    let parent = match t.cons[con].parent {
        None => return false,
        Some(p) => p,
    };

    /* Floating split containers should never have gaps inside them. */
    if con_inside_floating(t, con).is_some() {
        return false;
    }

    let _ = children;
    let leaf_or_stacked_tabbed = con_is_leaf(t, con)
        || (t.cons[con].layout == Layout::L_STACKED || t.cons[con].layout == Layout::L_TABBED);

    /* Inset direct children of the workspace that are leaf containers or
       stacked/tabbed containers. */
    if leaf_or_stacked_tabbed && t.cons[parent].r#type == ConType::CT_WORKSPACE {
        return true;
    }

    /* Inset direct children of vertical or horizontal split containers at any
       depth in the tree. Do not inset as soon as any parent is a stacked or
       tabbed container, to avoid double insets. */
    if leaf_or_stacked_tabbed
        && !con_inside_stacked_or_tabbed(t, con)
        && t.cons[parent].r#type == ConType::CT_CON
        && (t.cons[parent].layout == Layout::L_SPLITH || t.cons[parent].layout == Layout::L_SPLITV)
    {
        return true;
    }

    false
}

/// C: src/resize.c:72 `resize_find_tiling_participants`
///
/// Ported here because gaps.c's gaps_has_adjacent_container() is the only
/// tree-layer caller.
pub fn resize_find_tiling_participants(t: &Tree, current: &mut ConId, other: &mut Option<ConId>, direction: Direction, both_sides: bool) -> bool {
    let mut first = *current;
    let mut second: Option<ConId> = None;

    /* Go up in the tree and search for a container to resize */
    let search_orientation = crate::util::orientation_from_direction(direction);
    let dir_backwards = direction == Direction::D_UP || direction == Direction::D_LEFT;
    while t.cons[first].r#type != ConType::CT_WORKSPACE
        && t.cons[first].r#type != ConType::CT_FLOATING_CON
        && second.is_none()
    {
        let parent = match t.cons[first].parent {
            None => break,
            Some(p) => p,
        };
        /* get the appropriate first container with the matching
         * orientation (skip stacked/tabbed cons) */
        if con_orientation(t, parent) != search_orientation
            || t.cons[parent].layout == Layout::L_STACKED
            || t.cons[parent].layout == Layout::L_TABBED
        {
            first = parent;
            continue;
        }

        /* get the counterpart for this resizement */
        if dir_backwards {
            second = t.nodes_prev(first);
            if second.is_none() && both_sides {
                second = t.nodes_next(first);
            }
        } else {
            second = t.nodes_next(first);
            if second.is_none() && both_sides {
                second = t.nodes_prev(first);
            }
        }

        if second.is_none() {
            /* No second container in this direction found, trying to look
             * further up in the tree… */
            first = parent;
        }
    }

    *current = first;
    *other = second;
    if second.is_none() {
        return false;
    }

    true
}

/// C: src/gaps.c:82 `gaps_has_adjacent_container`
///
/// Returns whether the given container has an adjacent container in the
/// specified direction. In other words, this returns true if and only if
/// the container is not touching the edge of the screen in that direction.
pub fn gaps_has_adjacent_container(t: &Tree, con: ConId, direction: Direction) -> bool {
    let workspace = con_get_workspace(t, con);
    let mut fullscreen = match workspace {
        None => None,
        Some(ws) => crate::con::con_get_fullscreen_con(t, ws, crate::data::FullscreenMode::CF_GLOBAL),
    };
    if fullscreen.is_none() {
        fullscreen = match workspace {
            None => None,
            Some(ws) => crate::con::con_get_fullscreen_con(t, ws, crate::data::FullscreenMode::CF_OUTPUT),
        };
    }

    /* If this container is fullscreen by itself, there's no adjacent container. */
    if fullscreen == Some(con) {
        return false;
    }

    let mut first = con;
    let mut second: Option<ConId> = None;
    let found_neighbor = resize_find_tiling_participants(t, &mut first, &mut second, direction, false);
    if !found_neighbor {
        return false;
    }

    /* If we have an adjacent container and nothing is fullscreen, we consider it. */
    let fullscreen = match fullscreen {
        None => return true,
        Some(f) => f,
    };

    /* For fullscreen containers, only consider the adjacent container if it is also fullscreen. */
    crate::con::con_has_parent(t, Some(con), fullscreen)
        && crate::con::con_has_parent(t, second, fullscreen)
}

/// C: src/gaps.c:118 `gaps_for_workspace`
///
/// Returns the configured gaps for this workspace based on the workspace
/// name, number, and configured workspace gap assignments.
pub fn gaps_for_workspace(t: &Tree, ws: ConId) -> crate::data::gaps_t {
    let mut gaps = crate::data::gaps_t::default();
    let mut mask = GapsMask(0);
    let ws_name = t.cons[ws].name.clone().unwrap_or_default();
    let ws_num = t.cons[ws].num;
    for assignment in &t.ws_assignments {
        if assignment.name == ws_name {
            gaps = assignment.gaps;
            mask = assignment.gaps_mask;
            break;
        } else if ws_num != -1 && name_is_digits(&assignment.name) && ws_name_to_number(&assignment.name) == ws_num {
            gaps = assignment.gaps;
            mask = assignment.gaps_mask;
        }
    }
    if mask.0 == 0 {
        return gaps;
    }

    if (mask & GAPS_INNER).0 != 0 {
        gaps.inner -= t.config.gaps.inner;
    }
    if (mask & GAPS_TOP).0 != 0 {
        gaps.top -= t.config.gaps.top;
    }
    if (mask & GAPS_RIGHT).0 != 0 {
        gaps.right -= t.config.gaps.right;
    }
    if (mask & GAPS_BOTTOM).0 != 0 {
        gaps.bottom -= t.config.gaps.bottom;
    }
    if (mask & GAPS_LEFT).0 != 0 {
        gaps.left -= t.config.gaps.left;
    }

    gaps
}

/// C: src/gaps.c:150 `gaps_reapply_workspace_assignments`
///
/// Re-applies all workspace gap assignments to existing workspaces after
/// reloading the configuration file.
pub fn gaps_reapply_workspace_assignments(t: &mut Tree) {
    let croot = match t.croot {
        None => return,
        Some(c) => c,
    };
    let outputs: Vec<ConId> = t.cons[croot].nodes_head.iter().copied().collect();
    for output in outputs {
        let content = match crate::output::output_get_content(t, output) {
            None => continue,
            Some(c) => c,
        };
        let workspaces: Vec<ConId> = t.cons[content].nodes_head.iter().copied().collect();
        for workspace in workspaces {
            t.cons[workspace].gaps = gaps_for_workspace(t, workspace);
        }
    }
}
