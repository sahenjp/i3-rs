//! 1:1 port of src/click.c — Button press (mouse click) events.
//!
//! C: src/click.c

use crate::backend::{ConBackend, XConn};
use crate::con::{
    con_activate, con_border_style_rect, con_by_frame_id, con_by_window_id, con_descend_focused,
    con_get_output, con_get_workspace, con_has_parent, con_inside_floating, con_is_internal,
};
use crate::data::{
    border_t, ConId, ConType, Direction, Layout, Orientation, Position,
};
use crate::handlers::{button_event_t, EventContext};
use crate::tree::{get_tree_next_sibling, tree_render, Tree};
use crate::util::rect_contains;

/// C: src/click.c:14 click_destination_t
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum click_destination_t {
    CLICK_BORDER = 0,
    CLICK_DECORATION = 1,
    CLICK_INSIDE = 2,
}

/// C: src/click.c:24 tiling_resize_for_border
///
/// Finds the correct pair of first/second cons between the resize will take
/// place according to the passed border position (top, left, right, bottom),
/// then calls resize_graphical_handler().
pub fn tiling_resize_for_border(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    a: &crate::xcb_util::Atoms,
    ctx: &mut EventContext,
    con: ConId,
    border: border_t,
    event: &button_event_t,
    use_threshold: bool,
) -> bool {
    let mut first = con;
    let mut second: Option<ConId> = None;
    let search_direction = if border == border_t::BORDER_LEFT {
        Direction::D_LEFT
    } else if border == border_t::BORDER_RIGHT {
        Direction::D_RIGHT
    } else if border == border_t::BORDER_TOP {
        Direction::D_UP
    } else if border == border_t::BORDER_BOTTOM {
        Direction::D_DOWN
    } else {
        crate::log::errorlog("BUG: invalid border value\n");
        return false;
    };

    let res = crate::gaps::resize_find_tiling_participants(
        t,
        &mut first,
        &mut second,
        search_direction,
        false,
    );
    let mut second = match second {
        Some(second) if res => second,
        _ => {
            return false;
        }
    };
    if t.cons[first].fullscreen_mode != t.cons[second].fullscreen_mode {
        return false;
    }

    debug_assert!(first != second);
    debug_assert!(t.cons[first].parent == t.cons[second].parent);

    /* The first container should always be in front of the second container */
    if search_direction == Direction::D_UP || search_direction == Direction::D_LEFT {
        std::mem::swap(&mut first, &mut second);
    }

    let orientation = if border == border_t::BORDER_LEFT || border == border_t::BORDER_RIGHT {
        Orientation::HORIZ
    } else {
        Orientation::VERT
    };

    crate::resize::resize_graphical_handler(
        t,
        b,
        x,
        a,
        ctx,
        first,
        second,
        orientation,
        event,
        use_threshold,
    );

    tree_render(t, b);
    true
}

/// C: src/click.c:84 floating_mod_on_tiled_client
///
/// Called when the user clicks using the floating_modifier, but the client is
/// in tiling layout.
///
/// Returns false if it does not do anything (that is, the click should be
/// sent to the client).
fn floating_mod_on_tiled_client(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    a: &crate::xcb_util::Atoms,
    ctx: &mut EventContext,
    con: ConId,
    event: &button_event_t,
) -> bool {
    /* The client is in tiling layout. We can still initiate a resize with the
     * right mouse button, by choosing the border which is the most near one to
     * the position of the mouse pointer */
    let to_right = t.cons[con].rect.width as i32 - event.event_x;
    let to_left = event.event_x;
    let to_top = event.event_y;
    let to_bottom = t.cons[con].rect.height as i32 - event.event_y;

    if to_right < to_left && to_right < to_top && to_right < to_bottom {
        return tiling_resize_for_border(
            t,
            b,
            x,
            a,
            ctx,
            con,
            border_t::BORDER_RIGHT,
            event,
            false,
        );
    }

    if to_left < to_right && to_left < to_top && to_left < to_bottom {
        return tiling_resize_for_border(
            t,
            b,
            x,
            a,
            ctx,
            con,
            border_t::BORDER_LEFT,
            event,
            false,
        );
    }

    if to_top < to_right && to_top < to_left && to_top < to_bottom {
        return tiling_resize_for_border(
            t,
            b,
            x,
            a,
            ctx,
            con,
            border_t::BORDER_TOP,
            event,
            false,
        );
    }

    if to_bottom < to_right && to_bottom < to_left && to_bottom < to_top {
        return tiling_resize_for_border(
            t,
            b,
            x,
            a,
            ctx,
            con,
            border_t::BORDER_BOTTOM,
            event,
            false,
        );
    }

    false
}

/// C: src/click.c:127 tiling_resize
///
/// Finds out which border was clicked on and calls tiling_resize_for_border().
fn tiling_resize(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    a: &crate::xcb_util::Atoms,
    ctx: &mut EventContext,
    con: ConId,
    event: &button_event_t,
    dest: click_destination_t,
    use_threshold: bool,
) -> bool {
    /* check if this was a click on the window border (and on which one) */
    let bsr = con_border_style_rect(t, con);

    if dest == click_destination_t::CLICK_DECORATION {
        return tiling_resize_for_border(
            t,
            b,
            x,
            a,
            ctx,
            con,
            border_t::BORDER_TOP,
            event,
            use_threshold,
        );
    } else if dest == click_destination_t::CLICK_BORDER {
        if event.event_y >= 0
            && event.event_y <= bsr.y as i32
            && event.event_x >= bsr.x as i32
            && event.event_x <= (t.cons[con].rect.width.wrapping_add(bsr.width)) as i32
        {
            return tiling_resize_for_border(
                t,
                b,
                x,
                a,
                ctx,
                con,
                border_t::BORDER_TOP,
                event,
                false,
            );
        }
    }
    if event.event_x >= 0
        && event.event_x <= bsr.x as i32
        && event.event_y >= bsr.y as i32
        && event.event_y <= (t.cons[con].rect.height.wrapping_add(bsr.height)) as i32
    {
        return tiling_resize_for_border(
            t,
            b,
            x,
            a,
            ctx,
            con,
            border_t::BORDER_LEFT,
            event,
            false,
        );
    }

    if event.event_x >= (t.cons[con].window_rect.x.wrapping_add(t.cons[con].window_rect.width)) as i32
        && event.event_y >= bsr.y as i32
        && event.event_y <= (t.cons[con].rect.height.wrapping_add(bsr.height)) as i32
    {
        return tiling_resize_for_border(
            t,
            b,
            x,
            a,
            ctx,
            con,
            border_t::BORDER_RIGHT,
            event,
            false,
        );
    }

    if event.event_y >= (t.cons[con].window_rect.y.wrapping_add(t.cons[con].window_rect.height)) as i32 {
        return tiling_resize_for_border(
            t,
            b,
            x,
            a,
            ctx,
            con,
            border_t::BORDER_BOTTOM,
            event,
            false,
        );
    }

    false
}

/// C: src/click.c:158 allow_replay_pointer
pub fn allow_replay_pointer(t: &mut Tree, b: &mut dyn ConBackend, x: &mut dyn XConn, time: u32) {
    x.allow_events(crate::backend::XCB_ALLOW_REPLAY_POINTER, time);
    x.flush();
    tree_render(t, b);
}

/// C: src/click.c:169 route_click
///
/// Being called by handle_button_press, this function calls the appropriate
/// functions for resizing/dragging.
pub fn route_click(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    a: &crate::xcb_util::Atoms,
    ctx: &mut EventContext,
    con: ConId,
    event: &button_event_t,
    dest: click_destination_t,
) {
    let mod_mask = (t.config.floating_modifier & 0xFFFF) as u16;
    let mod_pressed = mod_mask != 0 && (event.state & mod_mask) == mod_mask;

    // C: src/click.c:173-175 DLOG("--> click properties: mod = %d, ...")
    crate::log::debuglog(&format!(
        "--> click properties: mod = {}, destination = {:?}
",
        mod_pressed, dest as u32
    ));

    /* don't handle dockarea cons, they must not be focused */
    if t.cons[con].parent.map(|p| t.cons[p].r#type == ConType::CT_DOCKAREA) == Some(true) {
        allow_replay_pointer(t, b, x, event.time);
        return;
    }

    /* if the user has bound an action to this click, it should override the
     * default behavior. */
    let binding_event = crate::bindings::BindingEvent {
        response_type: event.response_type,
        state: event.state,
        detail: u16::from(event.detail),
    };
    let bind_index = crate::bindings::get_binding_from_xcb_event(t, &binding_event);
    if let Some(index) = bind_index {
        let bind = t.bindings[index].clone();
        let applies = (dest == click_destination_t::CLICK_DECORATION && !bind.exclude_titlebar)
            || (dest == click_destination_t::CLICK_INSIDE && bind.whole_window)
            || (dest == click_destination_t::CLICK_BORDER && bind.border);
        if applies {
            if let Some(bindings) = ctx.bindings.as_deref_mut() {
                let mut hooks = crate::ipc::IpcHooks;
                let _ = crate::bindings::run_binding(bindings, t, b, &mut hooks, &bind, Some(con));
            }

            /* ASYNC_POINTER eats the event */
            x.allow_events(crate::backend::XCB_ALLOW_ASYNC_POINTER, event.time);
            x.flush();
            return;
        }
    }

    /* There is no default behavior for button release events so we are done. */
    if event.response_type == crate::bindings::XCB_BUTTON_RELEASE {
        allow_replay_pointer(t, b, x, event.time);
        return;
    }

    /* Any click in a workspace should focus that workspace. If the
     * workspace is on another output we need to do a workspace_show in
     * order for i3bar (and others) to notice the change in workspace. */
    let mut ws = con_get_workspace(t, con);
    let focused_workspace = t.focused.and_then(|f| con_get_workspace(t, f));

    if ws.is_none() {
        let content = con_get_output(t, con).and_then(|o| crate::output::output_get_content(t, o));
        ws = content.and_then(|c| t.cons[c].focus_head.front().copied());
        if ws.is_none() {
            allow_replay_pointer(t, b, x, event.time);
            return;
        }
    }
    let ws = ws.expect("route_click: workspace");

    /* get the floating con */
    let floatingcon = con_inside_floating(t, con);
    let proportional = (event.state & crate::backend::XCB_KEY_BUT_MASK_SHIFT)
        == crate::backend::XCB_KEY_BUT_MASK_SHIFT;
    let parent = t.cons[con].parent.expect("route_click: con without parent");
    let in_stacked =
        t.cons[parent].layout == Layout::L_STACKED || t.cons[parent].layout == Layout::L_TABBED;
    let was_focused = t.focused == Some(con);
    let is_left_click = event.detail == crate::backend::XCB_BUTTON_CLICK_LEFT;
    let is_right_click = event.detail == crate::backend::XCB_BUTTON_CLICK_RIGHT;
    let is_left_or_right_click = is_left_click || is_right_click;
    let is_scroll = event.detail == crate::backend::XCB_BUTTON_SCROLL_UP
        || event.detail == crate::backend::XCB_BUTTON_SCROLL_DOWN
        || event.detail == crate::backend::XCB_BUTTON_SCROLL_LEFT
        || event.detail == crate::backend::XCB_BUTTON_SCROLL_RIGHT;

    /* 1: see if the user scrolled on the decoration of a stacked/tabbed con */
    if in_stacked && dest == click_destination_t::CLICK_DECORATION && is_scroll {
        /* Correctly move workspace focus first, see: #5472 */
        crate::workspace::workspace_show(t, b, ws);

        /* Use the focused child of the tabbed / stacked container, not the
         * container the user scrolled on. */
        if let Some(current) = t.cons[parent].focus_head.front().copied() {
            let direction = if event.detail == crate::backend::XCB_BUTTON_SCROLL_UP
                || event.detail == crate::backend::XCB_BUTTON_SCROLL_LEFT
            {
                Position::BEFORE
            } else {
                Position::AFTER
            };
            let next = get_tree_next_sibling(t, current, direction);
            let to_focus = con_descend_focused(t, next.unwrap_or(current));
            con_activate(t, b, to_focus);
        }

        allow_replay_pointer(t, b, x, event.time);
        return;
    }

    /* 2: floating modifier pressed, initiate a drag */
    if mod_pressed
        && is_left_click
        && floatingcon.is_none()
        && matches!(
            t.config.tiling_drag,
            crate::data::tiling_drag_t::TILING_DRAG_MODIFIER
                | crate::data::tiling_drag_t::TILING_DRAG_MODIFIER_OR_TITLEBAR
        )
        && crate::tiling_drag::has_drop_targets(t)
    {
        let use_threshold = !mod_pressed;
        crate::tiling_drag::tiling_drag(t, b, x, a, ctx, con, event, use_threshold);
        allow_replay_pointer(t, b, x, event.time);
        return;
    }

    /* 3: focus this con or one of its children. */
    let mut con_to_focus = con;
    if in_stacked && dest == click_destination_t::CLICK_DECORATION {
        /* If the container is a tab/stacked container and the click happened
         * on a tab, switch to the tab. If the tab contents were already
         * focused, focus the tab container itself. If the tab container was
         * already focused, cycle back to focusing the tab contents. */
        let focused_con = t.focused;
        if was_focused || !con_has_parent(t, focused_con, con) {
            while let Some(child) = t.cons[con_to_focus].focus_head.front().copied() {
                con_to_focus = child;
            }
        }
    }
    if Some(ws) != focused_workspace {
        crate::workspace::workspace_show(t, b, ws);
    }
    con_activate(t, b, con_to_focus);

    /* 4: For floating containers, we also want to raise them on click.
     * We will skip handling events on floating cons in fullscreen mode */
    let fs = crate::con::con_get_fullscreen_covering_ws(t, Some(ws));
    if let Some(floatingcon) = floatingcon {
        if fs != Some(con) {
            /* 5: floating_modifier plus left mouse button drags */
            if mod_pressed && is_left_click {
                // C: src/click.c:285 floating_drag_window(floatingcon, event, false)
                crate::log::debuglog("floating drag due to floatingmodifier
");
                crate::floating::floating_drag_window(t, b, x, a, ctx, floatingcon, event, false);
                return;
            }

            /* 6: resize (floating) if this was a (left or right) click on the
             * left/right/bottom border, or a right click on the decoration.
             * also try resizing (tiling) if possible */
            if mod_pressed && is_right_click {
                crate::floating::floating_resize_window(t, b, x, a, ctx, floatingcon, proportional, event);
                return;
            }

            if (dest == click_destination_t::CLICK_BORDER
                || dest == click_destination_t::CLICK_DECORATION)
                && is_left_or_right_click
            {
                /* try tiling resize, but continue if it doesn't work */
                let use_threshold = dest == click_destination_t::CLICK_DECORATION && !was_focused;
                if tiling_resize(t, b, x, a, ctx, con, event, dest, use_threshold) {
                    allow_replay_pointer(t, b, x, event.time);
                    return;
                }
            }

            if dest == click_destination_t::CLICK_DECORATION && is_right_click {
                crate::floating::floating_resize_window(t, b, x, a, ctx, floatingcon, proportional, event);
                return;
            }

            if dest == click_destination_t::CLICK_BORDER && is_left_or_right_click {
                crate::floating::floating_resize_window(t, b, x, a, ctx, floatingcon, proportional, event);
                return;
            }

            /* 7: dragging, if this was a click on a decoration (which did not
             * lead to a resize) */
            if dest == click_destination_t::CLICK_DECORATION && is_left_click {
                crate::floating::floating_drag_window(t, b, x, a, ctx, floatingcon, event, !was_focused);
                return;
            }

            allow_replay_pointer(t, b, x, event.time);
            return;
        }
    }

    /* 8: floating modifier pressed, or click in titlebar, initiate a drag */
    if is_left_click
        && ((t.config.tiling_drag == crate::data::tiling_drag_t::TILING_DRAG_TITLEBAR
            && dest == click_destination_t::CLICK_DECORATION)
            || (t.config.tiling_drag
                == crate::data::tiling_drag_t::TILING_DRAG_MODIFIER_OR_TITLEBAR
                && (mod_pressed || dest == click_destination_t::CLICK_DECORATION)))
        && crate::tiling_drag::has_drop_targets(t)
    {
        allow_replay_pointer(t, b, x, event.time);
        let use_threshold = !mod_pressed;
        crate::tiling_drag::tiling_drag(t, b, x, a, ctx, con, event, use_threshold);
        return;
    }

    /* 9: floating modifier pressed, initiate a resize */
    if dest == click_destination_t::CLICK_INSIDE && mod_pressed && is_right_click {
        if floating_mod_on_tiled_client(t, b, x, a, ctx, con, event) {
            return;
        }
        /* Avoid propagating events to clients, since the user expects
         * $mod+click to be handled by i3. */
        x.allow_events(crate::backend::XCB_ALLOW_ASYNC_POINTER, event.time);
        x.flush();
        return;
    }
    /* 10: otherwise, check for border/decoration clicks and resize */
    if (dest == click_destination_t::CLICK_BORDER || dest == click_destination_t::CLICK_DECORATION)
        && is_left_or_right_click
    {
        let use_threshold = dest == click_destination_t::CLICK_DECORATION && !was_focused;
        tiling_resize(t, b, x, a, ctx, con, event, dest, use_threshold);
    }

    allow_replay_pointer(t, b, x, event.time);
}

/// C: src/click.c:372 handle_button_press
///
/// The button press X callback. This function determines whether the floating
/// modifier is pressed and where the user clicked (decoration, border, inside
/// the window).
///
/// Then, route_click is called on the appropriate con.
pub fn handle_button_press(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    a: &crate::xcb_util::Atoms,
    event: &button_event_t,
    button_release: bool,
    ctx: &mut EventContext,
) {
    // C: last_timestamp = event->time — the caller keeps last_timestamp.

    if con_by_window_id(t, event.event).is_some() {
        let con = con_by_window_id(t, event.event).unwrap();
        route_click(t, b, x, a, ctx, con, event, click_destination_t::CLICK_INSIDE);
        return;
    }

    let con = match con_by_frame_id(t, event.event) {
        Some(con) => con,
        None => {
            /* Run bindings on the root window as well, see #2097. We only run
             * it if --whole-window was set as that's the equivalent for a
             * normal window. */
            if ctx.root != 0 && event.event == ctx.root {
                let binding_event = crate::bindings::BindingEvent {
                    response_type: event.response_type,
                    state: event.state,
                    detail: u16::from(event.detail),
                };
                if let Some(index) = crate::bindings::get_binding_from_xcb_event(t, &binding_event) {
                    let whole_window = t.bindings.get(index).map(|b| b.whole_window).unwrap_or(false);
                    if whole_window {
                        if let Some(bindings) = ctx.bindings.as_deref_mut() {
                            let bind = t.bindings[index].clone();
                            let mut hooks = crate::ipc::IpcHooks;
                            let _ = crate::bindings::run_binding(
                                bindings, t, b, &mut hooks, &bind, None,
                            );
                        }
                    }
                }
            }

            /* If the root window is clicked, find the relevant output from the
             * click coordinates and focus the output's active workspace. */
            if ctx.root != 0 && event.event == ctx.root && !button_release {
                let outputs: Vec<ConId> = match t.croot {
                    Some(croot) => t.cons[croot].nodes_head.iter().copied().collect(),
                    None => Vec::new(),
                };
                for output in outputs {
                    if con_is_internal(t, output)
                        || !rect_contains(
                            t.cons[output].rect,
                            event.event_x as u32,
                            event.event_y as u32,
                        )
                    {
                        continue;
                    }

                    let content = match crate::output::output_get_content(t, output) {
                        Some(content) => content,
                        None => continue,
                    };
                    let ws = match t.cons[content].focus_head.front().copied() {
                        Some(ws) => ws,
                        None => continue,
                    };
                    let focused_ws = t.focused.and_then(|f| con_get_workspace(t, f));
                    if Some(ws) != focused_ws {
                        crate::workspace::workspace_show(t, b, ws);
                        tree_render(t, b);
                    }
                    return;
                }
                return;
            }

            crate::log::errorlog("Clicked into unknown window?!\n");
            x.allow_events(crate::backend::XCB_ALLOW_REPLAY_POINTER, event.time);
            x.flush();
            return;
        }
    };

    /* Check if the click was on the decoration of a child */
    if t.cons[con].window.is_some() {
        if rect_contains(t.cons[con].deco_rect, event.event_x as u32, event.event_y as u32) {
            route_click(t, b, x, a, ctx, con, event, click_destination_t::CLICK_DECORATION);
            return;
        }
    } else {
        let children: Vec<ConId> = t.cons[con].nodes_head.iter().rev().copied().collect();
        for child in children {
            if !rect_contains(t.cons[child].deco_rect, event.event_x as u32, event.event_y as u32) {
                continue;
            }

            route_click(t, b, x, a, ctx, child, event, click_destination_t::CLICK_DECORATION);
            return;
        }
    }

    if event.child != 0 {
        route_click(t, b, x, a, ctx, con, event, click_destination_t::CLICK_INSIDE);
        return;
    }

    route_click(t, b, x, a, ctx, con, event, click_destination_t::CLICK_BORDER);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::data::{output_name, BorderStyle, Output, Rect};
    use crate::xcb_util::{Atoms, RecordingConn};

    /// Builds croot -> output -> content -> workspace -> (w1, w2), the same
    /// shape src/randr.c creates at startup (one output, one workspace, two
    /// tiled windows).
    fn test_env() -> (Tree, ConId, ConId, ConId) {
        let mut t = Tree::new();
        let mut b = NullBackend;
        crate::tree::tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));
        let croot = t.croot.unwrap();

        let output = crate::con::con_new(&mut t, &mut b, Some(croot), None);
        t.cons[output].r#type = ConType::CT_OUTPUT;
        t.cons[output].layout = Layout::L_OUTPUT;
        t.cons[output].name = Some("fake-0".to_string());
        t.cons[output].rect = Rect::new(0, 0, 1280, 800);

        let mut output_data = Output::default();
        output_data.active = true;
        output_data.primary = true;
        output_data.rect = Rect::new(0, 0, 1280, 800);
        output_data.con = Some(output);
        output_data.names_head.push_back(output_name { name: "fake-0".to_string() });
        t.outputs.push_back(output_data);

        let content = crate::con::con_new(&mut t, &mut b, Some(output), None);
        t.cons[content].r#type = ConType::CT_CON;
        t.cons[content].layout = Layout::L_SPLITH;
        t.cons[content].rect = Rect::new(0, 0, 1280, 800);

        let ws = crate::con::con_new(&mut t, &mut b, Some(content), None);
        t.cons[ws].r#type = ConType::CT_WORKSPACE;
        t.cons[ws].layout = Layout::L_SPLITH;
        t.cons[ws].name = Some("1".to_string());
        t.cons[ws].rect = Rect::new(0, 0, 1280, 800);

        for (i, x) in [0u32, 640u32].iter().enumerate() {
            let w = crate::con::con_new(&mut t, &mut b, Some(ws), None);
            t.cons[w].r#type = ConType::CT_CON;
            t.cons[w].layout = Layout::L_SPLITH;
            t.cons[w].name = Some(format!("w{}", i));
            t.cons[w].rect = Rect::new(*x, 0, 640, 800);
            t.cons[w].deco_rect = Rect::new(*x, 0, 640, 18);
            t.cons[w].window_rect = Rect::new(*x, 18, 640, 780);
            t.cons[w].border_style = BorderStyle::BS_NORMAL;
            t.cons[w].border_width = 2;
            t.cons[w].percent = 0.5;
        }

        let w1 = t.cons[ws].nodes_head[0];
        let w2 = t.cons[ws].nodes_head[1];
        crate::con::con_focus(&mut t, &mut b, w1);
        (t, ws, w1, w2)
    }

    fn ctx<'a>() -> EventContext<'a> {
        EventContext {
            wm_sn: 0,
            root: 0x100,
            randr_base: -1,
            xkb_base: -1,
            shape_base: -1,
            shape_supported: true,
            bindings: None,
            xkb_factory: None,
            randr: None,
            ipc: None,
            last_timestamp: 0,
            disable_focus_follows_mouse: false,
            force_xinerama: false,
        }
    }

    fn press(event: u32, detail: u8, state: u16, x: i32, y: i32) -> button_event_t {
        button_event_t {
            event,
            child: 0,
            detail,
            state,
            event_x: x,
            event_y: y,
            root_x: x,
            root_y: y,
            time: 1234,
            response_type: crate::bindings::XCB_BUTTON_PRESS,
        }
    }

    fn has_log(x: &RecordingConn, needle: &str) -> bool {
        x.log.iter().any(|l| l.contains(needle))
    }

    #[test]
    fn titlebar_left_click_focuses_the_container() {
        // C: src/click.c:261-277 — step 3 focuses the clicked con; step 10
        // additionally tries a tiling resize for decoration clicks.
        let (mut t, _ws, w1, w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();
        assert_eq!(t.focused, Some(w1));

        route_click(
            &mut t,
            &mut b,
            &mut x,
            &a,
            &mut c,
            w2,
            &press(0, 1, 0, 700, 5),
            click_destination_t::CLICK_DECORATION,
        );

        assert_eq!(t.focused, Some(w2), "left click on the titlebar moves the focus");
        // C: allow_replay_pointer() = XCB_ALLOW_REPLAY_POINTER (2).
        assert!(has_log(&x, "allow_events(mode=2"), "the click is replayed to the app");
    }

    #[test]
    fn button_release_only_replays_the_pointer() {
        // C: src/click.c:200-203 — no default behavior for releases.
        let (mut t, _ws, w1, w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();
        let mut event = press(0, 1, 0, 700, 5);
        event.response_type = crate::bindings::XCB_BUTTON_RELEASE;

        route_click(&mut t, &mut b, &mut x, &a, &mut c, w2, &event, click_destination_t::CLICK_DECORATION);

        assert_eq!(t.focused, Some(w1), "a release must not move the focus");
        assert!(has_log(&x, "allow_events(mode=2"));
        assert!(!has_log(&x, "grab_pointer"), "a release never starts a drag");
    }

    #[test]
    fn dockarea_click_is_ignored() {
        // C: src/click.c:177-181 — dockarea cons must not be focused.
        let (mut t, _ws, _w1, _w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();

        let croot = t.croot.unwrap();
        let dock = crate::con::con_new(&mut t, &mut b, Some(croot), None);
        t.cons[dock].r#type = ConType::CT_DOCKAREA;
        t.cons[dock].rect = Rect::new(0, 780, 1280, 20);
        // con_attach() refuses a windowless con below a dockarea (C: con.c
        // asserts the same), so the dock child is attached to the workspace
        // first and re-parented by hand — route_click only reads the parent.
        let dock_con = crate::con::con_new(&mut t, &mut b, Some(_ws), None);
        t.cons[_ws].nodes_head.retain(|&c| c != dock_con);
        t.cons[_ws].focus_head.retain(|&c| c != dock_con);
        t.cons[dock].nodes_head.push_back(dock_con);
        t.cons[dock_con].parent = Some(dock);
        t.cons[dock_con].rect = Rect::new(0, 780, 1280, 20);
        t.cons[dock_con].deco_rect = Rect::new(0, 780, 1280, 20);

        let before = t.focused;
        route_click(
            &mut t,
            &mut b,
            &mut x,
            &a,
            &mut c,
            dock_con,
            &press(0, 1, 0, 10, 785),
            click_destination_t::CLICK_INSIDE,
        );
        assert_eq!(t.focused, before, "dock clicks are not focused");
        assert!(has_log(&x, "allow_events(mode=2"));
    }

    #[test]
    fn floating_modifier_left_click_on_titlebar_starts_a_drag() {
        // C: src/click.c:284-287 — $mod+left on the decoration of a floating
        // window calls floating_drag_window().
        let (mut t, _ws, _w1, w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();

        t.config.floating_modifier = 1;
        crate::floating::floating_enable(&mut t, &mut b, w2, false);

        route_click(
            &mut t,
            &mut b,
            &mut x,
            &a,
            &mut c,
            w2,
            &press(0, 1, 1, 700, 5),
            click_destination_t::CLICK_DECORATION,
        );

        assert!(has_log(&x, "grab_pointer"), "the drag grabs the pointer");
        assert!(con_inside_floating(&t, w2).is_some(), "the con stayed floating");
    }

    #[test]
    fn floating_modifier_right_click_on_floating_window_starts_a_resize() {
        // C: src/click.c:292-296 — $mod+right resizes a floating window.
        let (mut t, _ws, _w1, w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();

        t.config.floating_modifier = 1;
        crate::floating::floating_enable(&mut t, &mut b, w2, false);
        let before = t.cons[w2].rect;

        route_click(
            &mut t,
            &mut b,
            &mut x,
            &a,
            &mut c,
            w2,
            &press(0, 3, 1, 900, 500),
            click_destination_t::CLICK_INSIDE,
        );

        assert!(has_log(&x, "grab_pointer"), "the resize grabs the pointer");
        // The grab fails in the test double, so the rect is untouched.
        assert_eq!(t.cons[w2].rect, before);
    }

    #[test]
    fn scroll_on_tabbed_decoration_switches_the_tab() {
        // C: src/click.c:232-248 — scrolling on a tabbed/stacked decoration
        // activates the next sibling; button4/6 go BEFORE, button5/7 AFTER.
        let (mut t, ws, w1, w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();
        t.cons[ws].layout = Layout::L_TABBED;
        assert_eq!(t.focused, Some(w1));

        // button5 (scroll down) -> AFTER -> the second tab
        route_click(&mut t, &mut b, &mut x, &a, &mut c, w1, &press(0, 5, 0, 10, 5), click_destination_t::CLICK_DECORATION);
        assert_eq!(t.focused, Some(w2));

        // button4 (scroll up) -> BEFORE -> back to the first tab
        route_click(&mut t, &mut b, &mut x, &a, &mut c, w2, &press(0, 4, 0, 10, 5), click_destination_t::CLICK_DECORATION);
        assert_eq!(t.focused, Some(w1));
    }

    #[test]
    fn scroll_on_tiling_decoration_does_not_switch_workspaces() {
        // i3 4.25 has no handle_scroll(): wheel bindings are regular mouse
        // bindings (get_binding_from_xcb_event, src/click.c:185). Without a
        // binding, a scroll on a tiled decoration is only replayed.
        let (mut t, _ws, w1, _w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();
        let focused_before = t.focused;

        route_click(&mut t, &mut b, &mut x, &a, &mut c, w1, &press(0, 4, 0, 10, 5), click_destination_t::CLICK_DECORATION);
        assert_eq!(t.focused, focused_before);
        assert!(has_log(&x, "allow_events(mode=2"));
    }

    #[test]
    fn double_click_is_two_independent_clicks() {
        // i3 4.25 (src/click.c) has no 300 ms double-click window: a second
        // titlebar click is handled exactly like the first one. A floating
        // toggle is only reachable through a binding (bindsym button1 …).
        let (mut t, _ws, _w1, w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();

        for _ in 0..2 {
            route_click(&mut t, &mut b, &mut x, &a, &mut c, w2, &press(0, 1, 0, 700, 5), click_destination_t::CLICK_DECORATION);
        }

        assert_eq!(t.focused, Some(w2));
        assert!(con_inside_floating(&t, w2).is_none(), "no double click floating toggle in 4.25");
    }

    #[test]
    fn tiling_resize_border_click_starts_a_pointer_grab() {
        // C: src/click.c:127-156 + 24-74 — a click on the decoration of one of
        // two tiled cons starts a graphical resize against its neighbor.
        let (mut t, ws, _w1, w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();
        // A decoration click always resizes against the TOP border, so the
        // participants must be in a vertical split (C: src/click.c:133-134).
        t.cons[ws].layout = Layout::L_SPLITV;

        let started = tiling_resize(
            &mut t,
            &mut b,
            &mut x,
            &a,
            &mut c,
            w2,
            &press(0, 1, 0, 320, 5),
            click_destination_t::CLICK_DECORATION,
            true,
        );
        assert!(started, "the decoration click resizes against the neighbor");
        assert!(has_log(&x, "grab_pointer"));
        // The test double fails the pointer grab, so drag_pointer() aborts
        // before the keyboard grab (C: src/drag.c:226-236).
        assert_eq!(
            x.log.iter().filter(|l| l.contains("grab_pointer")).count(),
            1
        );
    }

    #[test]
    fn tiling_resize_without_neighbor_does_nothing() {
        let (mut t, _ws, w1, w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();

        // A single window has no resize participant.
        crate::tree::tree_close_internal(
            &mut t,
            &mut b,
            w2,
            crate::data::KillWindow::DONT_KILL_WINDOW,
            false,
        );
        let started = tiling_resize(
            &mut t,
            &mut b,
            &mut x,
            &a,
            &mut c,
            w1,
            &press(0, 1, 0, 320, 5),
            click_destination_t::CLICK_DECORATION,
            true,
        );
        assert!(!started);
        assert!(!has_log(&x, "grab_pointer"));
    }

    #[test]
    fn left_click_inside_does_not_start_a_drag() {
        // C: src/click.c has no drag for plain inside clicks; they only focus
        // and are replayed.
        let (mut t, _ws, _w1, w2) = test_env();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = Atoms::default();
        let mut c = ctx();

        route_click(&mut t, &mut b, &mut x, &a, &mut c, w2, &press(0, 1, 0, 700, 400), click_destination_t::CLICK_INSIDE);
        assert_eq!(t.focused, Some(w2));
        assert!(!has_log(&x, "grab_pointer"));
        assert!(has_log(&x, "allow_events(mode=2"));
    }
}
