//! 1:1 port of src/tiling_drag.c — Reposition tiled windows by dragging.
//!
//! C: src/tiling_drag.c

use crate::backend::{ConBackend, XConn};
use crate::con::{
    con_descend_direction, con_descend_tiling_focused, con_disable_fullscreen, con_enable_fullscreen,
    con_exists, con_focus, con_get_fullscreen_covering_ws, con_get_workspace, con_has_managed_window,
    con_is_floating, con_is_hidden, con_is_internal, con_move_to_target, con_num_children,
    con_orientation, con_swap,
};
use crate::data::{ConId, ConType, Direction, FullscreenMode, Layout, Orientation, Position, Rect};
use crate::handlers::{button_event_t, EventContext};
use crate::output::output_get_content;
use crate::tree::{tree_render, tree_split, Tree};
use crate::util::{orientation_from_direction, position_from_direction, rect_contains};
use crate::workspace::workspace_is_visible;
use crate::xcb_util::xcursor_cursor_t;

/// C: XCB_ATOM_STRING
const XCB_ATOM_STRING: u32 = 31;

/// C: src/tiling_drag.c:13 is_tiling_drop_target
fn is_tiling_drop_target(t: &Tree, con: ConId) -> bool {
    if !con_has_managed_window(t, con) || con_is_floating(t, con) || con_is_hidden(t, con) {
        return false;
    }
    let ws = match con_get_workspace(t, con) {
        Some(ws) => ws,
        None => return false,
    };
    if con_is_internal(t, ws) {
        /* Skip containers on i3-internal containers like the scratchpad, which
         * are technically visible on their pseudo-output. */
        return false;
    }
    if !workspace_is_visible(t, ws) {
        return false;
    }
    if let Some(fs) = con_get_fullscreen_covering_ws(t, Some(ws)) {
        if fs != con {
            /* Workspace is visible, but con is not visible because some other
             * container is in fullscreen. */
            return false;
        }
    }
    true
}

/// C: src/tiling_drag.c:42 has_drop_targets
///
/// Returns whether there currently are any drop targets.
/// Used to only initiate a drag when there is something to drop onto.
pub fn has_drop_targets(t: &Tree) -> bool {
    let mut drop_targets = 0;
    for &con in &t.all_cons {
        if !is_tiling_drop_target(t, con) {
            continue;
        }
        drop_targets += 1;
    }

    /* In addition to tiling containers themselves, an visible but empty
     * workspace (in a multi-monitor scenario) also is a drop target. */
    if let Some(croot) = t.croot {
        let outputs: Vec<ConId> = t.cons[croot].focus_head.iter().copied().collect();
        for output in outputs {
            if con_is_internal(t, output) {
                continue;
            }
            let content = match output_get_content(t, output) {
                Some(content) => content,
                None => continue,
            };
            let visible_ws = t.cons[content]
                .nodes_head
                .iter()
                .copied()
                .find(|&child| workspace_is_visible(t, child));
            if let Some(visible_ws) = visible_ws {
                if con_num_children(t, visible_ws) == 0 {
                    drop_targets += 1;
                }
            }
        }
    }

    drop_targets > 1
}

/// C: src/tiling_drag.c:73 find_drop_target
///
/// Return an appropriate target at given coordinates.
fn find_drop_target(t: &Tree, x: u32, y: u32) -> Option<ConId> {
    for &con in &t.all_cons {
        let rect = t.cons[con].rect;
        if !rect_contains(rect, x, y) || !is_tiling_drop_target(t, con) {
            continue;
        }
        let ws = match con_get_workspace(t, con) {
            Some(ws) => ws,
            None => continue,
        };
        let fs = con_get_fullscreen_covering_ws(t, Some(ws));
        return Some(fs.unwrap_or(con));
    }

    /* Couldn't find leaf container, get a workspace. */
    let output = crate::output::get_output_containing(t, x, y)?;
    let output_con = t.outputs[output].con?;
    let content = output_get_content(t, output_con)?;
    /* Still descend because you can drag to the bar on a non-empty workspace. */
    Some(con_descend_tiling_focused(t, content))
}

/// C: src/tiling_drag.c:96 typedef enum { DT_SIBLING, DT_CENTER, DT_PARENT }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum drop_type_t {
    DT_SIBLING,
    DT_CENTER,
    DT_PARENT,
}

/// C: src/tiling_drag.c:108 adjust_rect
fn adjust_rect(mut rect: Rect, direction: Direction, threshold: u32) -> Rect {
    match direction {
        Direction::D_LEFT => {
            rect.width = threshold;
        }
        Direction::D_UP => {
            rect.height = threshold;
        }
        Direction::D_RIGHT => {
            rect.x = rect.x.wrapping_add(rect.width.wrapping_sub(threshold));
            rect.width = threshold;
        }
        Direction::D_DOWN => {
            rect.y = rect.y.wrapping_add(rect.height.wrapping_sub(threshold));
            rect.height = threshold;
        }
    }
    rect
}

/// C: src/tiling_drag.c:128 con_on_side_of_parent
fn con_on_side_of_parent(t: &Tree, con: ConId, direction: Direction) -> bool {
    let orientation = orientation_from_direction(direction);
    let reverse_direction = match direction {
        Direction::D_LEFT => Direction::D_RIGHT,
        Direction::D_RIGHT => Direction::D_LEFT,
        Direction::D_UP => Direction::D_DOWN,
        Direction::D_DOWN => Direction::D_UP,
    };
    let parent = match t.cons[con].parent {
        Some(parent) => parent,
        None => return false,
    };
    con_orientation(t, parent) != orientation
        || t.cons[parent].layout == Layout::L_STACKED
        || t.cons[parent].layout == Layout::L_TABBED
        || con_descend_direction(t, parent, reverse_direction) == con
}

/// C: src/tiling_drag.c:271 create_drop_indicator
fn create_drop_indicator(t: &Tree, x: &mut dyn XConn, rect: Rect) -> u32 {
    let back_pixel = t.config.client_focused_indicator;
    let values = [
        (crate::backend::XCB_CW_BACK_PIXEL, back_pixel),
        (crate::backend::XCB_CW_OVERRIDE_REDIRECT, 1),
    ];
    let indicator = crate::xcb_util::create_window(
        x,
        rect,
        crate::backend::XCB_COPY_FROM_PARENT as u16,
        crate::backend::XCB_COPY_FROM_PARENT,
        crate::backend::XCB_WINDOW_CLASS_INPUT_OUTPUT,
        xcursor_cursor_t::XCURSOR_CURSOR_MOVE,
        false,
        &values,
    );
    /* Change the window class to "i3-drag", so that it can be matched in a
     * compositor configuration. Note that the class needs to be changed before
     * mapping the window. */
    x.change_property(
        indicator,
        crate::data::XCB_ATOM_WM_CLASS,
        XCB_ATOM_STRING,
        8,
        crate::backend::PropMode::REPLACE,
        b"i3-drag\0i3-drag\0",
    );
    x.map_window(indicator);
    x.circulate_window(crate::backend::XCB_CIRCULATE_RAISE_LOWEST, indicator);

    indicator
}

/// C: src/tiling_drag.c:304 tiling_drag
///
/// Initiates a mouse drag operation on a tiled window.
#[allow(clippy::too_many_arguments)]
pub fn tiling_drag(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    a: &crate::xcb_util::Atoms,
    ctx: &mut EventContext,
    con: ConId,
    event: &button_event_t,
    use_threshold: bool,
) {
    let mut set_focus = t.focused == Some(con);
    let mut set_fs = t.cons[con].fullscreen_mode != FullscreenMode::CF_NONE;

    /* Don't change focus while dragging. */
    b.x_mask_event_mask(t, !crate::backend::XCB_EVENT_MASK_ENTER_WINDOW);
    x.flush();

    /* Indicate drop location while dragging. This blocks until the drag is
     * completed. */
    let mut target: Option<ConId> = None;
    let mut direction: Direction = Direction::D_LEFT;
    let mut drop_type: drop_type_t = drop_type_t::DT_CENTER;
    let mut indicator: u32 = 0;

    let drag_result = {
        /* C: src/tiling_drag.c:158 DRAGGING_CB(drag_callback)
         *
         * The callback writes target / direction / drop_type through the
         * captured locals (C passes a struct of pointers as `extra`). */
        let mut callback = |t: &mut Tree,
                            b: &mut dyn ConBackend,
                            x: &mut dyn XConn,
                            _a: &crate::xcb_util::Atoms,
                            _con: Option<ConId>,
                            _old_rect: &Rect,
                            new_x: u32,
                            new_y: u32,
                            _event: &button_event_t| {
            /* 30% of the container (minus the parent indicator) is used to
             * drop the dragged container as a sibling to the target */
            let sibling_indicator_percent_of_rect = 0.3;
            /* Use the base decoration height and add a few pixels. This makes
             * the outer indicator generally thin but at least thick enough to
             * cover container titles */
            let parent_indicator_max_size =
                (crate::render::render_deco_height(t) + libi3::dpi::logical_px(5)) as f64;

            let mut target_con = match find_drop_target(t, new_x, new_y) {
                Some(target_con) => target_con,
                None => return,
            };

            let mut rect = t.cons[target_con].rect;
            let mut draw_window = true;

            if t.cons[target_con].r#type == ConType::CT_WORKSPACE {
                /* C: goto create_indicator */
            } else {
                /* Define the thresholds in pixels. The drop type depends on the
                 * cursor position. */
                let min_rect_dimension = rect.width.min(rect.height);
                let sibling_indicator_size = std::cmp::max(
                    libi3::dpi::logical_px(2) as u32,
                    (sibling_indicator_percent_of_rect * min_rect_dimension as f64) as u32,
                );
                let parent_indicator_size = std::cmp::min(
                    parent_indicator_max_size as u32,
                    /* For small containers, start where the sibling indicator
                     * finishes. This is always at least 1 pixel. We use min() to
                     * not override the sibling indicator: */
                    sibling_indicator_size.saturating_sub(1),
                );

                /* Find which edge the cursor is closer to. */
                let d_left = new_x.wrapping_sub(rect.x);
                let d_top = new_y.wrapping_sub(rect.y);
                let d_right = rect.x.wrapping_add(rect.width).wrapping_sub(new_x);
                let d_bottom = rect.y.wrapping_add(rect.height).wrapping_sub(new_y);
                let d_min = d_left.min(d_right).min(d_top.min(d_bottom));
                /* And move the container towards that direction. */
                if d_left == d_min {
                    direction = Direction::D_LEFT;
                } else if d_top == d_min {
                    direction = Direction::D_UP;
                } else if d_right == d_min {
                    direction = Direction::D_RIGHT;
                } else if d_bottom == d_min {
                    direction = Direction::D_DOWN;
                } else {
                    /* Keep the compiler happy */
                    crate::log::errorlog("min() is broken\n");
                }
                let target_parent =
                    d_min < parent_indicator_size && con_on_side_of_parent(t, target_con, direction);
                let target_sibling = d_min < sibling_indicator_size;
                drop_type = if target_parent {
                    drop_type_t::DT_PARENT
                } else if target_sibling {
                    drop_type_t::DT_SIBLING
                } else {
                    drop_type_t::DT_CENTER
                };

                /* target == con makes sense only when we are moving away from
                 * target's parent. */
                if drop_type != drop_type_t::DT_PARENT && Some(target_con) == Some(con) {
                    draw_window = false;
                    x.destroy_window(indicator);
                    indicator = 0;
                } else {
                    match drop_type {
                        drop_type_t::DT_PARENT => {
                            loop {
                                let parent = match t.cons[target_con].parent {
                                    Some(parent) => parent,
                                    None => break,
                                };
                                if t.cons[parent].r#type == ConType::CT_WORKSPACE
                                    || !con_on_side_of_parent(t, parent, direction)
                                {
                                    break;
                                }
                                target_con = parent;
                            }
                            if let Some(parent) = t.cons[target_con].parent {
                                rect = adjust_rect(t.cons[parent].rect, direction, parent_indicator_size);
                            }
                        }
                        drop_type_t::DT_CENTER => {
                            rect = t.cons[target_con].rect;
                            rect.x = rect.x.wrapping_add(sibling_indicator_size);
                            rect.y = rect.y.wrapping_add(sibling_indicator_size);
                            rect.width = rect.width.wrapping_sub(sibling_indicator_size * 2);
                            rect.height = rect.height.wrapping_sub(sibling_indicator_size * 2);
                        }
                        drop_type_t::DT_SIBLING => {
                            rect = adjust_rect(t.cons[target_con].rect, direction, sibling_indicator_size);
                        }
                    }
                }
            }

            /* create_indicator: */
            if draw_window {
                if indicator == 0 {
                    indicator = create_drop_indicator(t, x, rect);
                } else {
                    x.configure_window(
                        indicator,
                        &[
                            (crate::backend::ConfigWindow::X, rect.x),
                            (crate::backend::ConfigWindow::Y, rect.y),
                            (crate::backend::ConfigWindow::WIDTH, rect.width),
                            (crate::backend::ConfigWindow::HEIGHT, rect.height),
                        ],
                    );
                }
            }
            b.x_mask_event_mask(t, !crate::backend::XCB_EVENT_MASK_ENTER_WINDOW);
            x.flush();

            target = Some(target_con);
        };

        crate::drag::drag_pointer(
            t,
            b,
            x,
            a,
            ctx,
            Some(con),
            event,
            /* confine_to = */ 0,
            Some(xcursor_cursor_t::XCURSOR_CURSOR_MOVE),
            use_threshold,
            &mut callback,
        )
    };

    /* Dragging is done. We don't need the indicator window any more. */
    x.destroy_window(indicator);

    let target = match target {
        Some(target) => target,
        None => {
            crate::log::debuglog("drop aborted\n");
            return;
        }
    };
    if drag_result == crate::drag::drag_result_t::DRAG_REVERT
        || (target == con && drop_type != drop_type_t::DT_PARENT)
        || !con_exists(t, target)
    {
        crate::log::debuglog("drop aborted\n");
        return;
    }

    let orientation = orientation_from_direction(direction);
    let position = position_from_direction(direction);
    let layout = if orientation == Orientation::VERT {
        Layout::L_SPLITV
    } else {
        Layout::L_SPLITH
    };
    con_disable_fullscreen(t, b, con);
    match drop_type {
        drop_type_t::DT_CENTER => {
            /* Also handles workspaces. */
            let mod_mask = (t.config.swap_modifier & 0xFFFF) as u16;
            let swap_pressed = mod_mask != 0 && (event.state & mod_mask) == mod_mask;
            if swap_pressed {
                if !con_swap(t, b, con, target) {
                    return;
                }
            } else {
                con_move_to_target(t, b, con, target);
            }
        }
        drop_type_t::DT_SIBLING => {
            if let Some(target_parent) = t.cons[target].parent {
                if con_orientation(t, target_parent) != orientation {
                    /* If con and target are the only children of the same
                     * parent, we can just change the parent's layout manually
                     * and then move con to the correct position. tree_split
                     * checks for a parent with only one child so it would create
                     * a new parent with the new layout. */
                    if t.cons[con].parent == Some(target_parent)
                        && con_num_children(t, target_parent) == 2
                    {
                        t.cons[target_parent].layout = layout;
                    } else {
                        tree_split(t, b, target, orientation);
                    }
                }
            }

            crate::move_::insert_con_into(t, b, con, target, position);

            b.ipc_send_window_event(t, "move", con);
        }
        drop_type_t::DT_PARENT => {
            let target_parent = match t.cons[target].parent {
                Some(parent) => parent,
                None => return,
            };
            let parent_tabbed_or_stacked = t.cons[target_parent].layout == Layout::L_TABBED
                || t.cons[target_parent].layout == Layout::L_STACKED;
            let mut target = target;
            if parent_tabbed_or_stacked {
                /* When dealing with tabbed/stacked the target can be in the
                 * middle of the container. Thus, after a directional move, con
                 * will still be bound to the tabbed/stacked parent. */
                if position == Position::BEFORE {
                    if let Some(first) = t.cons[target_parent].nodes_head.front().copied() {
                        target = first;
                    }
                } else if let Some(last) = t.cons[target_parent].nodes_head.back().copied() {
                    target = last;
                }
            }
            if con != target {
                crate::move_::insert_con_into(t, b, con, target, position);
            }
            /* tree_move can change the focus */
            let old_focus = t.focused;
            crate::move_::tree_move(t, b, con, direction);
            if t.focused != old_focus {
                if let Some(old_focus) = old_focus {
                    if con_exists(t, old_focus) {
                        con_focus(t, b, old_focus);
                    }
                }
            }
        }
    }

    /* Manage fullscreen status. */
    if set_focus || set_fs {
        let fs = con_get_workspace(t, con).and_then(|ws| con_get_fullscreen_covering_ws(t, Some(ws)));
        if fs == Some(con) {
            crate::log::errorlog("dragged container somehow got fullscreen again.\n");
        } else if let Some(fs) = fs {
            if set_focus && set_fs {
                /* con was focused & fullscreen, disable other fullscreen
                 * container. */
                con_disable_fullscreen(t, b, fs);
            } else {
                /* con was not focused, prefer other fullscreen container. */
                set_fs = false;
                set_focus = false;
            }
        } else if !set_focus {
            /* con was not focused. If it was fullscreen and we are moving it to
             * the focused workspace we must focus it. */
            let focused_ws = t.focused.and_then(|f| con_get_workspace(t, f));
            set_focus = set_fs && focused_ws == con_get_workspace(t, con);
        }
    }
    if set_fs {
        con_enable_fullscreen(t, b, con, FullscreenMode::CF_OUTPUT);
    }
    if set_focus {
        if let Some(ws) = con_get_workspace(t, con) {
            crate::workspace::workspace_show(t, b, ws);
        }
        con_focus(t, b, con);
    }
    tree_render(t, b);
}
