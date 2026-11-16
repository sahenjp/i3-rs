//! C: libi3/format_placeholders.c

/// C: include/libi3.h:544 placeholder_t — Helper structure for usage in
/// format_placeholders().
pub struct placeholder_t<'a> {
    /// C: const char *name — The placeholder to be replaced, e.g. "%title".
    pub name: &'a str,
    /// C: const char *value — The value this placeholder should be replaced with.
    pub value: &'a str,
}

/// C: libi3/format_placeholders.c:21 — Replaces occurrences of the defined
/// placeholders in the format string.
///
/// Returns None where C returns NULL (format == NULL).
///
/// C first walks the string once to size the output buffer (that pass has no
/// observable effect other than the allocation size, so a growing String is
/// used here); the second pass below produces the same bytes.
pub fn format_placeholders(
    format: Option<&str>,
    placeholders: &[placeholder_t],
    num: i32,
) -> Option<String> {
    let format = format?;
    // C: int buffer_len = strlen(format) + 1; — used for the VLA, not needed here.
    let mut buffer: Vec<u8> = Vec::with_capacity(format.len() + 1);

    let bytes = format.as_bytes();
    let mut walk = 0usize;
    while walk < bytes.len() {
        // C: for (char *walk = format; *walk != '\0'; walk++)
        if bytes[walk] != b'%' {
            buffer.push(bytes[walk]);
            walk += 1;
            continue;
        }

        // C: bool matched = false; for (i...) if (!CS_STARTS_WITH(walk, placeholders[i].name)) continue;
        let mut matched = false;
        for i in 0..(num.max(0) as usize).min(placeholders.len()) {
            // C: CS_STARTS_WITH(string, needle) = strncmp(string, needle, strlen(needle)) == 0
            if !format[walk..].starts_with(placeholders[i].name) {
                continue;
            }
            matched = true;
            // C: outwalk += sprintf(outwalk, "%s", placeholders[i].value);
            buffer.extend_from_slice(placeholders[i].value.as_bytes());
            // C: walk += strlen(placeholders[i].name) - 1; (plus the loop's walk++)
            walk += placeholders[i].name.len();
            break;
        }

        if !matched {
            buffer.push(bytes[walk]);
            walk += 1;
        }
    }

    // C: *outwalk = '\0'; return sstrdup(buffer);
    Some(String::from_utf8(buffer).expect("copied bytes of a valid UTF-8 format string"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ph<'a>(name: &'a str, value: &'a str) -> placeholder_t<'a> {
        placeholder_t { name, value }
    }

    #[test]
    fn null_format_returns_none() {
        assert_eq!(format_placeholders(None, &[], 0), None);
    }

    #[test]
    fn replaces_placeholders() {
        let placeholders = [ph("%title", "some window"), ph("%app", "gimp")];
        let out = format_placeholders(Some("%title in %app"), &placeholders, 2).unwrap();
        assert_eq!(out, "some window in gimp");
    }

    #[test]
    fn multiple_occurrences() {
        let placeholders = [ph("%title", "x")];
        let out = format_placeholders(Some("%title and %title"), &placeholders, 1).unwrap();
        assert_eq!(out, "x and x");
    }

    #[test]
    fn text_without_percent_is_untouched() {
        // A placeholder name appearing without '%' is not replaced.
        let placeholders = [ph("%title", "x")];
        let out = format_placeholders(Some("title"), &placeholders, 1).unwrap();
        assert_eq!(out, "title");
    }

    #[test]
    fn unknown_placeholder_is_kept() {
        let placeholders = [ph("%title", "x")];
        assert_eq!(
            format_placeholders(Some("%titl"), &placeholders, 1).unwrap(),
            "%titl"
        );
        assert_eq!(
            format_placeholders(Some("%unknown"), &placeholders, 1).unwrap(),
            "%unknown"
        );
    }

    #[test]
    fn bare_percent_is_kept() {
        let placeholders = [ph("%title", "x")];
        assert_eq!(format_placeholders(Some("100%"), &placeholders, 1).unwrap(), "100%");
        assert_eq!(format_placeholders(Some("%"), &placeholders, 1).unwrap(), "%");
    }

    #[test]
    fn no_placeholders() {
        assert_eq!(format_placeholders(Some("plain text"), &[], 0).unwrap(), "plain text");
    }

    #[test]
    fn num_zero_disables_replacement() {
        let placeholders = [ph("%title", "x")];
        // C: for (int i = 0; i < num; i++) — num == 0 → no iteration.
        assert_eq!(format_placeholders(Some("%title"), &placeholders, 0).unwrap(), "%title");
    }

    #[test]
    fn empty_placeholder_value() {
        let placeholders = [ph("%title", "")];
        assert_eq!(format_placeholders(Some("[%title]"), &placeholders, 1).unwrap(), "[]");
    }

    #[test]
    fn multibyte_format_is_preserved() {
        let placeholders = [ph("%title", "日本語")];
        assert_eq!(
            format_placeholders(Some("→ %title!"), &placeholders, 1).unwrap(),
            "→ 日本語!"
        );
    }
}
