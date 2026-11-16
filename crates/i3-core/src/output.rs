//! 1:1 port of src/output.c plus the pure logic output helpers from
//! src/randr.c.
//!
//! C: src/output.c, src/randr.c

use crate::backend::ConBackend;
use crate::con::{con_descend_focused, con_get_output, con_get_workspace, con_is_sticky, ConId};
use crate::data::{ConType, Direction, Rect};
use crate::tree::Tree;
use crate::workspace::workspace_is_visible;

/// Index into `Tree::outputs` (the C `Output *`).
pub type OutputId = usize;

/// C: src/output.c:16 `output_get_content`
///
/// Returns the content container below the given output container.
pub fn output_get_content(t: &Tree, output: ConId) -> Option<ConId> {
    for &child in &t.cons[output].nodes_head {
        if t.cons[child].r#type == ConType::CT_CON {
            return Some(child);
        }
    }
    None
}

/// C: src/randr.c:50 `get_output_by_name`
///
/// Returns the output with the given name or None.
/// If require_active is true, only active outputs are considered.
pub fn get_output_by_name(t: &Tree, name: &str, require_active: bool) -> Option<OutputId> {
    let get_primary = name.eq_ignore_ascii_case("primary");
    let get_non_primary = name.eq_ignore_ascii_case("nonprimary");

    for (idx, output) in t.outputs.iter().enumerate() {
        if require_active && !output.active {
            continue;
        }
        if output.primary && get_primary {
            return Some(idx);
        }
        if !output.primary && get_non_primary {
            return Some(idx);
        }
        for output_name in &output.names_head {
            if output_name.name.eq_ignore_ascii_case(name) {
                return Some(idx);
            }
        }
    }

    None
}

/// C: src/randr.c:122 `get_output_containing`
///
/// Returns the active (!) output which contains the coordinates x, y or None
/// if there is no output which contains these coordinates.
pub fn get_output_containing(t: &Tree, x: u32, y: u32) -> Option<OutputId> {
    for (idx, output) in t.outputs.iter().enumerate() {
        if !output.active {
            continue;
        }
        let rect = output.rect;
        if x >= rect.x && x < rect.x.wrapping_add(rect.width) && y >= rect.y && y < rect.y.wrapping_add(rect.height) {
            return Some(idx);
        }
    }

    None
}

/// C: src/randr.c:259 `get_output_next`
///
/// Gets the output which is the next one in the given direction.
///
/// If close_far == CLOSEST_OUTPUT, then the output next to the current one
/// will be selected. If close_far == FARTHEST_OUTPUT, the output which is
/// the last one in the given direction will be selected.
///
/// None will be returned when no active outputs are present in the direction
/// specified (note that "current" counts as such an output).
pub fn get_output_next(t: &Tree, direction: Direction, current: OutputId, close_far: crate::data::OutputCloseFar) -> Option<OutputId> {
    let cur = t.outputs[current].rect;
    let mut best: Option<usize> = None;
    for (idx, output) in t.outputs.iter().enumerate() {
        if !output.active {
            continue;
        }

        let other = output.rect;
        if (direction == Direction::D_RIGHT && other.x > cur.x) || (direction == Direction::D_LEFT && other.x < cur.x) {
            /* Skip the output when it doesn't overlap the other one's y
             * coordinate at all. */
            if other.y.wrapping_add(other.height) <= cur.y || cur.y.wrapping_add(cur.height) <= other.y {
                continue;
            }
        } else if (direction == Direction::D_DOWN && other.y > cur.y) || (direction == Direction::D_UP && other.y < cur.y) {
            /* Skip the output when it doesn't overlap the other one's x
             * coordinate at all. */
            if other.x.wrapping_add(other.width) <= cur.x || cur.x.wrapping_add(cur.width) <= other.x {
                continue;
            }
        } else {
            continue;
        }

        /* No candidate yet? Start with this one. */
        let best_rect = best.map(|b| t.outputs[b].rect);
        if best.is_none() {
            best = Some(idx);
            continue;
        }
        let best_rect = best_rect.expect("checked above");

        if close_far == crate::data::OutputCloseFar::CLOSEST_OUTPUT {
            /* Is this output better (closer to the current output) than our
             * current best bet? */
            if (direction == Direction::D_RIGHT && other.x < best_rect.x)
                || (direction == Direction::D_LEFT && other.x > best_rect.x)
                || (direction == Direction::D_DOWN && other.y < best_rect.y)
                || (direction == Direction::D_UP && other.y > best_rect.y)
            {
                best = Some(idx);
                continue;
            }
        } else {
            /* Is this output better (farther to the current output) than our
             * current best bet? */
            if (direction == Direction::D_RIGHT && other.x > best_rect.x)
                || (direction == Direction::D_LEFT && other.x < best_rect.x)
                || (direction == Direction::D_DOWN && other.y > best_rect.y)
                || (direction == Direction::D_UP && other.y < best_rect.y)
            {
                best = Some(idx);
                continue;
            }
        }
    }

    best
}

/// C: src/randr.c:225 `get_output_next_wrap`
///
/// Like get_output_next with close_far == CLOSEST_OUTPUT, but wraps.
///
/// This function always returns a output: if no active outputs can be found,
/// current itself is returned.
pub fn get_output_next_wrap(t: &Tree, direction: Direction, current: Option<OutputId>) -> Option<OutputId> {
    let current = current?;
    let mut best = get_output_next(t, direction, current, crate::data::OutputCloseFar::CLOSEST_OUTPUT);
    /* If no output can be found, wrap */
    if best.is_none() {
        let opposite = match direction {
            Direction::D_RIGHT => Direction::D_LEFT,
            Direction::D_LEFT => Direction::D_RIGHT,
            Direction::D_DOWN => Direction::D_UP,
            Direction::D_UP => Direction::D_DOWN,
        };
        best = get_output_next(t, opposite, current, crate::data::OutputCloseFar::FARTHEST_OUTPUT);
    }
    if best.is_none() {
        best = Some(current);
    }
    best
}

/// C: src/output.c:26 `get_output_from_string`
///
/// Returns an 'output' corresponding to one of left/right/down/up or a
/// specific output name.
pub fn get_output_from_string(t: &Tree, current_output: Option<OutputId>, output_str: &str) -> Option<OutputId> {
    if output_str.eq_ignore_ascii_case("current") {
        return t.focused.and_then(|f| get_output_for_con(t, f));
    }
    if output_str.eq_ignore_ascii_case("left") {
        return get_output_next_wrap(t, Direction::D_LEFT, current_output);
    }
    if output_str.eq_ignore_ascii_case("right") {
        return get_output_next_wrap(t, Direction::D_RIGHT, current_output);
    }
    if output_str.eq_ignore_ascii_case("up") {
        return get_output_next_wrap(t, Direction::D_UP, current_output);
    }
    if output_str.eq_ignore_ascii_case("down") {
        return get_output_next_wrap(t, Direction::D_DOWN, current_output);
    }

    get_output_by_name(t, output_str, true)
}

/// C: src/output.c:51 `output_primary_name`
///
/// Retrieves the primary name of an output.
pub fn output_primary_name(t: &Tree, output: OutputId) -> String {
    t.outputs[output]
        .names_head
        .front()
        .map(|n| n.name.clone())
        .unwrap_or_default()
}

/// C: src/output.c:60 `get_output_for_con`
///
/// Retrieves the output for a given container. Never returns None in practice
/// (C asserts; internal workspaces would trip the assert).
pub fn get_output_for_con(t: &Tree, con: ConId) -> Option<OutputId> {
    let output_con = con_get_output(t, con)?;
    let name = t.cons[output_con].name.clone()?;
    get_output_by_name(t, &name, true)
}

/// C: src/output.c:80 `output_push_sticky_windows`
///
/// Iterates over all outputs and pushes sticky windows to the currently
/// visible workspace on that output.
///
/// old_focus is used to determine if a sticky window is going to be focused.
/// old_focus might be different than the currently focused container because
/// the caller might need to temporarily change the focus and then call
/// output_push_sticky_windows.
pub fn output_push_sticky_windows(t: &mut Tree, b: &mut dyn ConBackend, old_focus: Option<ConId>) {
    let croot = match t.croot {
        None => return,
        Some(c) => c,
    };
    let outputs: Vec<ConId> = t.cons[croot].focus_head.iter().copied().collect();
    for output in outputs {
        let content = match output_get_content(t, output) {
            None => continue,
            Some(c) => c,
        };
        let mut visible_ws: Option<ConId> = None;
        for &child in &t.cons[content].nodes_head {
            if workspace_is_visible(t, child) {
                visible_ws = Some(child);
                break;
            }
        }

        /* We use this loop instead of TAILQ_FOREACH to avoid problems if the
         * sticky window was the last window on that workspace as moving it in
         * this case will close the workspace. (The list is snapshotted.) */
        let workspaces: Vec<ConId> = t.cons[content].focus_head.iter().copied().collect();
        for current_ws in workspaces {
            /* Since moving the windows actually removes them from the list of
             * floating windows on this workspace, here too we need to use
             * another loop than TAILQ_FOREACH. */
            let children: Vec<ConId> = t.cons[current_ws].focus_head.iter().copied().collect();
            for current in children {
                if t.cons[current].r#type != ConType::CT_FLOATING_CON || !con_is_sticky(t, current) {
                    continue;
                }

                let ignore_focus = match old_focus {
                    None => true,
                    Some(of) => Some(current) != t.cons[of].parent,
                };
                let visible_ws = match visible_ws {
                    None => continue,
                    Some(v) => v,
                };
                crate::con::con_move_to_workspace(t, b, current, visible_ws, true, false, ignore_focus);
                if !ignore_focus {
                    let current_ws = t.focused.and_then(|f| con_get_workspace(t, f));
                    crate::con::con_activate(t, b, con_descend_focused(t, current));
                    /* Pushing sticky windows shouldn't change the focused workspace. */
                    if let Some(cws) = current_ws {
                        crate::con::con_activate(t, b, con_descend_focused(t, cws));
                    }
                }
            }
        }
    }
}

/// C: src/randr.c:145 get_output_from_rect
///
/// Returns the output with the maximum intersecting area.
pub fn get_output_from_rect(t: &Tree, rect: Rect) -> Option<OutputId> {
    let mid_x = rect.x.wrapping_add(rect.width / 2);
    let mid_y = rect.y.wrapping_add(rect.height / 2);
    let output = get_output_containing(t, mid_x, mid_y);

    match output {
        Some(o) => Some(o),
        None => output_containing_rect(t, rect),
    }
}

/// C: src/randr.c:183 output_containing_rect
///
/// Returns the output with the maximum intersecting area of the given rect.
pub fn output_containing_rect(t: &Tree, rect: Rect) -> Option<OutputId> {
    let lx = rect.x as i64;
    let uy = rect.y as i64;
    let rx = rect.x.wrapping_add(rect.width) as i64;
    let by = rect.y.wrapping_add(rect.height) as i64;
    let mut result: Option<(OutputId, i64)> = None;
    for (idx, output) in t.outputs.iter().enumerate() {
        if !output.active {
            continue;
        }
        let lx_o = output.rect.x as i64;
        let uy_o = output.rect.y as i64;
        let rx_o = output.rect.x.wrapping_add(output.rect.width) as i64;
        let by_o = output.rect.y.wrapping_add(output.rect.height) as i64;
        let left = lx.max(lx_o);
        let right = rx.min(rx_o);
        let bottom = by.min(by_o);
        let top = uy.max(uy_o);
        if left < right && top < bottom {
            let area = (right - left) * (bottom - top);
            if result.is_none() || area > result.unwrap().1 {
                result = Some((idx, area));
            }
        }
    }
    result.map(|(idx, _)| idx)
}
