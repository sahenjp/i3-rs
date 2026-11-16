//! C: libi3/get_config_path.c

use crate::path_exists::path_exists;
use crate::resolve_tilde::resolve_tilde;
use crate::SYSCONFDIR;
use std::env;
use std::sync::Mutex;

/// C: libi3/get_config_path.c:24 static const char *saved_configpath
static SAVED_CONFIGPATH: Mutex<Option<String>> = Mutex::new(None);

/// C: libi3/get_config_path.c:22 — Get the path of the first configuration
/// file found. If override_configpath is specified, that path is returned
/// and saved for further calls. Otherwise, checks the home directory first,
/// then the system directory, always taking into account the XDG Base
/// Directory Specification ($XDG_CONFIG_HOME, $XDG_CONFIG_DIRS).
pub fn get_config_path(override_configpath: Option<&str>, use_system_paths: bool) -> Option<String> {
    let mut saved = SAVED_CONFIGPATH.lock().unwrap_or_else(|e| e.into_inner());

    if let Some(oc) = override_configpath {
        *saved = Some(oc.to_string());
        return saved.clone();
    }

    if let Some(sc) = saved.as_ref() {
        return Some(sc.clone());
    }

    /* 1: check for $XDG_CONFIG_HOME/i3/config */
    // C: if ((xdg_config_home = getenv("XDG_CONFIG_HOME")) == NULL)
    let xdg_config_home = env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| "~/.config".to_string());
    let xdg_config_home = resolve_tilde(&xdg_config_home);
    let config_path = format!("{}/i3/config", xdg_config_home);
    if path_exists(&config_path) {
        return Some(config_path);
    }

    /* 2: check the traditional path under the home directory */
    let config_path = resolve_tilde("~/.i3/config");
    if path_exists(&config_path) {
        return Some(config_path);
    }

    /* The below paths are considered system-level, and can be skipped if the
     * caller only wants user-level configs. */
    if !use_system_paths {
        return None;
    }

    /* 3: check for $XDG_CONFIG_DIRS/i3/config */
    // C: if ((xdg_config_dirs = getenv("XDG_CONFIG_DIRS")) == NULL)
    let xdg_config_dirs = env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| format!("{}/xdg", SYSCONFDIR));
    // C: char *tok = strtok(buf, ":"); — strtok() skips empty tokens.
    for tok in xdg_config_dirs.split(':').filter(|t| !t.is_empty()) {
        let tok = resolve_tilde(tok);
        let config_path = format!("{}/i3/config", tok);
        if path_exists(&config_path) {
            return Some(config_path);
        }
    }

    /* 4: check the traditional path under /etc */
    let config_path = format!("{}/i3/config", SYSCONFDIR);
    if path_exists(&config_path) {
        return Some(config_path);
    }

    None
}

/// Test-only reset of the C static saved_configpath.
#[cfg(test)]
pub(crate) fn reset_saved_configpath_for_tests() {
    *SAVED_CONFIGPATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        crate::test_util::env_lock()
    }

    struct TempHome(std::path::PathBuf);
    impl TempHome {
        fn new(tag: &str) -> TempHome {
            let dir = std::env::temp_dir().join(format!("i3rs-cfg-{}-{}", tag, std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempHome(dir)
        }
        fn path(&self) -> String {
            self.0.to_str().unwrap().to_string()
        }
        fn write(&self, rel: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"# i3 config
").unwrap();
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn override_is_saved_and_returned() {
        let _g = lock_env();
        reset_saved_configpath_for_tests();
        assert_eq!(
            get_config_path(Some("/override/config"), true),
            Some("/override/config".to_string())
        );
        // saved for further calls
        assert_eq!(
            get_config_path(None, true),
            Some("/override/config".to_string())
        );
    }

    #[test]
    fn search_order_xdg_config_home_first() {
        let _g = lock_env();
        reset_saved_configpath_for_tests();
        let home = TempHome::new("xdg");
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", format!("{}/config", home.path()));
        std::env::set_var("XDG_CONFIG_DIRS", format!("{}/sysdirs", home.path()));
        // both ~/.config and ~/.i3 exist → XDG_CONFIG_HOME wins
        home.write("config/i3/config");
        home.write(".i3/config");
        assert_eq!(
            get_config_path(None, true),
            Some(format!("{}/config/i3/config", home.path()))
        );
    }

    #[test]
    fn search_order_falls_back_to_home_i3() {
        let _g = lock_env();
        reset_saved_configpath_for_tests();
        let home = TempHome::new("homei3");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_DIRS", format!("{}/sysdirs", home.path()));
        home.write(".i3/config");
        assert_eq!(
            get_config_path(None, true),
            Some(format!("{}/.i3/config", home.path()))
        );
    }

    #[test]
    fn no_user_config_and_no_system_paths() {
        let _g = lock_env();
        reset_saved_configpath_for_tests();
        let home = TempHome::new("empty");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_DIRS", format!("{}/none", home.path()));
        assert_eq!(get_config_path(None, false), None);
    }

    #[test]
    fn xdg_config_dirs_is_searched() {
        let _g = lock_env();
        reset_saved_configpath_for_tests();
        let home = TempHome::new("dirs");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_DIRS", format!("{}::{}", home.path(), home.path()));
        home.write("i3/config"); // <home>/i3/config via the first dir entry
        assert_eq!(
            get_config_path(None, true),
            Some(format!("{}/i3/config", home.path()))
        );
    }

    #[test]
    fn system_default_when_nothing_found() {
        let _g = lock_env();
        reset_saved_configpath_for_tests();
        let home = TempHome::new("sysdefault");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_DIRS", format!("{}/none", home.path()));
        // The last fallback is SYSCONFDIR "/i3/config" (embedded at build
        // time, default "/etc").
        if crate::path_exists::path_exists(&format!("{}/i3/config", SYSCONFDIR)) {
            assert_eq!(
                get_config_path(None, true),
                Some(format!("{}/i3/config", SYSCONFDIR))
            );
        } else {
            assert_eq!(get_config_path(None, true), None);
        }
    }
}
