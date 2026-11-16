

use crate::xcb_screen_t;
use crate::xcb_visualtype_t;

/// C: libi3/get_visualtype.c:14 — Returns the visual type associated with
/// the given screen.
pub fn get_visualtype(screen: &xcb_screen_t) -> Option<&xcb_visualtype_t> {
    // C: for (depth_iter = xcb_screen_allowed_depths_iterator(screen); ...)
    for depth in &screen.allowed_depths {
        // C: for (visual_iter = xcb_depth_visuals_iterator(depth_iter.data); ...)
        for visual in &depth.visuals {
            if screen.root_visual == visual.visual_id {
                return Some(visual);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_root_visual() {
        use x11rb::protocol::xproto::{BackingStore, Depth, EventMask, Screen, VisualClass, Visualtype};
        let screen = Screen {
            root: 0,
            default_colormap: 0,
            white_pixel: 0,
            black_pixel: 0,
            current_input_masks: EventMask::NO_EVENT,
            width_in_pixels: 0,
            height_in_pixels: 0,
            width_in_millimeters: 0,
            height_in_millimeters: 0,
            min_installed_maps: 0,
            max_installed_maps: 0,
            root_visual: 0x21,
            backing_stores: BackingStore::NOT_USEFUL,
            save_unders: false,
            root_depth: 24,
            allowed_depths: vec![Depth {
                depth: 24,
                visuals: vec![Visualtype {
                    visual_id: 0x21,
                    class: VisualClass::TRUE_COLOR,
                    red_mask: 0xff0000,
                    green_mask: 0xff00,
                    blue_mask: 0xff,
                    bits_per_rgb_value: 8,
                    colormap_entries: 256,
                }],
            }],
        };
        let visual = get_visualtype(&screen).expect("root visual");
        assert_eq!(visual.visual_id, 0x21);
    }

    #[test]
    fn no_visual_returns_none() {
        use x11rb::protocol::xproto::{BackingStore, EventMask, Screen};
        let screen = Screen {
            root: 0,
            default_colormap: 0,
            white_pixel: 0,
            black_pixel: 0,
            current_input_masks: EventMask::NO_EVENT,
            width_in_pixels: 0,
            height_in_pixels: 0,
            width_in_millimeters: 0,
            height_in_millimeters: 0,
            min_installed_maps: 0,
            max_installed_maps: 0,
            root_visual: 0x99,
            backing_stores: BackingStore::NOT_USEFUL,
            save_unders: false,
            root_depth: 24,
            allowed_depths: vec![],
        };
        assert!(get_visualtype(&screen).is_none());
    }
}
