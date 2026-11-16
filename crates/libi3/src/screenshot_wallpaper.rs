//! C: libi3/screenshot_wallpaper.c

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, CreateGCAux, FillStyle, SubwindowMode, GX,
};

use crate::xcb_screen_t;

/// C: libi3/screenshot_wallpaper.c:18-20 — the four GC values of the copy:
/// XCB_GC_FUNCTION | XCB_GC_PLANE_MASK | XCB_GC_FILL_STYLE |
/// XCB_GC_SUBWINDOW_MODE with {GX_COPY, ~0, SOLID, INCLUDE_INFERIORS}.
///
/// Kept as a plain function so the values can be asserted without an X server.
fn gc_values() -> (GX, u32, FillStyle, SubwindowMode) {
    (
        GX::COPY,
        !0u32,
        FillStyle::SOLID,
        SubwindowMode::INCLUDE_INFERIORS,
    )
}

/// C: libi3/screenshot_wallpaper.c:10 set_screenshot_as_wallpaper — grab a
/// screenshot of the screen's root window and set it as the wallpaper.
///
/// C keeps the connection and screen in globals; the port passes them in
/// (x11rb requests only need a shared borrow).
pub fn set_screenshot_as_wallpaper<C>(conn: &C, screen: &xcb_screen_t)
where
    C: Connection + x11rb::protocol::xproto::ConnectionExt + ?Sized,
{
    /* C: 11-12 */
    let width = screen.width_in_pixels;
    let height = screen.height_in_pixels;

    /* C: 13-14 xcb_generate_id x2 */
    let (Ok(pixmap), Ok(gc)) = (conn.generate_id(), conn.generate_id()) else {
        return;
    };

    /* C: 16 xcb_create_pixmap(conn, screen->root_depth, pixmap, screen->root,
     * width, height) */
    let _ = x11rb::protocol::xproto::ConnectionExt::create_pixmap(
        conn,
        screen.root_depth,
        pixmap,
        screen.root,
        width,
        height,
    );

    /* C: 18-20 xcb_create_gc */
    let (function, plane_mask, fill_style, subwindow_mode) = gc_values();
    let gc_aux = CreateGCAux::new()
        .function(function)
        .plane_mask(plane_mask)
        .fill_style(fill_style)
        .subwindow_mode(subwindow_mode);
    let _ = x11rb::protocol::xproto::ConnectionExt::create_gc(conn, gc, screen.root, &gc_aux);

    /* C: 22 xcb_copy_area(conn, screen->root, pixmap, gc, 0, 0, 0, 0, width,
     * height) */
    let _ = x11rb::protocol::xproto::ConnectionExt::copy_area(
        conn,
        screen.root,
        pixmap,
        gc,
        0,
        0,
        0,
        0,
        width,
        height,
    );

    /* C: 23 xcb_change_window_attributes(conn, screen->root, XCB_CW_BACK_PIXMAP,
     * {pixmap}) */
    let attributes = ChangeWindowAttributesAux::new().background_pixmap(pixmap);
    let _ = x11rb::protocol::xproto::ConnectionExt::change_window_attributes(
        conn,
        screen.root,
        &attributes,
    );

    /* C: 24-25 — the window attribute keeps the pixmap alive, so both are
     * freed right away. */
    let _ = x11rb::protocol::xproto::ConnectionExt::free_gc(conn, gc);
    let _ = x11rb::protocol::xproto::ConnectionExt::free_pixmap(conn, pixmap);

    /* C: 26 xcb_flush(conn) */
    let _ = x11rb::connection::Connection::flush(conn);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::is_background_set::is_background_set;
    /* C: xcb_aux_sync — the round trip that makes the requests take effect */
    use x11rb::wrapper::ConnectionExt as _;

    /// C: libi3/screenshot_wallpaper.c:18-20 — the exact GC values.
    #[test]
    fn gc_values_match_the_c_arguments() {
        let (function, plane_mask, fill_style, subwindow_mode) = gc_values();
        assert_eq!(function, GX::COPY);
        assert_eq!(plane_mask, 0xffff_ffff);
        assert_eq!(fill_style, FillStyle::SOLID);
        assert_eq!(subwindow_mode, SubwindowMode::INCLUDE_INFERIORS);
    }

    /// Real X (headless Xvfb): the pixmap becomes the root background pixmap
    /// (C sets XCB_CW_BACK_PIXMAP) and stays alive after xcb_free_pixmap.
    #[test]
    fn real_x_sets_the_root_background_pixmap() {
        let Some(xvfb) = crate::test_util::start_xvfb() else {
            eprintln!("skipping: no Xvfb available");
            return;
        };
        let (conn, screen_num) = match x11rb::connect(Some(xvfb.display())) {
            Ok(conn) => conn,
            Err(err) => panic!("could not connect to {}: {err}", xvfb.display()),
        };
        let screen = conn.setup().roots[screen_num].clone();

        // Paint a known color onto the root content: a mapped override-redirect
        // window in the top left corner (what a real screen shows).
        let window = conn.generate_id().expect("generate_id failed");
        x11rb::protocol::xproto::ConnectionExt::create_window(
            &conn,
            x11rb::COPY_FROM_PARENT as u8,
            window,
            screen.root,
            0,
            0,
            10,
            10,
            0,
            x11rb::protocol::xproto::WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &x11rb::protocol::xproto::CreateWindowAux::new()
                .background_pixel(0x00ff_0000)
                .override_redirect(1),
        )
        .expect("create_window failed");
        x11rb::protocol::xproto::ConnectionExt::map_window(&conn, window)
            .expect("map_window failed");
        let _ = conn.sync();
        let painted = read_pixel(&conn, &screen, 0, 0);
        assert_eq!(painted.len(), 4, "24-bit root: four bytes per pixel");

        // The screenshot copies that content into the pixmap which becomes the
        // root background.
        set_screenshot_as_wallpaper(&conn, &screen);
        let _ = x11rb::connection::Connection::flush(&conn);

        // Closing the window repaints the area from the wallpaper, so the
        // pixel is still the one the screenshot captured.
        x11rb::protocol::xproto::ConnectionExt::destroy_window(&conn, window)
            .expect("destroy_window failed");
        let _ = conn.sync();
        assert_eq!(
            read_pixel(&conn, &screen, 0, 0),
            painted,
            "the root must show the screenshot after the window is gone"
        );
        assert!(
            is_background_set(&conn, &screen),
            "the screenshot pixmap must be the root background"
        );
    }

    /// 1x1 GetImage of the root, used to observe what the screen shows.
    fn read_pixel<C>(conn: &C, screen: &xcb_screen_t, x: i16, y: i16) -> Vec<u8>
    where
        C: x11rb::connection::Connection + x11rb::protocol::xproto::ConnectionExt + ?Sized,
    {
        x11rb::protocol::xproto::ConnectionExt::get_image(
            conn,
            x11rb::protocol::xproto::ImageFormat::Z_PIXMAP,
            screen.root,
            x,
            y,
            1,
            1,
            !0u32,
        )
        .expect("get_image failed")
        .reply()
        .expect("get_image reply failed")
        .data
    }
}
