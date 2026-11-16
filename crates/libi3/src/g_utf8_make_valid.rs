//! C: libi3/g_utf8_make_valid.c (copied from glib's gutf8.c).

/// C: libi3/g_utf8_make_valid.c:50 g_utf8_make_valid — If the provided
/// string is valid UTF-8, return a copy of it. If not, return a copy in
/// which bytes that could not be interpreted as valid Unicode are replaced
/// with the Unicode replacement character (U+FFFD).
///
/// len < 0 means "NUL-terminated" in C: the input is processed up to the
/// first NUL byte (strlen semantics).
pub fn g_utf8_make_valid(input: &[u8], len: isize) -> String {
    // C: if (len < 0) len = strlen (str);
    let bytes: &[u8] = if len < 0 {
        match input.iter().position(|&b| b == 0) {
            Some(p) => &input[..p],
            None => input,
        }
    } else {
        // (C trusts len to be within the buffer; we clamp instead.)
        &input[..(len as usize).min(input.len())]
    };

    let mut string: Option<String> = None;
    // C: remainder = str; remaining_bytes = len;
    let mut remainder_start = 0usize;
    let mut remaining_bytes = bytes.len();

    // C: while (remaining_bytes != 0) { if (g_utf8_validate(...)) break; ... }
    while remaining_bytes != 0 {
        match std::str::from_utf8(&bytes[remainder_start..remainder_start + remaining_bytes]) {
            Ok(_) => break,
            Err(e) => {
                // C: valid_bytes = invalid - remainder;
                let valid_bytes = e.valid_up_to();

                // C: if (string == NULL) string = g_string_sized_new (remaining_bytes);
                let s = string.get_or_insert_with(|| String::with_capacity(bytes.len()));
                // C: g_string_append_len (string, remainder, valid_bytes);
                s.push_str(
                    std::str::from_utf8(&bytes[remainder_start..remainder_start + valid_bytes])
                        .expect("valid prefix"),
                );
                // append U+FFFD REPLACEMENT CHARACTER
                s.push('�');

                remaining_bytes -= valid_bytes + 1;
                remainder_start += valid_bytes + 1;
            }
        }
    }

    match string {
        None => {
            // C: g_strndup (str, len) — the whole input was valid UTF-8.
            String::from_utf8(bytes.to_vec()).expect("validated UTF-8")
        }
        Some(mut s) => {
            // C: g_string_append_len (string, remainder, remaining_bytes);
            //    g_string_append_c (string, '\0');  — the C NUL terminator is
            //    implicit in a Rust String.
            if remaining_bytes > 0 {
                s.push_str(
                    std::str::from_utf8(&bytes[remainder_start..]).expect("valid remainder"),
                );
            }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_input_is_copied() {
        assert_eq!(g_utf8_make_valid(b"already valid", -1), "already valid");
        // len-limited
        assert_eq!(g_utf8_make_valid(b"abcdef", 3), "abc");
    }

    #[test]
    fn invalid_bytes_become_replacement_chars() {
        assert_eq!(g_utf8_make_valid(b"\xff\xfe", -1), "\u{FFFD}\u{FFFD}");
        assert_eq!(g_utf8_make_valid(b"a\xffb", -1), "a\u{FFFD}b");
        // truncated 2-byte sequence at the end
        assert_eq!(g_utf8_make_valid(b"a\xc3", -1), "a\u{FFFD}");
    }

    #[test]
    fn len_zero() {
        assert_eq!(g_utf8_make_valid(b"abc", 0), "");
    }

    #[test]
    fn nul_terminated_len() {
        // len < 0: only bytes up to the first NUL are processed (strlen).
        assert_eq!(g_utf8_make_valid(b"ab\0cd", -1), "ab");
    }
}
