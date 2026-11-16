//! C: libi3/ucs2_conversion.c

use crate::err::{perror, set_errno};
use crate::xcb_char2b_t;

/// C: libi3/ucs2_conversion.c:23 — Converts the given string to UTF-8 from
/// UCS-2 big endian. The return value must be freed after use.
///
/// Returns None where C returns NULL (iconv failure on unpaired surrogates;
/// the C code prints perror("Converting to UTF-8 failed") first).
pub fn convert_ucs2_to_utf8(text: &[xcb_char2b_t], num_glyphs: usize) -> Option<String> {
    // C: buffer_size = num_glyphs * 4 + 1 (UTF-8 is at most 4 bytes per glyph)
    // (C trusts num_glyphs to be within the text buffer; we clamp instead.)
    let n = num_glyphs.min(text.len());
    let mut buffer = String::with_capacity(n * 4 + 1);

    // iconv("UTF-8", "UCS-2BE"): each big-endian word is one code point.
    // Unpaired surrogate words are invalid sequences → iconv returns -1 →
    // perror + NULL (errno = EILSEQ).
    for &glyph in &text[..n] {
        let codepoint = ((glyph.byte1 as u16) << 8) | glyph.byte2 as u16;
        if (0xd800..=0xdfff).contains(&codepoint) {
            set_errno(libc::EILSEQ);
            perror("Converting to UTF-8 failed");
            return None;
        }
        buffer.push(char::from_u32(codepoint as u32).expect("non-surrogate BMP code point"));
    }

    Some(buffer)
}

/// C: libi3/ucs2_conversion.c:63 — Converts the given string to UCS-2 big
/// endian for use with xcb_image_text_16(). The amount of real glyphs is
/// stored in real_strlen.
///
/// iconv is opened with "UCS-2BE//IGNORE": characters that cannot be
/// represented in UCS-2 (outside the BMP) are silently discarded.
pub fn convert_utf8_to_ucs2(input: &str, real_strlen: Option<&mut usize>) -> Vec<xcb_char2b_t> {
    // C: buffer_size = input_size * sizeof(xcb_char2b_t)
    let mut buffer: Vec<xcb_char2b_t> = Vec::with_capacity(input.len());

    for c in input.chars() {
        let cp = c as u32;
        if cp > 0xffff {
            // "//IGNORE" drops unrepresentable characters
            continue;
        }
        let codepoint = cp as u16;
        buffer.push(xcb_char2b_t {
            byte1: (codepoint >> 8) as u8,
            byte2: (codepoint & 0xff) as u8,
        });
    }

    // C: *real_strlen = buffer_size / sizeof(xcb_char2b_t)
    if let Some(rs) = real_strlen {
        *rs = buffer.len();
    }

    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ucs2(units: &[u16]) -> Vec<xcb_char2b_t> {
        units
            .iter()
            .map(|&u| xcb_char2b_t {
                byte1: (u >> 8) as u8,
                byte2: (u & 0xff) as u8,
            })
            .collect()
    }

    #[test]
    fn utf8_to_ucs2_and_back() {
        let input = "héllo €";
        let mut num_glyphs = 0usize;
        let converted = convert_utf8_to_ucs2(input, Some(&mut num_glyphs));
        assert_eq!(num_glyphs, 7);
        assert_eq!(converted.len(), 7);
        // round trip
        assert_eq!(
            convert_ucs2_to_utf8(&converted, converted.len()),
            Some(input.to_string())
        );
    }

    #[test]
    fn utf8_to_ucs2_drops_astral_chars() {
        // U+1F600 cannot be represented in UCS-2 → //IGNORE drops it.
        let input = "a😀b";
        let mut num_glyphs = 999usize;
        let converted = convert_utf8_to_ucs2(input, Some(&mut num_glyphs));
        assert_eq!(num_glyphs, 2);
        assert_eq!(convert_ucs2_to_utf8(&converted, 2), Some("ab".to_string()));
    }

    #[test]
    fn utf8_to_ucs2_null_real_strlen() {
        let converted = convert_utf8_to_ucs2("abc", None);
        assert_eq!(converted.len(), 3);
    }

    #[test]
    fn ucs2_to_utf8_surrogate_fails() {
        // Unpaired surrogate → C iconv error → NULL.
        let units = ucs2(&[0x0041, 0xd800]);
        assert_eq!(convert_ucs2_to_utf8(&units, 2), None);
    }

    #[test]
    fn ucs2_to_utf8_basic() {
        let units = ucs2(&[0x0048, 0x0069]);
        assert_eq!(convert_ucs2_to_utf8(&units, 2), Some("Hi".to_string()));
        // num_glyphs larger than the buffer is clamped (C would read OOB).
        assert_eq!(convert_ucs2_to_utf8(&units, 100), Some("Hi".to_string()));
    }
}
