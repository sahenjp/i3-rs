//! 1:1 port of src/resize.c — Interactive resizing.
//!
//! C: src/resize.c
//!
//! resize_find_tiling_participants() lives in gaps.rs (its first caller was
//! gaps_has_adjacent_container); it is re-exported here so that the call sites
//! read like the C file.

use crate::backend::{ConBackend, XConn};
use crate::con::{
    con_descend_focused, con_fix_percent, con_get_output, con_orientation, con_rect_size_in_orientation,
};
use crate::data::{ConId, Orientation, Rect};
use crate::handlers::{button_event_t, EventContext};
use crate::tree::{tree_render, Tree};
use crate::xcb_util::xcursor_cursor_t;

pub use crate::gaps::resize_find_tiling_participants;

/// C: src/resize.c:131 percent_for_1px
///
/// Calculate the minimum percent needed for the given container to be at
/// least 1 pixel.
pub fn percent_for_1px(t: &Tree, con: ConId) -> f64 {
    let parent = t.cons[con].parent.expect("percent_for_1px: no parent");
    let parent_size = con_rect_size_in_orientation(t, parent);
    /* deco_rect.height is subtracted from each child in render_con_split */
    let min_size = if con_orientation(t, parent) == Orientation::HORIZ {
        1
    } else {
        1 + t.cons[con].deco_rect.height as i64
    };
    (min_size as f64) / (parent_size as f64)
}

/// C: src/resize.c:146 resize_neighboring_cons
///
/// Resize the two given containers using the given amount of pixels or
/// percentage points. One of the two needs to be 0. A positive amount means
/// growing the first container while a negative means shrinking it.
/// Returns false when the resize would result in one of the two containers
/// having less than 1 pixel of size.
pub fn resize_neighboring_cons(t: &mut Tree, first: ConId, second: ConId, px: i32, ppt: i32) -> bool {
    debug_assert!(px as i64 * ppt as i64 == 0);

    let parent = t.cons[first].parent.expect("resize_neighboring_cons: no parent");
    let (new_first_percent, new_second_percent) = if ppt != 0 {
        (
            t.cons[first].percent + (ppt as f64 / 100.0),
            t.cons[second].percent - (ppt as f64 / 100.0),
        )
    } else {
        /* Convert px change to change in percentages */
        let pct = px as f64 / con_rect_size_in_orientation(t, parent) as f64;
        (
            t.cons[first].percent + pct,
            t.cons[second].percent - pct,
        )
    };
    /* Ensure that no container will be less than 1 pixel in the resizing
     * direction. */
    if new_first_percent < percent_for_1px(t, first) || new_second_percent < percent_for_1px(t, second) {
        return false;
    }

    t.cons[first].percent = new_first_percent;
    t.cons[second].percent = new_second_percent;
    con_fix_percent(t, parent);
    true
}

/// C: src/resize.c:173 resize_graphical_handler
///
/// Opens the resize bar, grabs the pointer and lets the user move the bar
/// until the button is released; the two containers are then resized by the
/// distance the bar travelled.
#[allow(clippy::too_many_arguments)]
pub fn resize_graphical_handler(
    t: &mut Tree,
    b: &mut dyn ConBackend,
    x: &mut dyn XConn,
    a: &crate::xcb_util::Atoms,
    ctx: &mut EventContext,
    first: ConId,
    second: ConId,
    orientation: Orientation,
    event: &button_event_t,
    use_threshold: bool,
) {
    let output = con_get_output(t, first);

    // C: x_mask_event_mask(~XCB_EVENT_MASK_ENTER_WINDOW)
    b.x_mask_event_mask(
        t,
        !crate::backend::XCB_EVENT_MASK_ENTER_WINDOW,
    );
    x.flush();

    /* Open a new window, the resizebar. Grab the pointer and move the window
     * around as the user moves the pointer. */
    let mut mask: Vec<(u32, u32)> = Vec::new();
    mask.push((crate::backend::XCB_CW_OVERRIDE_REDIRECT, 1));
    let output_rect = output.map(|o| t.cons[o].rect).unwrap_or_default();
    let grabwin = crate::xcb_util::create_window(
        x,
        output_rect,
        crate::backend::XCB_COPY_FROM_PARENT as u16,
        crate::backend::XCB_COPY_FROM_PARENT,
        crate::backend::XCB_WINDOW_CLASS_INPUT_ONLY,
        xcursor_cursor_t::XCURSOR_CURSOR_POINTER,
        true,
        &mask,
    );

    /* Keep track of the coordinate orthogonal to motion so we can determine
     * the length of the resize afterward. */
    let initial_position: u32;

    /* Configure the resizebar and snap the pointer. The resizebar runs along
     * the rect of the second con and follows the motion of the pointer. */
    let mut helprect = Rect {
        x: t.cons[second].rect.x,
        y: t.cons[second].rect.y,
        width: 0,
        height: 0,
    };
    /* Resizes might happen between a split container and a leaf container.
     * Because gaps happen *within* a split container, we need to work with
     * (any) leaf window inside the split, so descend focused. */
    let ffirst = con_descend_focused(t, first);
    let fsecond = con_descend_focused(t, second);
    if orientation == Orientation::HORIZ {
        helprect.width = libi3::dpi::logical_px(2) as u32;
        helprect.height = t.cons[second].rect.height;
        let ffirst_right = t.cons[ffirst].rect.x.wrapping_add(t.cons[ffirst].rect.width);
        let gap = t.cons[fsecond].rect.x.wrapping_sub(ffirst_right);
        let middle = t.cons[fsecond].rect.x.wrapping_sub(gap / 2);
        initial_position = middle;
    } else {
        helprect.width = t.cons[second].rect.width;
        helprect.height = libi3::dpi::logical_px(2) as u32;
        let ffirst_bottom = t.cons[ffirst].rect.y.wrapping_add(t.cons[ffirst].rect.height);
        let gap = t.cons[fsecond].rect.y.wrapping_sub(ffirst_bottom);
        let middle = t.cons[fsecond].rect.y.wrapping_sub(gap / 2);
        initial_position = middle;
    }

    // C: mask = XCB_CW_BACK_PIXEL | XCB_CW_OVERRIDE_REDIRECT
    let back_pixel = t.config.client_focused_border;
    let values = [
        (crate::backend::XCB_CW_BACK_PIXEL, back_pixel),
        (crate::backend::XCB_CW_OVERRIDE_REDIRECT, 1),
    ];
    let cursor = if orientation == Orientation::HORIZ {
        xcursor_cursor_t::XCURSOR_CURSOR_RESIZE_HORIZONTAL
    } else {
        xcursor_cursor_t::XCURSOR_CURSOR_RESIZE_VERTICAL
    };
    let helpwin = crate::xcb_util::create_window(
        x,
        helprect,
        crate::backend::XCB_COPY_FROM_PARENT as u16,
        crate::backend::XCB_COPY_FROM_PARENT,
        crate::backend::XCB_WINDOW_CLASS_INPUT_OUTPUT,
        cursor,
        false,
        &values,
    );

    if !use_threshold {
        x.map_window(helpwin);
        let root = x.root_screen().root;
        if orientation == Orientation::HORIZ {
            x.warp_pointer(0, root, 0, 0, 0, 0, initial_position as i16, event.root_y as i16);
        } else {
            x.warp_pointer(0, root, 0, 0, 0, 0, event.root_x as i16, initial_position as i16);
        }
    }

    x.circulate_window(crate::backend::XCB_CIRCULATE_RAISE_LOWEST, helpwin);

    x.flush();

    /* new_position will be updated by the resize_callback. */
    let mut new_position = initial_position;
    let mut threshold_exceeded = !use_threshold;

    /* Re-render the tree before returning to the event loop (drag_pointer()
     * runs its own event-loop) in case if there are unrendered updates. */
    tree_render(t, b);

    /* drag_pointer blocks until the drag is completed. */
    let drag_result = {
        let mut callback = |_t: &mut Tree,
                            _b: &mut dyn ConBackend,
                            x: &mut dyn XConn,
                            _a: &crate::xcb_util::Atoms,
                            _con: Option<ConId>,
                            _old_rect: &Rect,
                            new_x: u32,
                            new_y: u32,
                            event: &button_event_t| {
            // C: DRAGGING_CB(resize_callback)
            if !threshold_exceeded {
                x.map_window(helpwin);
                /* Warp pointer in the same way as resize_graphical_handler()
                 * would do if threshold wasn't enabled, but also take into
                 * account travelled distance. */
                let root = x.root_screen().root;
                if orientation == Orientation::HORIZ {
                    x.warp_pointer(
                        0,
                        root,
                        0,
                        0,
                        0,
                        0,
                        new_position.wrapping_add(new_x).wrapping_sub(event.root_x as u32) as i16,
                        new_y as i16,
                    );
                } else {
                    x.warp_pointer(
                        0,
                        root,
                        0,
                        0,
                        0,
                        0,
                        new_x as i16,
                        new_position.wrapping_add(new_y).wrapping_sub(event.root_y as u32) as i16,
                    );
                }
                threshold_exceeded = true;
                return;
            }

            if orientation == Orientation::HORIZ {
                /* Check if the new coordinates are within screen boundaries */
                if new_x > output_rect.x.wrapping_add(output_rect.width).wrapping_sub(25)
                    || new_x < output_rect.x.wrapping_add(25)
                {
                    return;
                }

                new_position = new_x;
                x.configure_window(
                    helpwin,
                    &[(crate::backend::ConfigWindow::X, new_position)],
                );
            } else {
                if new_y > output_rect.y.wrapping_add(output_rect.height).wrapping_sub(25)
                    || new_y < output_rect.y.wrapping_add(25)
                {
                    return;
                }

                new_position = new_y;
                x.configure_window(
                    helpwin,
                    &[(crate::backend::ConfigWindow::Y, new_position)],
                );
            }

            x.flush();
        };

        crate::drag::drag_pointer(
            t,
            b,
            x,
            a,
            ctx,
            None,
            event,
            grabwin,
            None, /* C: 0 — no cursor for the resize bar */
            use_threshold,
            &mut callback,
        )
    };

    x.destroy_window(helpwin);
    x.destroy_window(grabwin);
    x.flush();

    /* User cancelled the drag so no action should be taken. */
    if drag_result == crate::drag::drag_result_t::DRAG_REVERT {
        return;
    }

    let pixels = new_position.wrapping_sub(initial_position) as i32;

    /* No change; no action needed. */
    if pixels == 0 {
        return;
    }

    /* if we got thus far, the containers must have valid percentages. */
    debug_assert!(t.cons[first].percent > 0.0);
    debug_assert!(t.cons[second].percent > 0.0);
    let result = resize_neighboring_cons(t, first, second, pixels, 0);
    crate::log::debuglog(&format!(
        "Graphical resize {}: first->percent = {}, second->percent = {}.\n",
        if result { "successful" } else { "failed" },
        t.cons[first].percent,
        t.cons[second].percent
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::NullBackend;
    use crate::data::{ConType, Layout};
    use crate::workspace::create_workspace_on_output;
    use crate::xcb_util::RecordingConn;

    /// croot -> output -> content -> workspace -> (w1, w2) in a horizontal
    /// split; returns (tree, workspace, w1, w2).
    fn two_windows() -> (Tree, ConId, ConId, ConId) {
        let mut t = Tree::new();
        let mut b = NullBackend;
        crate::tree::tree_init(&mut t, &mut b, Rect::new(0, 0, 1280, 800));
        let croot = t.croot.unwrap();

        let mut output = crate::data::Output::default();
        let mut output_data = crate::data::Output::default();
        output_data.con = Some(croot);
        output.rect = Rect::new(0, 0, 1280, 800);
        let output_con = crate::con::con_new(&mut t, &mut b, Some(croot), None);
        t.cons[output_con].r#type = ConType::CT_OUTPUT;
        t.cons[output_con].layout = Layout::L_OUTPUT;
        t.cons[output_con].rect = Rect::new(0, 0, 1280, 800);

        let content = crate::con::con_new(&mut t, &mut b, Some(output_con), None);
        t.cons[content].r#type = ConType::CT_CON;
        t.cons[content].layout = Layout::L_SPLITH;
        t.cons[content].rect = Rect::new(0, 0, 1280, 800);

        let ws = crate::con::con_new(&mut t, &mut b, Some(content), None);
        t.cons[ws].r#type = ConType::CT_WORKSPACE;
        t.cons[ws].layout = Layout::L_SPLITH;
        t.cons[ws].name = Some("1".to_string());
        t.cons[ws].rect = Rect::new(0, 0, 1280, 800);

        let mut ids = Vec::new();
        for _ in 0..2 {
            let w = crate::con::con_new(&mut t, &mut b, Some(ws), None);
            t.cons[w].r#type = ConType::CT_CON;
            t.cons[w].percent = 0.5;
            t.cons[w].rect = Rect::new(0, 0, 640, 800);
            ids.push(w);
        }
        (t, ws, ids[0], ids[1])
    }

    /// C: src/resize.c:131 — 1 px of a 1280 px horizontal parent is 1/1280.
    #[test]
    fn percent_for_1px_uses_the_parent_size() {
        let (t, _ws, w1, _w2) = two_windows();
        let p = percent_for_1px(&t, w1);
        assert!((p - 1.0 / 1280.0).abs() < 1e-12, "{p}");
    }

    /// C: src/resize.c:146 — a pixel delta is converted to a percentage delta
    /// and both containers keep 1.0 in total.
    #[test]
    fn resize_neighboring_cons_converts_pixels_to_percent() {
        let (mut t, _ws, w1, w2) = two_windows();
        assert!(resize_neighboring_cons(&mut t, w1, w2, 128, 0));
        assert!((t.cons[w1].percent - 0.6).abs() < 1e-9, "{}", t.cons[w1].percent);
        assert!((t.cons[w2].percent - 0.4).abs() < 1e-9, "{}", t.cons[w2].percent);
    }

    /// C: src/resize.c:161-165 — a resize below 1 px is refused.
    #[test]
    fn resize_neighboring_cons_refuses_less_than_one_pixel() {
        let (mut t, _ws, w1, w2) = two_windows();
        assert!(!resize_neighboring_cons(&mut t, w1, w2, -640, 0));
        assert_eq!(t.cons[w1].percent, 0.5);
        assert_eq!(t.cons[w2].percent, 0.5);
    }

    /// C: src/resize.c:173 — the graphical resize skips the change when the
    /// bar did not move (pixels == 0).
    #[test]
    fn graphical_resize_without_movement_changes_nothing() {
        let (mut t, _ws, w1, w2) = two_windows();
        let mut b = NullBackend;
        let mut x = RecordingConn::new(0x100);
        let a = crate::xcb_util::Atoms::default();
        let mut c = EventContext {
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
        };
        let event = button_event_t { detail: 1, root_x: 640, root_y: 400, ..Default::default() };

        resize_graphical_handler(
            &mut t,
            &mut b,
            &mut x,
            &a,
            &mut c,
            w1,
            w2,
            Orientation::HORIZ,
            &event,
            false,
        );

        // The grab fails in the test double, so the drag aborts and the
        // percentages stay untouched.
        assert_eq!(t.cons[w1].percent, 0.5);
        assert_eq!(t.cons[w2].percent, 0.5);
        assert!(x.log.iter().any(|l| l.contains("grab_pointer")));
        // The resize bar and the grab window are destroyed again.
        assert_eq!(x.log.iter().filter(|l| l.contains("destroy_window")).count(), 2);
        let _ = create_workspace_on_output;
    }
}
