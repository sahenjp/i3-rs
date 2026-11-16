//! Minimal port of src/floating.c — only the parts the tree layer calls
//! directly (floating_raise_con, floating_fix_coordinates, floating_center,
//! floating_enable, floating_disable, toggle_floating_mode).
//!
//! C: src/floating.c

use crate::backend::ConBackend;
use crate::con::{
    con_activate, con_attach, con_border_style_rect, con_by_window_id, con_descend_focused,
    con_detach, con_descend_tiling_focused, con_exists, con_get_output,
    con_get_workspace, con_is_docked, con_is_floating, con_is_internal, con_new,
    con_num_children, ConId,
};
use crate::data::{
    border_t, ConType, FloatingState, FullscreenMode, KillWindow, Layout, Rect, ScratchpadState,
};
use crate::handlers::{button_event_t, EventContext};
use crate::tree::{tree_close_internal, tree_render, Tree};
use crate::xcb_util::xcursor_cursor_t;
use crate::util::rect_equals;
use crate::move_::insert_con_into;

/// C: src/floating.c:478 `floating_raise_con`
///
/// Raises the given container in the list of floating containers.
pub fn floating_raise_con(t: &mut Tree, _b: &mut dyn ConBackend, con: ConId) {
    let parent = t.cons[con].parent.expect("floating_raise_con: no parent");
    t.floating_remove(parent, con);
    t.floating_insert_tail(parent, con);
}

/// C: src/floating.c:528 `floating_center`
pub fn floating_center(t: &mut Tree, con: ConId, rect: Rect) {
    t.cons[con].rect.x = rect.x.wrapping_add(rect.width / 2).wrapping_sub(t.cons[con].rect.width / 2);
    t.cons[con].rect.y = rect.y.wrapping_add(rect.height / 2).wrapping_sub(t.cons[con].rect.height / 2);
}

/// C: src/floating.c:815 `floating_fix_coordinates`
///
/// Checks if con's coordinates are within its workspace and re-assigns it to
/// the actual workspace if not. (Here: the coordinate translation only.)
pub fn floating_fix_coordinates(t: &mut Tree, con: ConId, old_rect: &Rect, new_rect: &Rect) {
    /* First we get the x/y coordinates relative to the x/y coordinates
     * of the output on which the window is on */
    let rel_x = (t.cons[con].rect.x as i64 - old_rect.x as i64) + (t.cons[con].rect.width / 2) as i64;
    let rel_y = (t.cons[con].rect.y as i64 - old_rect.y as i64) + (t.cons[con].rect.height / 2) as i64;
    /* Then we calculate a fraction, for example 0.63 for a window
     * which is at y = 1212 of a 1920 px high output */
    /* Here we have to multiply at first. Or we will lose precision when not compiled with -msse2 */
    let new_x = new_rect.x as f64 + (rel_x * new_rect.width as i64) as f64 / old_rect.width as f64 - (t.cons[con].rect.width / 2) as i64 as f64;
    let new_y = new_rect.y as f64 + (rel_y * new_rect.height as i64) as f64 / old_rect.height as f64 - (t.cons[con].rect.height / 2) as i64 as f64;
    t.cons[con].rect.x = new_x as u32;
    t.cons[con].rect.y = new_y as u32;
}

/// C: src/floating.c:232 `floating_enable`
///
/// Enables floating mode for the given container. The focus-stack position
/// preservation logic is ported 1:1; the X-dependent parts
/// (floating_check_size, output sanity checks, floating_set_hint_atom) are
/// stubbed via the backend / TODO.
pub fn floating_enable(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, automatic: bool) -> bool {
    let set_focus = t.focused == Some(con);

    if con_is_docked(t, con) {
        /* Container is a dock window, not enabling floating mode. */
        return false;
    }

    if con_is_floating(t, con) {
        /* Container is already in floating mode, not doing anything. */
        return false;
    }

    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        /* Container is a workspace, not enabling floating mode. */
        return false;
    }

    /* Focus-stack position preservation:
     * Find recursively the ancestor container which is a child of our
     * workspace. We need to reuse its focus position later. */
    let mut focus_head_placeholder: Option<ConId> = None;
    let mut focus_before_parent = true;
    if !set_focus {
        let mut ancestor = con;
        while t.cons[t.cons[ancestor].parent.expect("floating_enable: no parent")].r#type != ConType::CT_WORKSPACE {
            let parent = t.cons[ancestor].parent.expect("floating_enable: no parent");
            focus_before_parent &= t.cons[parent].focus_head.front() == Some(&ancestor);
            ancestor = t.cons[ancestor].parent.expect("floating_enable: no parent");
        }
        /* Consider the part of the focus stack of our current workspace:
         * [ ... S_{i-1} S_{i} S_{i+1} ... ]
         * Where S_{x} is a container tree and the container 'con' that is being
         * switched to floating belongs in S_{i}. The new floating container,
         * 'nc', will have the workspace as its parent so it needs to be placed
         * in this stack. If C was focused we just need to call con_focus().
         * Otherwise, nc must be placed before or after S_{i}.
         * We should avoid using the S_{i} container for our operations since
         * it might get killed if it has no other children. So, the two
         * possible positions are after S_{i-1} or before S_{i+1}. */
        if focus_before_parent {
            focus_head_placeholder = t.focus_prev(ancestor);
        } else {
            focus_head_placeholder = t.focus_next(ancestor);
        }
    }

    /* 1: detach the container from its parent */
    let old_parent = t.cons[con].parent.expect("floating_enable: no parent");
    con_detach(t, con);
    crate::con::con_fix_percent(t, old_parent);

    /* 2: create a new container to render the decoration on, add
     * it as a floating window to the workspace */
    let nc = con_new(t, b, None, None);
    /* we need to set the parent afterwards instead of passing it as an
     * argument to con_new() because nc would be inserted into the tiling layer
     * otherwise. */
    let ws = con_get_workspace(t, con).expect("floating_enable: no workspace");
    t.cons[nc].parent = Some(ws);
    t.cons[nc].r#type = ConType::CT_FLOATING_CON;
    t.cons[nc].layout = Layout::L_SPLITH;
    /* We insert nc already, even though its rect is not yet calculated. This
     * is necessary because otherwise the workspace might be empty (and get
     * closed in tree_close_internal()) even though it's not. */
    t.floating_insert_head(ws, nc);

    if focus_before_parent {
        match focus_head_placeholder {
            Some(placeholder) => t.focus_insert_after(placeholder, nc),
            None => t.focus_insert_head(ws, nc),
        }
    } else {
        match focus_head_placeholder {
            Some(placeholder) => t.focus_insert_before(placeholder, nc),
            None => {
                /* Also used for the set_focus case */
                t.focus_insert_tail(ws, nc);
            }
        }
    }

    /* check if the parent container is empty and close it if so */
    let con_parent = t.cons[con].parent;
    if let Some(parent) = con_parent {
        if (t.cons[parent].r#type == ConType::CT_CON || t.cons[parent].r#type == ConType::CT_FLOATING_CON)
            && con_num_children(t, parent) == 0
        {
            /* Old container empty after setting this child to floating, closing */
            /* clear the pointer before calling tree_close_internal in which the
             * memory is freed */
            t.cons[con].parent = None;
            tree_close_internal(t, b, parent, KillWindow::DONT_KILL_WINDOW, false);
        }
    }

    let name = format!("[i3 con] floatingcon around {:?}", con);
    b.x_set_name(t, nc, &name);

    t.cons[nc].rect = t.cons[con].geometry;
    /* If the geometry was not set (split containers), we need to determine a
     * sensible one by combining the geometry of all children */
    if rect_equals(t.cons[nc].rect, Rect::new(0, 0, 0, 0)) {
        /* Geometry not set, combining children */
        let children: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
        for child in children {
            t.cons[nc].rect.width = t.cons[nc].rect.width.wrapping_add(t.cons[child].geometry.width);
            t.cons[nc].rect.height = std::cmp::max(t.cons[nc].rect.height, t.cons[child].geometry.height);
        }
    }

    t.cons[nc].nodes_head.push_back(con);
    t.cons[nc].focus_head.push_back(con);

    /* 3: attach the child to the new parent container. We need to do this
     * because con_border_style_rect() needs to access con->parent. */
    t.cons[con].parent = Some(nc);
    t.cons[con].percent = 1.0;
    t.cons[con].floating = FloatingState::FLOATING_USER_ON;

    /* 4: set the border style as specified with new_float */
    if automatic {
        t.cons[con].border_style = t.config.default_floating_border;
        t.cons[con].max_user_border_style = t.config.default_floating_border;
    }

    /* Add pixels for the decoration. */
    let bsr = con_border_style_rect(t, con);

    t.cons[nc].rect.height = t.cons[nc].rect.height.wrapping_sub(bsr.height);
    t.cons[nc].rect.width = t.cons[nc].rect.width.wrapping_sub(bsr.width);

    /* Honor the X11 border */
    t.cons[nc].rect.height = t.cons[nc].rect.height.wrapping_add((t.cons[con].border_width * 2) as u32);
    t.cons[nc].rect.width = t.cons[nc].rect.width.wrapping_add((t.cons[con].border_width * 2) as u32);

    /* C: src/floating.c:367 floating_check_size(nc, false); — honors the
     * window's size hints and the configured floating_minimum/maximum_size
     * (the sane defaults 75x50 apply when the user left them at 0). Without
     * this a freshly floating window keeps whatever geometry the client
     * asked for. */
    floating_check_size(t, nc, false);

    /* Some clients (like GIMP's color picker window) get mapped
     * to (0, 0), so we push them to a reasonable position
     * (centered over their leader) */
    if t.cons[nc].rect.x == 0 && t.cons[nc].rect.y == 0 {
        let leader = t.cons[con]
            .window
            .map(|w| t.windows[w].leader)
            .filter(|leader| {
                *leader != crate::data::XCB_WINDOW_NONE
                    && Some(*leader) != t.cons[con].window.map(|w| t.windows[w].id)
            })
            .and_then(|leader| con_by_window_id(t, leader));
        if let Some(leader) = leader {
            /* Centering above leader */
            let leader_rect = t.cons[leader].rect;
            floating_center(t, nc, leader_rect);
        } else {
            /* center the window on workspace as fallback */
            let ws_rect = t.cons[ws].rect;
            floating_center(t, nc, ws_rect);
        }
    }

    /* TODO: sanity check whether the coordinates are on the appropriate
     * output (needs get_output_from_rect + Output rects from randr). */

    /* render the cons to get initial window_rect correct */
    crate::render::render_con(t, b, nc);

    if set_focus {
        con_activate(t, b, con);
    }

    b.floating_set_hint_atom(t, nc, true);
    b.ipc_send_window_event(t, "floating", con);
    true
}

/// C: src/floating.c:419 `floating_disable`
pub fn floating_disable(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    if !con_is_floating(t, con) {
        /* Container isn't floating, not doing anything. */
        return;
    }

    let ws = con_get_workspace(t, con);
    if ws.map(|w| con_is_internal(t, w)).unwrap_or(false) {
        /* Can't disable floating for container in internal workspace. */
        return;
    }
    let ws = ws.expect("floating_disable: no workspace");
    let tiling_focused = con_descend_tiling_focused(t, ws);

    if t.cons[tiling_focused].r#type == ConType::CT_WORKSPACE {
        let parent = t.cons[con].parent.expect("floating_disable: no parent");
        con_detach(t, con);
        t.cons[con].parent = None;
        tree_close_internal(t, b, parent, KillWindow::DONT_KILL_WINDOW, true);
        con_attach(t, b, con, tiling_focused, false);
        t.cons[con].percent = 0.0;
        let parent = t.cons[con].parent.expect("floating_disable: no parent");
        crate::con::con_fix_percent(t, parent);
    } else {
        insert_con_into(t, b, con, tiling_focused, crate::data::Position::AFTER);
    }

    t.cons[con].floating = FloatingState::FLOATING_USER_OFF;
    b.floating_set_hint_atom(t, con, false);
    b.ipc_send_window_event(t, "floating", con);
}

/// C: src/floating.c:458 `toggle_floating_mode`
///
/// Toggles floating mode for the given container.
///
/// If the automatic flag is set to true, this was an automatic update by a
/// change of the window class from the application which can be overwritten
/// by the user.
pub fn toggle_floating_mode(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, automatic: bool) {
    /* forbid the command to toggle floating on a CT_FLOATING_CON */
    if t.cons[con].r#type == ConType::CT_FLOATING_CON {
        return;
    }

    /* see if the client is already floating */
    if con_is_floating(t, con) {
        /* already floating, re-setting to tiling */
        floating_disable(t, b, con);
        return;
    }

    floating_enable(t, b, con, automatic);
}
/// C: src/floating.c:22 total_outputs_dimensions
pub fn total_outputs_dimensions(t: &Tree) -> Rect {
    if t.outputs.is_empty() {
        // C: (Rect){0, 0, root_screen->width_in_pixels, root_screen->height_in_pixels}
        return match libi3::dpi::root_screen() {
            Some(screen) => Rect::new(0, 0, screen.width_in_pixels as u32, screen.height_in_pixels as u32),
            None => Rect::new(0, 0, 0, 0),
        };
    }

    /* Use Rect to encapsulate dimensions, ignoring x/y */
    let mut outputs_dimensions = Rect::new(0, 0, 0, 0);
    for output in &t.outputs {
        outputs_dimensions.height = outputs_dimensions.height.wrapping_add(output.rect.height);
        outputs_dimensions.width = outputs_dimensions.width.wrapping_add(output.rect.width);
    }
    outputs_dimensions
}

/// C: src/floating.c:76 floating_check_size
pub fn floating_check_size(t: &mut Tree, floating_con: ConId, prefer_height: bool) {
    /* Define reasonable minimal and maximal sizes for floating windows */
    const FLOATING_SANE_MIN_HEIGHT: i32 = 50;
    const FLOATING_SANE_MIN_WIDTH: i32 = 75;
    let focused_con = con_descend_focused(t, floating_con);

    let mut border_rect = con_border_style_rect(t, focused_con);
    /* We have to do the opposite calculations that render_con() do
     * to get the exact size we want. */
    border_rect.width = border_rect.width.wrapping_neg();
    border_rect.height = border_rect.height.wrapping_neg();

    /* undo x11 border */
    border_rect.width = border_rect.width.wrapping_add(2 * t.cons[focused_con].border_width as u32);
    border_rect.height = border_rect.height.wrapping_add(2 * t.cons[focused_con].border_width as u32);

    if let Some(window) = t.cons[focused_con].window {
        /* ICCCM says: If a base size is not provided, the minimum size is to
         * be used in its place and vice versa. */
        let w = &t.windows[window];
        let min_width = if w.min_width != 0 { w.min_width } else { w.base_width };
        let min_height = if w.min_height != 0 { w.min_height } else { w.base_height };
        let base_width = if w.base_width != 0 { w.base_width } else { w.min_width };
        let base_height = if w.base_height != 0 { w.base_height } else { w.min_height };
        let max_width = w.max_width;
        let max_height = w.max_height;
        let min_aspect_ratio = w.min_aspect_ratio;
        let max_aspect_ratio = w.max_aspect_ratio;
        let height_increment = w.height_increment;
        let width_increment = w.width_increment;

        let con = &mut t.cons[floating_con];
        if min_width != 0 {
            con.rect.width = con.rect.width.wrapping_sub(border_rect.width);
            con.rect.width = con.rect.width.max(min_width as u32);
            con.rect.width = con.rect.width.wrapping_add(border_rect.width);
        }

        if min_height != 0 {
            con.rect.height = con.rect.height.wrapping_sub(border_rect.height);
            con.rect.height = con.rect.height.max(min_height as u32);
            con.rect.height = con.rect.height.wrapping_add(border_rect.height);
        }

        if max_width != 0 {
            con.rect.width = con.rect.width.wrapping_sub(border_rect.width);
            con.rect.width = con.rect.width.min(max_width as u32);
            con.rect.width = con.rect.width.wrapping_add(border_rect.width);
        }

        if max_height != 0 {
            con.rect.height = con.rect.height.wrapping_sub(border_rect.height);
            con.rect.height = con.rect.height.min(max_height as u32);
            con.rect.height = con.rect.height.wrapping_add(border_rect.height);
        }

        /* Obey the aspect ratio, if any, unless we are in fullscreen mode. */
        if con.fullscreen_mode == FullscreenMode::CF_NONE
            && (min_aspect_ratio > 0.0 || max_aspect_ratio > 0.0)
        {
            let width = con.rect.width as f64 - base_width as f64 - border_rect.width as f64;
            let height = con.rect.height as f64 - base_height as f64 - border_rect.height as f64;
            let ar = width / height;
            let mut new_ar = -1.0f64;
            if min_aspect_ratio > 0.0 && ar < min_aspect_ratio {
                new_ar = min_aspect_ratio;
            } else if max_aspect_ratio > 0.0 && ar > max_aspect_ratio {
                new_ar = max_aspect_ratio;
            }
            if new_ar > 0.0 {
                let (width, height) = if prefer_height {
                    let width = (height * new_ar).round();
                    (width, (width / new_ar).round())
                } else {
                    let height = (width / new_ar).round();
                    ((height * new_ar).round(), height)
                };
                con.rect.width = (width + base_width as f64 + border_rect.width as f64) as u32;
                con.rect.height = (height + base_height as f64 + border_rect.height as f64) as u32;
            }
        }

        if height_increment != 0
            && con.rect.height >= (base_height as u32).wrapping_add(border_rect.height)
        {
            con.rect.height = con
                .rect
                .height
                .wrapping_sub((base_height as u32).wrapping_add(border_rect.height));
            con.rect.height -= con.rect.height % height_increment as u32;
            con.rect.height = con
                .rect
                .height
                .wrapping_add((base_height as u32).wrapping_add(border_rect.height));
        }

        if width_increment != 0
            && con.rect.width >= (base_width as u32).wrapping_add(border_rect.width)
        {
            con.rect.width = con
                .rect
                .width
                .wrapping_sub((base_width as u32).wrapping_add(border_rect.width));
            con.rect.width -= con.rect.width % width_increment as u32;
            con.rect.width = con
                .rect
                .width
                .wrapping_add((base_width as u32).wrapping_add(border_rect.width));
        }
    }

    /* Unless user requests otherwise (-1), raise the width/height to
     * reasonable minimum dimensions */
    if t.config.floating_minimum_height != -1 {
        let con = &mut t.cons[floating_con];
        con.rect.height = con.rect.height.wrapping_sub(border_rect.height);
        if t.config.floating_minimum_height == 0 {
            con.rect.height = con.rect.height.max(FLOATING_SANE_MIN_HEIGHT as u32);
        } else {
            con.rect.height = con.rect.height.max(t.config.floating_minimum_height as u32);
        }
        con.rect.height = con.rect.height.wrapping_add(border_rect.height);
    }

    if t.config.floating_minimum_width != -1 {
        let con = &mut t.cons[floating_con];
        con.rect.width = con.rect.width.wrapping_sub(border_rect.width);
        if t.config.floating_minimum_width == 0 {
            con.rect.width = con.rect.width.max(FLOATING_SANE_MIN_WIDTH as u32);
        } else {
            con.rect.width = con.rect.width.max(t.config.floating_minimum_width as u32);
        }
        con.rect.width = con.rect.width.wrapping_add(border_rect.width);
    }

    /* Unless user requests otherwise (-1), ensure width/height do not exceed
     * configured maxima or, if unconfigured, limit to combined width of all
     * outputs */
    let floating_sane_max_dimensions = total_outputs_dimensions(t);
    if t.config.floating_maximum_height != -1 {
        let con = &mut t.cons[floating_con];
        con.rect.height = con.rect.height.wrapping_sub(border_rect.height);
        if t.config.floating_maximum_height == 0 {
            if floating_sane_max_dimensions.height != 0 {
                con.rect.height = con.rect.height.min(floating_sane_max_dimensions.height);
            }
        } else {
            con.rect.height = con.rect.height.min(t.config.floating_maximum_height as u32);
        }
        con.rect.height = con.rect.height.wrapping_add(border_rect.height);
    }

    if t.config.floating_maximum_width != -1 {
        let con = &mut t.cons[floating_con];
        con.rect.width = con.rect.width.wrapping_sub(border_rect.width);
        if t.config.floating_maximum_width == 0 {
            if floating_sane_max_dimensions.width != 0 {
                con.rect.width = con.rect.width.min(floating_sane_max_dimensions.width);
            }
        } else {
            con.rect.width = con.rect.width.min(t.config.floating_maximum_width as u32);
        }
        con.rect.width = con.rect.width.wrapping_add(border_rect.width);
    }
}

/// C: src/floating.c:489 floating_maybe_reassign_ws
pub fn floating_maybe_reassign_ws(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) -> bool {
    let ws = match con_get_workspace(t, con) {
        Some(ws) => ws,
        None => return false,
    };
    if con_is_internal(t, ws) {
        return false;
    }

    let output = match crate::output::get_output_from_rect(t, t.cons[con].rect) {
        None => return false,
        Some(o) => o,
    };

    let output_con = match t.outputs[output].con {
        Some(con) => con,
        None => return false,
    };
    if con_get_output(t, con) == Some(output_con) {
        /* still the same ws */
        return false;
    }

    let content = match crate::output::output_get_content(t, output_con) {
        Some(content) => content,
        None => return false,
    };
    let ws = match t.cons[content].focus_head.front().copied() {
        Some(ws) => ws,
        None => return false,
    };
    let needs_focus = con_descend_focused(t, con);
    let needs_focus = if crate::con::con_inside_focused(t, needs_focus) {
        Some(needs_focus)
    } else {
        None
    };
    crate::con::con_move_to_workspace(t, b, con, ws, false, true, false);
    if let Some(needs_focus) = needs_focus {
        crate::workspace::workspace_show(t, b, ws);
        con_activate(t, b, needs_focus);
    }
    true
}

/// C: src/floating.c:754 floating_reposition
///
/// Repositions the CT_FLOATING_CON to have the coordinates specified by
/// newrect, but only if the coordinates are not out-of-bounds. Also reassigns
/// the floating con to a different workspace if this move was across
/// different outputs.
pub fn floating_reposition(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, newrect: Rect) -> bool {
    /* Sanity check: Are the new coordinates on any output? If not, we
     * ignore that request. */
    if crate::output::output_containing_rect(t, newrect).is_none() {
        crate::log::errorlog("No output found at destination coordinates. Not repositioning.\n");
        return false;
    }

    t.cons[con].rect = newrect;

    floating_maybe_reassign_ws(t, b, con);

    /* If this is a scratchpad window, don't auto center it from now on. */
    if t.cons[con].scratchpad_state == ScratchpadState::SCRATCHPAD_FRESH {
        t.cons[con].scratchpad_state = ScratchpadState::SCRATCHPAD_CHANGED;
    }

    crate::tree::tree_render(t, b);
    true
}

/// C: src/floating.c:537 floating_move_to_pointer
///
/// Moves the given floating con to the current pointer position.
pub fn floating_move_to_pointer(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn crate::backend::XConn,
    con: ConId,
) {
    debug_assert!(t.cons[con].r#type == ConType::CT_FLOATING_CON);

    let root = x.root_screen().root;
    let reply = match x.query_pointer(root) {
        Some(reply) => reply,
        None => {
            crate::log::errorlog("could not query pointer position, not moving this container\n");
            return;
        }
    };

    floating_move_to_position(t, b, con, i32::from(reply.root_x), i32::from(reply.root_y));
}

/// The rest of C's floating_move_to_pointer (src/floating.c:537) with the
/// pointer position already queried.
///
/// The command layer (src/commands.c:1897 cmd_move_window_to_mouse) has no
/// XConn — C reads the global conn there — so it queries the pointer through
/// ConBackend::x_query_pointer_root and lands here.
pub fn floating_move_to_position(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    con: ConId,
    root_x: i32,
    root_y: i32,
) {
    let output = match crate::output::get_output_containing(t, root_x as u32, root_y as u32) {
        Some(output) => output,
        None => {
            crate::log::errorlog("The pointer is not on any output, cannot move the container here.\n");
            return;
        }
    };
    let output_rect = t.outputs[output].rect;

    /* Determine where to put the window. */
    let mut x_pos = root_x as i64 - t.cons[con].rect.width as i64 / 2;
    let mut y_pos = root_y as i64 - t.cons[con].rect.height as i64 / 2;

    /* Correct target coordinates to be in-bounds. */
    x_pos = x_pos.max(output_rect.x as i64);
    y_pos = y_pos.max(output_rect.y as i64);
    if x_pos + t.cons[con].rect.width as i64 > output_rect.x as i64 + output_rect.width as i64 {
        x_pos = output_rect.x as i64 + output_rect.width as i64 - t.cons[con].rect.width as i64;
    }
    if y_pos + t.cons[con].rect.height as i64 > output_rect.y as i64 + output_rect.height as i64 {
        y_pos = output_rect.y as i64 + output_rect.height as i64 - t.cons[con].rect.height as i64;
    }

    /* Update container's coordinates to position it correctly. */
    let newrect = Rect {
        x: x_pos as u32,
        y: y_pos as u32,
        width: t.cons[con].rect.width,
        height: t.cons[con].rect.height,
    };
    floating_reposition(t, b, con, newrect);
}

/// C: src/floating.c:594 floating_drag_window
///
/// Called when the user clicked on the titlebar of a floating window.
/// Calls the drag_pointer function with the drag_window callback.
#[allow(clippy::too_many_arguments)]
pub fn floating_drag_window(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn crate::backend::XConn,
    a: &crate::xcb_util::Atoms,
    ctx: &mut EventContext,
    con: ConId,
    event: &button_event_t,
    use_threshold: bool,
) {
    /* Push changes before dragging, so that the window gets raised now and
     * not after the user releases the mouse button */
    tree_render(t, b);

    /* Store the initial rect in case of user revert/cancel */
    let initial_rect = t.cons[con].rect;

    /* Drag the window */
    let drag_result = {
        // C: DRAGGING_CB(drag_window_callback)
        let mut callback = |t: &mut Tree,
                            b: &mut dyn ConBackend,
                            x: &mut dyn crate::backend::XConn,
                            _a: &crate::xcb_util::Atoms,
                            con: Option<ConId>,
                            old_rect: &Rect,
                            new_x: u32,
                            new_y: u32,
                            event: &button_event_t| {
            let con = match con {
                Some(con) => con,
                None => return,
            };
            /* Reposition the client correctly while moving */
            t.cons[con].rect.x = old_rect
                .x
                .wrapping_add(new_x.wrapping_sub(event.root_x as u32));
            t.cons[con].rect.y = old_rect
                .y
                .wrapping_add(new_y.wrapping_sub(event.root_y as u32));

            crate::render::render_con(t, b, con);
            b.x_push_node(t, con);
            x.flush();

            /* Check if we cross workspace boundaries while moving */
            if !floating_maybe_reassign_ws(t, b, con) {
                return;
            }
            /* Ensure not to warp the pointer while dragging
             * (C: src/floating.c:585 x_set_warp_to(NULL)) */
            crate::x::x_set_warp_to(t.config.mouse_warping, None);
            tree_render(t, b);
        };

        crate::drag::drag_pointer(
            t,
            b,
            x,
            a,
            ctx,
            Some(con),
            event,
            0,
            Some(xcursor_cursor_t::XCURSOR_CURSOR_MOVE),
            use_threshold,
            &mut callback,
        )
    };

    if !con_exists(t, con) {
        return;
    }

    /* If the user cancelled, undo the changes. */
    if drag_result == crate::drag::drag_result_t::DRAG_REVERT {
        floating_reposition(t, b, con, initial_rect);
        return;
    }

    /* If this is a scratchpad window, don't auto center it from now on. */
    if t.cons[con].scratchpad_state == ScratchpadState::SCRATCHPAD_FRESH {
        t.cons[con].scratchpad_state = ScratchpadState::SCRATCHPAD_CHANGED;
    }

    tree_render(t, b);
}

/// C: src/floating.c:697 floating_resize_window
///
/// Called when the user clicked on a floating window while holding the
/// floating_modifier and the right mouse button.
/// Calls the drag_pointer function with the resize_window callback.
#[allow(clippy::too_many_arguments)]
pub fn floating_resize_window(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn crate::backend::XConn,
    a: &crate::xcb_util::Atoms,
    ctx: &mut EventContext,
    con: ConId,
    proportional: bool,
    event: &button_event_t,
) {
    /* Push changes before resizing, so that the window gets raised now and
     * not after the user releases the mouse button */
    tree_render(t, b);

    /* corner saves the nearest corner to the original click. It contains
     * a bitmask of the nearest borders (BORDER_LEFT, BORDER_RIGHT, …) */
    let mut corner = border_t::default();

    if event.event_x <= (t.cons[con].rect.width / 2) as i32 {
        corner |= border_t::BORDER_LEFT;
    } else {
        corner |= border_t::BORDER_RIGHT;
    }

    let cursor;
    if event.event_y <= (t.cons[con].rect.height / 2) as i32 {
        corner |= border_t::BORDER_TOP;
        cursor = if corner.has(border_t::BORDER_LEFT) {
            xcursor_cursor_t::XCURSOR_CURSOR_TOP_LEFT_CORNER
        } else {
            xcursor_cursor_t::XCURSOR_CURSOR_TOP_RIGHT_CORNER
        };
    } else {
        corner |= border_t::BORDER_BOTTOM;
        cursor = if corner.has(border_t::BORDER_LEFT) {
            xcursor_cursor_t::XCURSOR_CURSOR_BOTTOM_LEFT_CORNER
        } else {
            xcursor_cursor_t::XCURSOR_CURSOR_BOTTOM_RIGHT_CORNER
        };
    }

    /* get the initial rect in case of revert/cancel */
    let initial_rect = t.cons[con].rect;

    let drag_result = {
        // C: DRAGGING_CB(resize_window_callback)
        let mut callback = |t: &mut Tree,
                            b: &mut dyn ConBackend,
                            x: &mut dyn crate::backend::XConn,
                            _a: &crate::xcb_util::Atoms,
                            con: Option<ConId>,
                            old_rect: &Rect,
                            new_x: u32,
                            new_y: u32,
                            event: &button_event_t| {
            let con = match con {
                Some(con) => con,
                None => return,
            };
            let mut dest_x = t.cons[con].rect.x as i64;
            let mut dest_y = t.cons[con].rect.y as i64;

            let ratio = old_rect.width as f64 / old_rect.height as f64;

            let dx = new_x as i64 - event.root_x as i64;
            let dy = new_y as i64 - event.root_y as i64;

            /* First guess: We resize by exactly the amount the mouse moved,
             * taking into account in which corner the client was grabbed */
            let dest_width = if corner.has(border_t::BORDER_LEFT) {
                old_rect.width.wrapping_sub(dx as u32)
            } else {
                old_rect.width.wrapping_add(dx as u32)
            };

            let dest_height = if corner.has(border_t::BORDER_TOP) {
                old_rect.height.wrapping_sub(dy as u32)
            } else {
                old_rect.height.wrapping_add(dy as u32)
            };

            /* User wants to keep proportions, so we may have to adjust our
             * values */
            let (dest_width, dest_height) = if proportional {
                (
                    std::cmp::max(dest_width as i32, (dest_height as f64 * ratio) as i32) as u32,
                    std::cmp::max(dest_height as i32, (dest_width as f64 / ratio) as i32) as u32,
                )
            } else {
                (dest_width, dest_height)
            };

            t.cons[con].rect.width = dest_width;
            t.cons[con].rect.height = dest_height;

            /* Obey window size */
            floating_check_size(t, con, false);

            /* If not the lower right corner is grabbed, we must also
             * reposition the client by exactly the amount we resized it */
            if corner.has(border_t::BORDER_LEFT) {
                dest_x = old_rect.x as i64 + (old_rect.width as i64 - t.cons[con].rect.width as i64);
            }

            if corner.has(border_t::BORDER_TOP) {
                dest_y = old_rect.y as i64 + (old_rect.height as i64 - t.cons[con].rect.height as i64);
            }

            t.cons[con].rect.x = dest_x as u32;
            t.cons[con].rect.y = dest_y as u32;

            crate::render::render_con(t, b, con);
            if let Some(croot) = t.croot {
                b.x_push_changes(t, croot);
            }
            x.flush();
        };

        crate::drag::drag_pointer(
            t,
            b,
            x,
            a,
            ctx,
            Some(con),
            event,
            0,
            Some(cursor),
            false,
            &mut callback,
        )
    };

    if !con_exists(t, con) {
        return;
    }

    /* If the user cancels, undo the resize */
    if drag_result == crate::drag::drag_result_t::DRAG_REVERT {
        floating_reposition(t, b, con, initial_rect);
    }

    /* If this is a scratchpad window, don't auto center it from now on. */
    if t.cons[con].scratchpad_state == ScratchpadState::SCRATCHPAD_FRESH {
        t.cons[con].scratchpad_state = ScratchpadState::SCRATCHPAD_CHANGED;
    }
}

