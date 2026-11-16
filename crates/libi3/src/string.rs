//! C: libi3/string.c — Define an i3String type to automagically handle
//! UTF-8/UCS-2 conversions. Some font backends need UCS-2 (X core fonts),
//! others want UTF-8 (Pango).

use crate::g_utf8_make_valid::g_utf8_make_valid;
use crate::ucs2_conversion::{convert_ucs2_to_utf8, convert_utf8_to_ucs2};
use crate::xcb_char2b_t;

/// C: libi3/string.c:15 struct _i3String (opaque in include/libi3.h).
pub struct i3String {
    /// C: char *utf8
    utf8: Option<String>,
    /// C: xcb_char2b_t *ucs2
    ucs2: Option<Vec<xcb_char2b_t>>,
    /// C: size_t num_glyphs
    num_glyphs: usize,
    /// C: size_t num_bytes
    num_bytes: usize,
    /// C: bool pango_markup
    pango_markup: bool,
}

impl Default for i3String {
    fn default() -> Self {
        i3String {
            utf8: None,
            ucs2: None,
            num_glyphs: 0,
            num_bytes: 0,
            pango_markup: false,
        }
    }
}

/// C: libi3/string.c:31 — Build an i3String from an UTF-8 encoded string.
/// Returns the newly-allocated i3String.
pub fn i3string_from_utf8(from_utf8: &str) -> i3String {
    i3string_from_utf8_with_length(from_utf8.as_bytes(), -1)
}

/// C: libi3/string.c:39 — Build an i3String from an UTF-8 encoded string in
/// Pango markup.
pub fn i3string_from_markup(from_markup: &str) -> i3String {
    let mut str = i3string_from_utf8(from_markup);

    // Set the markup flag
    str.pango_markup = true;

    str
}

/// C: libi3/string.c:54 — Build an i3String from an UTF-8 encoded string
/// with fixed length. To be used when no proper NULL-termination is
/// available.
pub fn i3string_from_utf8_with_length(from_utf8: &[u8], num_bytes: isize) -> i3String {
    let mut str = i3String::default();

    // g_utf8_make_valid NULL-terminates the string.
    let valid = g_utf8_make_valid(from_utf8, num_bytes);
    // num_bytes < 0 means NULL-terminated string, need to calculate length
    // (C: strlen() — stops at the first NUL byte).
    str.num_bytes = if num_bytes < 0 {
        valid.find('\0').unwrap_or(valid.len())
    } else {
        num_bytes as usize
    };
    str.utf8 = Some(valid);

    str
}

/// C: libi3/string.c:71 — Build an i3String from an UTF-8 encoded string in
/// Pango markup with fixed length.
pub fn i3string_from_markup_with_length(from_markup: &[u8], num_bytes: usize) -> i3String {
    let mut str = i3string_from_utf8_with_length(from_markup, num_bytes as isize);

    // set the markup flag
    str.pango_markup = true;

    str
}

/// C: libi3/string.c:85 — Build an i3String from an UCS-2 encoded string.
pub fn i3string_from_ucs2(from_ucs2: &[xcb_char2b_t], num_glyphs: usize) -> i3String {
    let mut str = i3String::default();

    // Copy the actual text to our i3String
    // (C trusts num_glyphs to be within the buffer; we clamp instead.)
    let n = num_glyphs.min(from_ucs2.len());
    str.ucs2 = Some(from_ucs2[..n].to_vec());

    // Store the length
    str.num_glyphs = n;

    str.utf8 = None;
    str.num_bytes = 0;

    str
}

/// C: libi3/string.c:105 — Copies the given i3string.
/// Note that this will not free the source string.
pub fn i3string_copy(str: &mut i3String) -> i3String {
    // (C crashes if the UTF-8 conversion failed and as_utf8 returns NULL;
    // we pass the empty string instead.)
    let mut copy = i3string_from_utf8(i3string_as_utf8(str).unwrap_or(""));
    copy.pango_markup = str.pango_markup;
    copy
}

/// C: libi3/string.c:115 — Free an i3String.
/// In Rust freeing is done by dropping the value; this function consumes
/// (and thereby frees) the string so ported call sites keep their shape.
/// The C I3STRING_FREE() macro (free + NULL the pointer) is unnecessary with
/// Rust ownership.
pub fn i3string_free(str: Option<i3String>) {
    drop(str);
}

/// C: libi3/string.c:124 i3string_ensure_utf8 (static)
fn i3string_ensure_utf8(str: &mut i3String) {
    if str.utf8.is_some() {
        return;
    }
    // (utf8 is None only for strings built from UCS-2, which always carry
    // the ucs2 buffer.)
    if let Some(converted) = convert_ucs2_to_utf8(str.ucs2.as_deref().unwrap_or(&[]), str.num_glyphs) {
        // C: str->num_bytes = strlen(str->utf8)
        str.num_bytes = converted.len();
        str.utf8 = Some(converted);
    }
}

/// C: libi3/string.c:133 i3string_ensure_ucs2 (static)
fn i3string_ensure_ucs2(str: &mut i3String) {
    if str.ucs2.is_some() {
        return;
    }
    let mut num_glyphs = str.num_glyphs;
    let ucs2 = convert_utf8_to_ucs2(str.utf8.as_deref().unwrap_or(""), Some(&mut num_glyphs));
    str.num_glyphs = num_glyphs;
    str.ucs2 = Some(ucs2);
}

/// C: libi3/string.c:144 — Returns the UTF-8 encoded version of the
/// i3String. (The C function can return NULL when the UCS-2 → UTF-8
/// conversion fails; None mirrors that.)
pub fn i3string_as_utf8(str: &mut i3String) -> Option<&str> {
    i3string_ensure_utf8(str);
    str.utf8.as_deref()
}

/// C: libi3/string.c:153 — Returns the UCS-2 encoded version of the i3String.
pub fn i3string_as_ucs2(str: &mut i3String) -> &[xcb_char2b_t] {
    i3string_ensure_ucs2(str);
    str.ucs2.as_deref().unwrap_or(&[])
}

/// C: libi3/string.c:162 — Returns the number of bytes (UTF-8 encoded) in
/// an i3String.
pub fn i3string_get_num_bytes(str: &mut i3String) -> usize {
    i3string_ensure_utf8(str);
    str.num_bytes
}

/// C: libi3/string.c:170 — Whether the given i3String is in Pango markup.
pub fn i3string_is_markup(str: &i3String) -> bool {
    str.pango_markup
}

/// C: libi3/string.c:177 — Set whether the i3String should use Pango markup.
pub fn i3string_set_markup(str: &mut i3String, pango_markup: bool) {
    str.pango_markup = pango_markup;
}

/// C: libi3/string.c:184 — Escape pango markup characters in the given string.
pub fn i3string_escape_markup(str: &mut i3String) -> i3String {
    let text = i3string_as_utf8(str).unwrap_or("");
    let escaped = g_markup_escape_text(text);
    i3string_from_utf8(&escaped)
}

/// C: libi3/string.c:196 — Returns the number of glyphs in an i3String.
pub fn i3string_get_num_glyphs(str: &mut i3String) -> usize {
    i3string_ensure_ucs2(str);
    str.num_glyphs
}

// C: glib gmarkup.c g_markup_escape_text() — called at libi3/string.c:187.
// Escapes '&', '<', '>', '\'', '"' and the control characters outside the
// XML-valid range as numeric character references.
fn g_markup_escape_text(text: &str) -> String {
    let mut str = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => str.push_str("&amp;"),
            '<' => str.push_str("&lt;"),
            '>' => str.push_str("&gt;"),
            '\'' => str.push_str("&#39;"),
            '"' => str.push_str("&quot;"),
            _ => {
                let cu = c as u32;
                if (0x1..=0x8).contains(&cu)
                    || (0xb..=0xc).contains(&cu)
                    || (0xe..=0x1f).contains(&cu)
                    || (0x7f..=0x84).contains(&cu)
                    || (0x86..=0x9f).contains(&cu)
                {
                    // C: g_string_append_printf (str, "&#x%x;", c)
                    str.push_str(&format!("&#x{:x};", c as u32));
                } else {
                    str.push(c);
                }
            }
        }
    }
    str
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_utf8_basic() {
        let mut s = i3string_from_utf8("héllo");
        assert_eq!(i3string_as_utf8(&mut s), Some("héllo"));
        assert_eq!(i3string_get_num_bytes(&mut s), 6);
        assert_eq!(i3string_get_num_glyphs(&mut s), 5);
        assert!(!i3string_is_markup(&s));
    }

    #[test]
    fn from_markup_sets_flag() {
        let mut s = i3string_from_markup("<b>bold</b>");
        assert!(i3string_is_markup(&s));
        assert_eq!(i3string_as_utf8(&mut s), Some("<b>bold</b>"));
    }

    #[test]
    fn from_utf8_with_length_and_invalid_bytes() {
        // invalid byte → U+FFFD replacement (g_utf8_make_valid)
        let mut s = i3string_from_utf8_with_length(b"\xff", 1);
        assert_eq!(i3string_as_utf8(&mut s), Some("\u{FFFD}"));
        assert_eq!(i3string_get_num_bytes(&mut s), 1);
        // fixed length shorter than the input
        let mut s2 = i3string_from_utf8_with_length(b"abcdef", 3);
        assert_eq!(i3string_as_utf8(&mut s2), Some("abc"));
        assert_eq!(i3string_get_num_bytes(&mut s2), 3);
    }

    #[test]
    fn from_markup_with_length() {
        let mut s = i3string_from_markup_with_length(b"<b>hi</b>", 9);
        assert!(i3string_is_markup(&s));
        assert_eq!(i3string_as_utf8(&mut s), Some("<b>hi</b>"));
    }

    #[test]
    fn from_ucs2_lazy_utf8() {
        let units = vec![
            xcb_char2b_t { byte1: 0, byte2: b'H' },
            xcb_char2b_t { byte1: 0, byte2: b'i' },
        ];
        let mut s = i3string_from_ucs2(&units, 2);
        assert_eq!(i3string_get_num_glyphs(&mut s), 2);
        assert_eq!(i3string_as_utf8(&mut s), Some("Hi"));
        assert_eq!(i3string_get_num_bytes(&mut s), 2);
    }

    #[test]
    fn copy_preserves_markup() {
        let mut s = i3string_from_markup("x");
        let mut copy = i3string_copy(&mut s);
        assert!(i3string_is_markup(&copy));
        assert_eq!(i3string_as_utf8(&mut copy), Some("x"));
    }

    #[test]
    fn escape_markup() {
        let mut s = i3string_from_utf8("<a & 'b' \"c\">");
        let mut escaped = i3string_escape_markup(&mut s);
        assert_eq!(
            i3string_as_utf8(&mut escaped),
            Some("&lt;a &amp; &#39;b&#39; &quot;c&quot;&gt;")
        );
    }

    #[test]
    fn escape_markup_control_chars() {
        let mut s = i3string_from_utf8("a\u{1}b");
        let mut escaped = i3string_escape_markup(&mut s);
        assert_eq!(i3string_as_utf8(&mut escaped), Some("a&#x1;b"));
    }

    #[test]
    fn set_markup() {
        let mut s = i3string_from_utf8("plain");
        i3string_set_markup(&mut s, true);
        assert!(i3string_is_markup(&s));
        i3string_set_markup(&mut s, false);
        assert!(!i3string_is_markup(&s));
    }

    #[test]
    fn num_glyphs_drops_astral() {
        // U+1F600 is not representable in UCS-2 → dropped from the glyph count.
        let mut s = i3string_from_utf8("a\u{1F600}b");
        assert_eq!(i3string_get_num_glyphs(&mut s), 2);
    }

    #[test]
    fn free_is_drop() {
        let s = i3string_from_utf8("x");
        i3string_free(Some(s));
        i3string_free(None);
    }
}
