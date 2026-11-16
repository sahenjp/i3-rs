//! C: libi3/get_colorpixel.c

/// C: libi3/get_colorpixel.c:32 — Returns the colorpixel to use for the
/// given hex color (think of HTML).
///
/// The hex_color has to start with #, for example #FF00FF.
///
/// NOTE that get_colorpixel() does _NOT_ check the given color code for
/// validity. This has to be done by the caller.
///
/// NOTE that this function may in the future rely on a global
/// xcb_connection_t variable called 'conn' to be present.
pub fn get_colorpixel(hex: &str) -> u32 {
    // C: char alpha[2];
    let (a0, a1) = if hex.len() == "#rrggbbaa".len() {
        // C: alpha[0] = hex[7]; alpha[1] = hex[8];
        (hex.as_bytes()[7], hex.as_bytes()[8])
    } else {
        (b'F', b'F')
    };

    // C: char strgroups[4][3] — two hex digits per group, NUL-terminated
    let groups: [[u8; 2]; 4] = [
        [hex.as_bytes()[1], hex.as_bytes()[2]],
        [hex.as_bytes()[3], hex.as_bytes()[4]],
        [hex.as_bytes()[5], hex.as_bytes()[6]],
        [a0, a1],
    ];
    // C: uint8_t r = strtol(strgroups[0], NULL, 16); ...
    // (strtol of a malformed group yields 0, like unwrap_or(0) here.)
    let parse_group = |g: &[u8; 2]| -> u32 {
        let s = String::from_utf8_lossy(g).into_owned();
        u8::from_str_radix(s.trim(), 16).unwrap_or(0) as u32
    };
    let r = parse_group(&groups[0]);
    let g = parse_group(&groups[1]);
    let b = parse_group(&groups[2]);
    let a = parse_group(&groups[3]);

    // Shortcut: if our screen is true color, no need to do a roundtrip to
    // X11 (the C code checks the global root_screen here).
    // TODO(X11 port): check root_screen->root_depth (24/32) and implement
    // the colorpixel cache + xcb_alloc_color round-trip for other depths.
    (a << 24) | (r << 16) | (g << 8) | b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truecolor_shortcut() {
        assert_eq!(get_colorpixel("#FF00FF"), 0xFFFF00FF);
        assert_eq!(get_colorpixel("#000000"), 0xFF000000);
        assert_eq!(get_colorpixel("#123456"), 0xFF123456);
        assert_eq!(get_colorpixel("#ffffff"), 0xFFFFFFFF);
    }

    #[test]
    fn eight_digit_hex_uses_alpha() {
        assert_eq!(get_colorpixel("#00ff0080"), 0x8000ff00);
        assert_eq!(get_colorpixel("#00ff0000"), 0x0000ff00);
    }
}
