//! C: libi3/is_background_set.c

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    CreateWindowAux, ImageFormat, MapState, Rectangle, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;

use crate::xcb_screen_t;

/// C: libi3/is_background_set.c:16 unobscured_region — the region of the given
/// window that is not covered by a mapped child window.
///
/// C holds this in a cairo_region_t; the port keeps a rectangle list, because
/// the only operations C performs on the region are "create from one
/// rectangle", "subtract a rectangle" and "what is the first rectangle".
///
/// The list order differs from cairo's sorted region, but the caller only needs
/// one pixel that no child covers, so any rectangle of the result is as good as
/// cairo's first one.
fn unobscured_region(
    window_width: u16,
    window_height: u16,
    children: &[(Rectangle, bool)],
) -> Vec<Rectangle> {
    /* C: 19-25 — rectangle = the whole window, region = that rectangle */
    let mut region = vec![Rectangle {
        x: 0,
        y: 0,
        width: window_width,
        height: window_height,
    }];

    /* C: 44-59 — remove every visible child from the region */
    for (rectangle, viewable) in children {
        if !*viewable {
            continue;
        }
        region = subtract_rectangle(&region, rectangle);
    }

    region
}

/// C: cairo_region_subtract_rectangle — split every rectangle of the region
/// around the cut rectangle (up to four remaining pieces per rectangle).
fn subtract_rectangle(region: &[Rectangle], cut: &Rectangle) -> Vec<Rectangle> {
    let mut result = Vec::new();
    for r in region {
        let (r_x1, r_y1) = (i64::from(r.x), i64::from(r.y));
        let (r_x2, r_y2) = (r_x1 + i64::from(r.width), r_y1 + i64::from(r.height));
        let (c_x1, c_y1) = (i64::from(cut.x), i64::from(cut.y));
        let (c_x2, c_y2) = (c_x1 + i64::from(cut.width), c_y1 + i64::from(cut.height));

        /* Disjoint rectangles stay untouched. */
        if c_x1 >= r_x2 || c_x2 <= r_x1 || c_y1 >= r_y2 || c_y2 <= r_y1 {
            result.push(*r);
            continue;
        }

        /* The band above the cut. */
        if c_y1 > r_y1 {
            result.push(rect_between(r_x1, r_y1, r_x2, c_y1));
        }
        /* The band below the cut. */
        if c_y2 < r_y2 {
            result.push(rect_between(r_x1, c_y2, r_x2, r_y2));
        }
        /* The left and right pieces of the band the cut overlaps. */
        let overlap_y1 = c_y1.max(r_y1);
        let overlap_y2 = c_y2.min(r_y2);
        if c_x1 > r_x1 {
            result.push(rect_between(r_x1, overlap_y1, c_x1, overlap_y2));
        }
        if c_x2 < r_x2 {
            result.push(rect_between(c_x2, overlap_y1, r_x2, overlap_y2));
        }
    }
    result
}

/// Builds a rectangle from two corners (C: cairo_rectangle_int_t fields).
fn rect_between(x1: i64, y1: i64, x2: i64, y2: i64) -> Rectangle {
    Rectangle {
        x: x1 as i16,
        y: y1 as i16,
        width: (x2 - x1) as u16,
        height: (y2 - y1) as u16,
    }
}

/// C: libi3/is_background_set.c:65 find_unobscured_pixel — the top left pixel
/// of the first rectangle of the unobscured region (0,0 when there is none).
fn find_unobscured_pixel<C>(
    conn: &C,
    window: Window,
    window_width: u16,
    window_height: u16,
) -> (i16, i16)
where
    C: Connection + x11rb::protocol::xproto::ConnectionExt + ?Sized,
{
    /* C: 27-31 xcb_query_tree — a failing reply leaves the whole window
     * unobscured. */
    let children = match x11rb::protocol::xproto::ConnectionExt::query_tree(conn, window)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    {
        Some(tree) => tree.children,
        None => vec![],
    };

    /* C: 36-42 — issue every request first, then read the replies */
    let geometries: Vec<_> = children
        .iter()
        .map(|child| x11rb::protocol::xproto::ConnectionExt::get_geometry(conn, *child).ok())
        .collect();
    let attributes: Vec<_> = children
        .iter()
        .map(|child| {
            x11rb::protocol::xproto::ConnectionExt::get_window_attributes(conn, *child).ok()
        })
        .collect();

    let mut covered: Vec<(Rectangle, bool)> = Vec::new();
    for (geometry, attribute) in geometries.into_iter().zip(attributes) {
        let geometry = geometry.and_then(|cookie| cookie.reply().ok());
        let attribute = attribute.and_then(|cookie| cookie.reply().ok());
        /* C: 49 — if (geom && attr && map_state == VIEWABLE) */
        if let (Some(geometry), Some(attribute)) = (geometry, attribute) {
            covered.push((
                Rectangle {
                    x: geometry.x,
                    y: geometry.y,
                    width: geometry.width,
                    height: geometry.height,
                },
                attribute.map_state == MapState::VIEWABLE,
            ));
        }
    }

    let region = unobscured_region(window_width, window_height, &covered);
    /* C: 69-79 — the top left pixel of the first rectangle, or (0,0) */
    match region.first() {
        Some(rect) => (rect.x, rect.y),
        None => (0, 0),
    }
}

/// C: libi3/is_background_set.c:83 flicker_window_at — show a 10x10 window
/// filled with the given pixel at (x,y), destroy it again and read back what
/// the root window shows at that position.
fn flicker_window_at<C>(
    conn: &C,
    screen: &xcb_screen_t,
    x: i16,
    y: i16,
    window: Window,
    pixel: u32,
) -> u32
where
    C: Connection + x11rb::protocol::xproto::ConnectionExt + ?Sized,
{
    let aux = CreateWindowAux::new()
        .background_pixel(pixel)
        /* C: XCB_CW_OVERRIDE_REDIRECT with the value 1 */
        .override_redirect(1);
    /* C: 85-87 xcb_create_window(..., XCB_COPY_FROM_PARENT, window,
     * screen->root, x, y, 10, 10, 0, INPUT_OUTPUT, COPY_FROM_PARENT, ...) */
    let _ = x11rb::protocol::xproto::ConnectionExt::create_window(
        conn,
        x11rb::COPY_FROM_PARENT as u8,
        window,
        screen.root,
        x,
        y,
        10,
        10,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &aux,
    );
    let _ = x11rb::protocol::xproto::ConnectionExt::map_window(conn, window);
    /* C: 89 xcb_clear_area(conn, 0, window, 0, 0, 0, 0) — 0/0 is "the whole
     * window" */
    let _ = x11rb::protocol::xproto::ConnectionExt::clear_area(conn, false, window, 0, 0, 0, 0);
    /* C: 90 xcb_aux_sync(conn) */
    let _ = conn.sync();
    let _ = x11rb::protocol::xproto::ConnectionExt::destroy_window(conn, window);

    /* C: 93-105 xcb_get_image(..., Z_PIXMAP, screen->root, x, y, 1, 1, ~0) */
    let image = x11rb::protocol::xproto::ConnectionExt::get_image(
        conn,
        ImageFormat::Z_PIXMAP,
        screen.root,
        x,
        y,
        1,
        1,
        !0u32,
    )
    .ok()
    .and_then(|cookie| cookie.reply().ok());

    match image {
        Some(image) => flicker_pixel(image.depth, &image.data),
        /* C: if (img) ... else result stays 0 */
        None => 0,
    }
}

/// C: libi3/is_background_set.c:97-103 — fold the first MIN(depth, 4) bytes of
/// the GetImage reply into one value.
fn flicker_pixel(depth: u8, data: &[u8]) -> u32 {
    let mut result = 0u32;
    for index in 0..usize::from(depth.min(4)) {
        let byte = data.get(index).copied().unwrap_or(0);
        result = (result << 8) | u32::from(byte);
    }
    result
}

/// C: libi3/is_background_set.c:108 is_background_set — test whether the
/// screen's root window has a background set.
///
/// This opens & closes a window and tests whether the root window still shows
/// the content of the window. C keeps the connection and screen in globals;
/// the port passes them in (x11rb requests only need a shared borrow).
pub fn is_background_set<C>(conn: &C, screen: &xcb_screen_t) -> bool
where
    C: Connection + x11rb::protocol::xproto::ConnectionExt + ?Sized,
{
    /* C: 109-110 */
    let (x, y) = find_unobscured_pixel(
        conn,
        screen.root,
        screen.width_in_pixels,
        screen.height_in_pixels,
    );

    /* C: 112 — one window id, reused for both flickers (C destroys it in
     * between, the port does the same). */
    let window = match conn.generate_id() {
        Ok(window) => window,
        Err(_) => return false,
    };

    /* C: 114-116 */
    let pixel1 = flicker_window_at(conn, screen, x, y, window, screen.black_pixel);
    let pixel2 = flicker_window_at(conn, screen, x, y, window, screen.white_pixel);
    pixel1 == pixel2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i16, y: i16, width: u16, height: u16) -> Rectangle {
        Rectangle {
            x,
            y,
            width,
            height,
        }
    }

    /// x11rb's Rectangle has no PartialEq, so the tests compare the fields.
    fn boxes(region: &[Rectangle]) -> Vec<(i16, i16, u16, u16)> {
        region
            .iter()
            .map(|r| (r.x, r.y, r.width, r.height))
            .collect()
    }

    /// C: libi3/is_background_set.c:16-63 — no children leaves the whole
    /// window, one viewable child is removed from the region.
    #[test]
    fn unobscured_region_removes_viewable_children_only() {
        // No children: the whole 640x480 window is unobscured.
        assert_eq!(
            boxes(&unobscured_region(640, 480, &[])),
            vec![(0, 0, 640, 480)]
        );

        // A viewable child in the top left corner leaves the rest; its own
        // top left pixel (0,0) is covered.
        let region = unobscured_region(640, 480, &[(rect(0, 0, 100, 50), true)]);
        assert_eq!(
            boxes(&region),
            vec![
                (0, 50, 640, 430), // below the child
                (100, 0, 540, 50), // right of the child
            ]
        );
        assert_eq!(region.first().map(|r| (r.x, r.y)), Some((0, 50)));

        // An unmapped child does not cover anything.
        assert_eq!(
            boxes(&unobscured_region(
                640,
                480,
                &[(rect(0, 0, 100, 50), false)]
            )),
            vec![(0, 0, 640, 480)]
        );
    }

    /// C: 44-59 — the child in the middle splits the region in four pieces;
    /// children outside the window do not change it.
    #[test]
    fn unobscured_region_splits_around_a_child_in_the_middle() {
        let region = boxes(&unobscured_region(
            100,
            100,
            &[(rect(40, 40, 20, 20), true)],
        ));
        assert_eq!(region.len(), 4);
        assert!(region.contains(&(0, 0, 100, 40)));
        assert!(region.contains(&(0, 60, 100, 40)));
        assert!(region.contains(&(0, 40, 40, 20)));
        assert!(region.contains(&(60, 40, 40, 20)));

        // A child fully outside the window is subtracted without effect.
        assert_eq!(
            boxes(&unobscured_region(
                100,
                100,
                &[(rect(200, 200, 10, 10), true)]
            )),
            vec![(0, 0, 100, 100)]
        );
        // A child covering everything leaves no rectangle at all.
        assert!(unobscured_region(100, 100, &[(rect(0, 0, 100, 100), true)]).is_empty());
    }

    /// C: 96-105 — the reply bytes are folded big-endian, MIN(depth, 4) of
    /// them: on the 24/32-bit roots i3 runs on that is always four bytes.
    #[test]
    fn flicker_pixel_folds_the_reply_bytes() {
        assert_eq!(flicker_pixel(24, &[0x12, 0x34, 0x56, 0x78]), 0x1234_5678);
        assert_eq!(flicker_pixel(32, &[0x00, 0x11, 0x22, 0x33]), 0x0011_2233);
        // C's MIN(depth, 4) keeps folding four bytes for an 8-bit root too.
        assert_eq!(flicker_pixel(8, &[0x7f, 0x11, 0x22, 0x33]), 0x7f11_2233);
        // Short data does not panic (C would read past it; the port pads 0).
        assert_eq!(flicker_pixel(24, &[0x11]), 0x1100_0000);
    }

    /// Real X (headless Xvfb): the flicker handshake issues its create/map/
    /// clear/sync/destroy/get_image round trips and leaves the server clean.
    ///
    /// Xvfb always repaints the root, so the "stale content" answer (false) can
    /// only be produced on a display whose root has no background at all; the
    /// geometry and pixel decisions of that path are covered by the pure tests
    /// above (unobscured_region, flicker_pixel).
    #[test]
    fn real_x_flicker_handshake_runs_against_a_server() {
        let Some(xvfb) = crate::test_util::start_xvfb() else {
            eprintln!("skipping: no Xvfb available");
            return;
        };
        let (conn, screen_num) = match x11rb::connect(Some(xvfb.display())) {
            Ok(conn) => conn,
            Err(err) => panic!("could not connect to {}: {err}", xvfb.display()),
        };
        let screen = conn.setup().roots[screen_num].clone();

        assert!(is_background_set(&conn, &screen));

        // The flicker window is destroyed again: the root has no children.
        let children = x11rb::protocol::xproto::ConnectionExt::query_tree(&conn, screen.root)
            .expect("query_tree failed")
            .reply()
            .expect("query_tree reply failed")
            .children;
        assert!(children.is_empty(), "leftover children: {children:?}");
    }
}
