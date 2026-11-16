//! C: libi3/boolstr.c

use crate::safewrappers::strcasecmp;

/// C: libi3/boolstr.c:18 — Reports whether str represents the enabled state
/// (1, yes, true, …).
pub fn boolstr(str: &str) -> bool {
    strcasecmp(str, "1") == 0
        || strcasecmp(str, "yes") == 0
        || strcasecmp(str, "true") == 0
        || strcasecmp(str, "on") == 0
        || strcasecmp(str, "enable") == 0
        || strcasecmp(str, "active") == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_states() {
        for s in ["1", "YES", "Yes", "true", "TRUE", "on", "ON", "enable", "ENABLE", "active"] {
            assert!(boolstr(s), "{} should be true", s);
        }
    }

    #[test]
    fn disabled_states() {
        for s in ["0", "no", "false", "off", "disable", "", "maybe"] {
            assert!(!boolstr(s), "{} should be false", s);
        }
    }
}
