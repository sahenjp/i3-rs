//! 1:1 port of src/render.c — renders (determines position/sizes) the
//! layout tree, updating the various rects. Needs to be pushed to X11 (see
//! x.c) to be visible.
//!
//! C: src/render.c

use crate::backend::ConBackend;
use crate::con::{
    con_border_style_rect, con_find_transient_for_window, con_get_fullscreen_con,
    con_get_fullscreen_covering_ws, con_has_parent, con_is_internal, con_is_leaf,
    con_num_children, con_orientation, con_rect_size_in_orientation, ConId,
};
use crate::data::{BorderStyle, ConType, Direction, FullscreenMode, Layout, Rect};
use crate::gaps::{calculate_effective_gaps, gaps_has_adjacent_container, gaps_should_inset_con};
use crate::output::output_get_content;
use crate::tree::Tree;
use crate::util::{rect_add, rect_sanitize_dimensions};
use crate::workspace::workspace_is_visible;

/// C: include/render.h:19 `render_params`
///
/// This is used to keep a state to pass around when rendering a con in
/// render_con().
#[derive(Debug, Clone)]
pub struct render_params {
    /// A copy of the coordinates of the container which is being rendered.
    pub x: i32,
    pub y: i32,
    /// The computed height for decorations.
    pub deco_height: i32,
    /// Container rect, subtract container border. This is the actually usable
    /// space inside this container for clients.
    pub rect: Rect,
    /// The number of children of the container which is being rendered.
    pub children: i32,
    /// A precalculated list of sizes of each child.
    pub sizes: Option<Vec<i32>>,
}

/// C: src/render.c:33 `render_deco_height`
///
/// Returns the height for the decorations.
pub fn render_deco_height(t: &Tree) -> i32 {
    let mut deco_height = t.config.font_height as i32 + 4;
    if t.config.font_height & 0x01 != 0 {
        deco_height += 1;
    }
    deco_height
}

/// C: src/render.c:43 `render_con`
///
/// "Renders" the given container (and its children), meaning that all rects
/// are updated correctly. Note that this function does not call any xcb_*
/// functions, so the changes are completely done in memory only (and
/// side-effect free). As soon as you call x_push_changes(), the changes will
/// be updated in X11.
// C: src/render.c:82 'inset.height = 0;' resets the local inset after the
// last use; the C line is kept 1:1 although the value is never read again.
#[allow(unused_assignments)]
pub fn render_con(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    // C 4.25.1: a workspace which is not the visible one on its output is not
    // rendered at all (verified against the baseline binary: after switching
    // to another workspace its render_con DLOG lines stop and the client
    // windows of the old workspace become unmapped). Because tree_render()
    // ran mark_unmapped() first, such a workspace and its cons stay
    // `mapped == false`, so x_push_node() sets `unmap_now` and
    // x_push_node_unmaps() really unmaps them.
    if t.cons[con].r#type == ConType::CT_WORKSPACE && !workspace_is_visible(t, con) {
        return;
    }

    let mut params = render_params {
        rect: t.cons[con].rect,
        x: t.cons[con].rect.x as i32,
        y: t.cons[con].rect.y as i32,
        children: con_num_children(t, con) as i32,
        deco_height: 0,
        sizes: None,
    };

    if t.cons[con].r#type == ConType::CT_WORKSPACE {
        let gaps = calculate_effective_gaps(t, con);
        let inset = Rect::new(
            gaps.left as u32,
            gaps.top as u32,
            (-(gaps.left.wrapping_add(gaps.right))) as u32,
            (-(gaps.top.wrapping_add(gaps.bottom))) as u32,
        );
        t.cons[con].rect = rect_add(t.cons[con].rect, inset);
        params.rect = rect_add(params.rect, inset);
        params.x += gaps.left;
        params.y += gaps.top;
    }

    if gaps_should_inset_con(t, con, params.children) {
        let gaps = calculate_effective_gaps(t, con);
        let half = gaps.inner / 2;
        let mut inset = Rect::new(
            if gaps_has_adjacent_container(t, con, Direction::D_LEFT) { half as u32 } else { gaps.inner as u32 },
            if gaps_has_adjacent_container(t, con, Direction::D_UP) { half as u32 } else { gaps.inner as u32 },
            if gaps_has_adjacent_container(t, con, Direction::D_RIGHT) { (-(half)) as u32 } else { (-(gaps.inner)) as u32 },
            if gaps_has_adjacent_container(t, con, Direction::D_DOWN) { (-(half)) as u32 } else { (-(gaps.inner)) as u32 },
        );
        inset.width = inset.width.wrapping_sub(inset.x);
        inset.height = inset.height.wrapping_sub(inset.y);

        if t.cons[con].fullscreen_mode == FullscreenMode::CF_NONE {
            params.rect = rect_add(params.rect, inset);
            t.cons[con].rect = rect_add(t.cons[con].rect, inset);
        }
        // C: src/render.c:82 'inset.height = 0;' — kept 1:1, the value is not read again.
        inset.height = 0;

        params.x = t.cons[con].rect.x as i32;
        params.y = t.cons[con].rect.y as i32;
    }

    t.cons[con].mapped = true;

    /* if this container contains a window, set the coordinates */
    if t.cons[con].window.is_some() {
        /* depending on the border style, the rect of the child window
         * needs to be smaller */
        let mut inset = Rect::new(0, 0, t.cons[con].rect.width, t.cons[con].rect.height);
        if t.cons[con].fullscreen_mode == FullscreenMode::CF_NONE {
            let bsr = con_border_style_rect(t, con);
            inset = rect_add(inset, bsr);
        }

        /* Obey x11 border */
        inset.width = inset.width.wrapping_sub((2 * t.cons[con].border_width) as u32);
        inset.height = inset.height.wrapping_sub((2 * t.cons[con].border_width) as u32);

        inset = rect_sanitize_dimensions(inset);
        t.cons[con].window_rect = inset;
    }

    /* Check for fullscreen nodes */
    let mut fullscreen: Option<ConId> = None;
    if t.cons[con].r#type != ConType::CT_OUTPUT {
        let mode = if t.cons[con].r#type == ConType::CT_ROOT { FullscreenMode::CF_GLOBAL } else { FullscreenMode::CF_OUTPUT };
        fullscreen = con_get_fullscreen_con(t, con, mode);
    }
    if let Some(fullscreen) = fullscreen {
        t.cons[fullscreen].rect = params.rect;
        b.x_raise_con(t, fullscreen);
        render_con(t, b, fullscreen);
        /* Fullscreen containers are either global (underneath the CT_ROOT
         * container) or per-output (underneath the CT_CONTENT container). For
         * global fullscreen containers, we cannot abort rendering here yet,
         * because the floating windows (with popup_during_fullscreen smart)
         * have not yet been rendered (see the CT_ROOT code path below). See
         * also https://bugs.i3wm.org/1393 */
        if t.cons[con].r#type != ConType::CT_ROOT {
            /* return (free_params) */
            return;
        }
    }

    /* find the height for the decorations */
    params.deco_height = render_deco_height(t);

    /* precalculate the sizes to be able to correct rounding errors */
    params.sizes = precalculate_sizes(t, con, &params);

    if t.cons[con].layout == Layout::L_OUTPUT {
        /* Skip i3-internal outputs */
        if con_is_internal(t, con) {
            /* goto free_params */
            return;
        }
        render_output(t, b, con);
    } else if t.cons[con].r#type == ConType::CT_ROOT {
        render_root(t, b, con, fullscreen);
    } else {
        let mut i = 0usize;
        let children: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
        for child in children {
            assert!(params.children > 0);

            if t.cons[con].layout == Layout::L_SPLITH || t.cons[con].layout == Layout::L_SPLITV {
                render_con_split(t, con, child, &mut params, i);
            } else if t.cons[con].layout == Layout::L_STACKED {
                render_con_stacked(t, con, child, &mut params, i);
            } else if t.cons[con].layout == Layout::L_TABBED {
                render_con_tabbed(t, con, child, &mut params, i);
            } else if t.cons[con].layout == Layout::L_DOCKAREA {
                render_con_dockarea(t, con, child, &mut params);
            }

            let rect = rect_sanitize_dimensions(t.cons[child].rect);
            t.cons[child].rect = rect;

            b.x_raise_con(t, child);
            render_con(t, b, child);

            /* render_con_split() sets the deco_rect width based on the rect
             * width, but the render_con() call updates the rect width by
             * applying gaps, so we need to update deco_rect. */
            if t.cons[con].layout == Layout::L_SPLITH || t.cons[con].layout == Layout::L_SPLITV {
                if con_is_leaf(t, child) {
                    if t.cons[child].border_style == BorderStyle::BS_NORMAL {
                        t.cons[child].deco_rect.width = t.cons[child].rect.width;
                    }
                }
            }

            i += 1;
        }

        /* in a stacking or tabbed container, we ensure the focused client is raised */
        if t.cons[con].layout == Layout::L_STACKED || t.cons[con].layout == Layout::L_TABBED {
            for &child in t.cons[con].focus_head.iter().rev() {
                b.x_raise_con(t, child);
            }
            if let Some(&child) = t.cons[con].focus_head.front() {
                /* By rendering the stacked container again, we handle the case
                 * that we have a non-leaf-container inside the stack. In that
                 * case, the children of the non-leaf-container need to be
                 * raised as well. */
                render_con(t, b, child);
            }

            if params.children != 1 {
                /* Raise the stack con itself. This will put the stack
                 * decoration on top of every stack window. That way, when a
                 * new window is opened in the stack, the old window will not
                 * obscure part of the decoration (it's unmapped afterwards). */
                b.x_raise_con(t, con);
            }
        }
    }

    /* free_params: params.sizes is owned here and simply dropped. */
}

/// C: src/render.c:221 `precalculate_sizes` (static)
fn precalculate_sizes(t: &Tree, con: ConId, p: &render_params) -> Option<Vec<i32>> {
    if (t.cons[con].layout != Layout::L_SPLITH && t.cons[con].layout != Layout::L_SPLITV) || p.children <= 0 {
        return None;
    }

    let mut sizes: Vec<i32> = vec![0; p.children as usize];
    let mut i = 0usize;
    let mut assigned: i64 = 0;
    let total = con_rect_size_in_orientation(t, con) as i64;
    let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in nodes {
        let percentage = if t.cons[child].percent > 0.0 { t.cons[child].percent } else { 1.0 / p.children as f64 };
        let size = (percentage * total as f64).round() as i64;
        assigned += size;
        sizes[i] = size as i32;
        i += 1;
    }
    let signal: i64 = if assigned < total { 1 } else { -1 };
    while assigned != total {
        for j in 0..(p.children as usize) {
            if assigned == total {
                break;
            }
            sizes[j] += signal as i32;
            assigned += signal;
        }
    }

    Some(sizes)
}

/// C: src/render.c:251 `fullscreen_blocks_floating_render` (static)
fn fullscreen_blocks_floating_render(t: &Tree, fullscreen: Option<ConId>, floating: ConId) -> bool {
    use crate::data::PopupDuringFullscreen;
    let fullscreen = match fullscreen {
        None => return false,
        Some(f) => f,
    };
    /* Don't render floating windows when there is a fullscreen window on that
     * workspace. Necessary to make floating fullscreen work correctly (ticket
     * #564). Exception to the above rule: popup_during_fullscreen smart|all. */
    match t.config.popup_during_fullscreen {
        PopupDuringFullscreen::PDF_LEAVE_FULLSCREEN | PopupDuringFullscreen::PDF_IGNORE => true,
        PopupDuringFullscreen::PDF_SMART => {
            if t.cons[fullscreen].window.is_none() {
                return true;
            }
            let fw = t.cons[fullscreen].window.expect("checked above");
            let fs_window_id = t.windows[fw].id;
            !con_find_transient_for_window(t, con_descend_focused_pub(t, floating), fs_window_id)
        }
        PopupDuringFullscreen::PDF_ALL => con_has_parent(t, Some(fullscreen), floating),
    }
}

/// con_descend_focused re-export for render.rs use (C calls the con.c one).
fn con_descend_focused_pub(t: &Tree, con: ConId) -> ConId {
    crate::con::con_descend_focused(t, con)
}

/// C: src/render.c:267 `render_root` (static)
fn render_root(t: &mut Tree, b: &mut dyn ConBackend, con: ConId, fullscreen: Option<ConId>) {
    if fullscreen.is_none() {
        let outputs: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
        for output in outputs {
            render_con(t, b, output);
        }
    }

    /* We need to render floating windows after rendering all outputs'
     * tiling windows because they need to be on top of *every* output at
     * all times. This is important when the user places floating
     * windows/containers so that they overlap on another output. */
    let outputs: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for output in outputs {
        if con_is_internal(t, output) {
            continue;
        }
        /* Get the active workspace of that output */
        let content = match output_get_content(t, output) {
            None => continue,
            Some(c) => c,
        };
        if t.cons[content].focus_head.is_empty() {
            /* Skipping this output because it is currently being destroyed. */
            continue;
        }
        let workspace = t.cons[content].focus_head[0];
        let fullscreen = con_get_fullscreen_covering_ws(t, Some(workspace));
        let children: Vec<ConId> = t.cons[workspace].floating_head.iter().copied().collect();
        for child in children {
            if fullscreen_blocks_floating_render(t, fullscreen, child) {
                continue;
            }
            b.x_raise_con(t, child);
            render_con(t, b, child);
        }
    }
}

/// C: src/render.c:307 `render_output` (static)
///
/// Renders a container with layout L_OUTPUT. In this layout, all CT_DOCKAREAs
/// get the height of their content and the remaining CT_CON gets the rest.
fn render_output(t: &mut Tree, b: &mut dyn ConBackend, con: ConId) {
    let x = t.cons[con].rect.x;
    let mut y = t.cons[con].rect.y;
    let mut height = t.cons[con].rect.height;

    /* Find the content container and ensure that there is exactly one. Also
     * check for any non-CT_DOCKAREA clients. */
    let mut content: Option<ConId> = None;
    let nodes: Vec<ConId> = t.cons[con].nodes_head.iter().copied().collect();
    for child in &nodes {
        if t.cons[*child].r#type == ConType::CT_CON {
            if content.is_some() {
                panic!("More than one CT_CON on output container");
            }
            content = Some(*child);
        } else if t.cons[*child].r#type != ConType::CT_DOCKAREA {
            panic!("Child of type {:?} is inside the OUTPUT con", t.cons[*child].r#type);
        }
    }

    let content = match content {
        None => {
            /* Skipping this output because it is currently being destroyed. */
            return;
        }
        Some(c) => c,
    };

    /* We need to find out if there is a fullscreen con on the current workspace
     * and take the short-cut to render it directly (the user does not want to
     * see the dockareas in that case) */
    let ws = match con_get_fullscreen_con(t, content, FullscreenMode::CF_OUTPUT) {
        None => {
            /* Skipping this output because it is currently being destroyed. */
            return;
        }
        Some(ws) => ws,
    };
    if let Some(fullscreen) = con_get_fullscreen_con(t, ws, FullscreenMode::CF_OUTPUT) {
        t.cons[fullscreen].rect = t.cons[con].rect;
        b.x_raise_con(t, fullscreen);
        render_con(t, b, fullscreen);
        return;
    }

    /* First pass: determine the height of all CT_DOCKAREAs (the sum of their
     * children) and figure out how many pixels we have left for the rest */
    for child in &nodes {
        if t.cons[*child].r#type != ConType::CT_DOCKAREA {
            continue;
        }

        t.cons[*child].rect.height = 0;
        let dockchildren: Vec<ConId> = t.cons[*child].nodes_head.iter().copied().collect();
        for dockchild in dockchildren {
            t.cons[*child].rect.height = t.cons[*child].rect.height.wrapping_add(t.cons[dockchild].geometry.height);
        }

        height = height.wrapping_sub(t.cons[*child].rect.height);
    }

    /* Second pass: Set the widths/heights */
    for child in nodes {
        if t.cons[child].r#type == ConType::CT_CON {
            t.cons[child].rect.x = x;
            t.cons[child].rect.y = y;
            t.cons[child].rect.width = t.cons[con].rect.width;
            t.cons[child].rect.height = height;
        }

        t.cons[child].rect.x = x;
        t.cons[child].rect.y = y;
        t.cons[child].rect.width = t.cons[con].rect.width;

        t.cons[child].deco_rect.x = 0;
        t.cons[child].deco_rect.y = 0;
        t.cons[child].deco_rect.width = 0;
        t.cons[child].deco_rect.height = 0;

        y = y.wrapping_add(t.cons[child].rect.height);

        b.x_raise_con(t, child);
        render_con(t, b, child);
        let _ = x;
    }
}

/// C: src/render.c:370 `render_con_split` (static)
fn render_con_split(t: &mut Tree, con: ConId, child: ConId, p: &mut render_params, i: usize) {
    assert!(t.cons[con].layout == Layout::L_SPLITH || t.cons[con].layout == Layout::L_SPLITV);

    let sizes = p.sizes.as_ref().map(|s| s[i]).unwrap_or(0);
    if t.cons[con].layout == Layout::L_SPLITH {
        t.cons[child].rect.x = p.x as u32;
        t.cons[child].rect.y = p.y as u32;
        t.cons[child].rect.width = sizes as u32;
        t.cons[child].rect.height = p.rect.height;
        p.x += sizes;
    } else {
        t.cons[child].rect.x = p.x as u32;
        t.cons[child].rect.y = p.y as u32;
        t.cons[child].rect.width = p.rect.width;
        t.cons[child].rect.height = sizes as u32;
        p.y += sizes;
    }

    /* first we have the decoration, if this is a leaf node */
    if con_is_leaf(t, child) {
        if t.cons[child].border_style == BorderStyle::BS_NORMAL {
            /* TODO: make a function for relative coords? */
            t.cons[child].deco_rect.x = 0;
            t.cons[child].deco_rect.y = 0;

            t.cons[child].deco_rect.width = t.cons[child].rect.width;
            t.cons[child].deco_rect.height = p.deco_height as u32;
        } else {
            t.cons[child].deco_rect.x = 0;
            t.cons[child].deco_rect.y = 0;
            t.cons[child].deco_rect.width = 0;
            t.cons[child].deco_rect.height = 0;
        }
    }
}

/// C: src/render.c:398 `render_con_stacked` (static)
fn render_con_stacked(t: &mut Tree, con: ConId, child: ConId, p: &mut render_params, i: usize) {
    assert!(t.cons[con].layout == Layout::L_STACKED);

    t.cons[child].rect.x = p.x as u32;
    t.cons[child].rect.y = p.y as u32;
    t.cons[child].rect.width = p.rect.width;
    t.cons[child].rect.height = p.rect.height;

    t.cons[child].deco_rect.x = (p.x as u32).wrapping_sub(t.cons[con].rect.x);
    t.cons[child].deco_rect.y = (p.y as u32).wrapping_sub(t.cons[con].rect.y).wrapping_add((i as u32).wrapping_mul(p.deco_height as u32));
    t.cons[child].deco_rect.width = t.cons[child].rect.width;
    t.cons[child].deco_rect.height = p.deco_height as u32;

    if p.children > 1 || (t.cons[child].border_style != BorderStyle::BS_PIXEL && t.cons[child].border_style != BorderStyle::BS_NONE) {
        t.cons[child].rect.y = t.cons[child].rect.y.wrapping_add((p.deco_height as u32).wrapping_mul(p.children as u32));
        t.cons[child].rect.height = t.cons[child].rect.height.wrapping_sub((p.deco_height as u32).wrapping_mul(p.children as u32));
    }
}

/// C: src/render.c:420 `render_con_tabbed` (static)
fn render_con_tabbed(t: &mut Tree, con: ConId, child: ConId, p: &mut render_params, i: usize) {
    assert!(t.cons[con].layout == Layout::L_TABBED);

    t.cons[child].rect.x = p.x as u32;
    t.cons[child].rect.y = p.y as u32;
    t.cons[child].rect.width = p.rect.width;
    t.cons[child].rect.height = p.rect.height;

    let tab_width = (t.cons[child].rect.width as f32 / p.children as f32).floor() as u32;
    t.cons[child].deco_rect.width = tab_width;
    t.cons[child].deco_rect.x = (p.x as u32)
        .wrapping_sub(t.cons[con].rect.x)
        .wrapping_add((i as u32).wrapping_mul(t.cons[child].deco_rect.width));
    t.cons[child].deco_rect.y = (p.y as u32).wrapping_sub(t.cons[con].rect.y);

    /* Since the tab width may be something like 31,6 px per tab, we
     * let the last tab have all the extra space (0,6 * children). */
    if i == (p.children as usize).wrapping_sub(1) {
        t.cons[child].deco_rect.width = t.cons[child].rect.width.wrapping_sub(t.cons[child].deco_rect.x);
    }

    if p.children > 1 || (t.cons[child].border_style != BorderStyle::BS_PIXEL && t.cons[child].border_style != BorderStyle::BS_NONE) {
        t.cons[child].rect.y = t.cons[child].rect.y.wrapping_add(p.deco_height as u32);
        t.cons[child].rect.height = t.cons[child].rect.height.wrapping_sub(p.deco_height as u32);
        t.cons[child].deco_rect.height = p.deco_height as u32;
    } else {
        t.cons[child].deco_rect.height = if t.cons[child].border_style == BorderStyle::BS_PIXEL { 1 } else { 0 };
    }
}

/// C: src/render.c:446 `render_con_dockarea` (static)
fn render_con_dockarea(t: &mut Tree, _con: ConId, child: ConId, p: &mut render_params) {
    child_rect_set(t, child, p);
    // C: render.c:209 — a dock client keeps its own requested height; the
    // dockarea only stretches it to the output width. Without this the bar
    // is shrunk to the parent's height (which is 0 before the first layout).
    t.cons[child].rect.height = t.cons[child].geometry.height;

    t.cons[child].deco_rect.x = 0;
    t.cons[child].deco_rect.y = 0;
    t.cons[child].deco_rect.width = 0;
    t.cons[child].deco_rect.height = 0;
    p.y += t.cons[child].rect.height as i32;
}

fn child_rect_set(t: &mut Tree, child: ConId, p: &render_params) {
    t.cons[child].rect.x = p.x as u32;
    t.cons[child].rect.y = p.y as u32;
    t.cons[child].rect.width = p.rect.width;
    t.cons[child].rect.height = t.cons[child].geometry.height;
}

// workspace_is_visible re-export anchor (C: render_root uses it via output.c).
#[allow(unused)]
fn _workspace_is_visible_anchor(t: &Tree, ws: ConId) -> bool {
    workspace_is_visible(t, ws)
}

// con_orientation / con_num_children import anchors.
#[allow(unused)]
fn _con_orientation_anchor(t: &Tree, con: ConId) -> crate::data::Orientation {
    con_orientation(t, con)
}