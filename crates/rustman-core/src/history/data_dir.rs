//! The platform data directory, where ad-hoc requests made outside any
//! workspace are recorded (request-history spec "Storage location").

use std::path::PathBuf;

/// Overrides the platform data directory (tests, sandboxes, CI).
pub const DATA_DIR_VAR: &str = "RUSTMAN_DATA_DIR";

/// `<data_dir>/rustman`: `$RUSTMAN_DATA_DIR` when set, else the platform
/// data directory (`$XDG_DATA_HOME` or `~/.local/share` on Linux,
/// `~/Library/Application Support` on macOS, `%APPDATA%` on Windows).
pub fn rustman_data_dir() -> Option<PathBuf> {
    rustman_data_dir_with(|name| std::env::var_os(name).filter(|v| !v.is_empty()))
}

fn rustman_data_dir_with(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(dir) = var(DATA_DIR_VAR) {
        return Some(PathBuf::from(dir));
    }
    platform_data_dir(&var).map(|dir| dir.join("rustman"))
}

#[cfg(windows)]
fn platform_data_dir(var: &impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    var("APPDATA").map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn platform_data_dir(var: &impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    var("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn platform_data_dir(var: &impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".local/share")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn the_override_wins() {
        let dir = rustman_data_dir_with(vars(&[(DATA_DIR_VAR, "/x/data"), ("HOME", "/h")]));
        assert_eq!(dir, Some(PathBuf::from("/x/data")));
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn linux_uses_xdg_then_home() {
        let xdg = rustman_data_dir_with(vars(&[("XDG_DATA_HOME", "/xdg"), ("HOME", "/h")]));
        assert_eq!(xdg, Some(PathBuf::from("/xdg/rustman")));
        let home = rustman_data_dir_with(vars(&[("HOME", "/h")]));
        assert_eq!(home, Some(PathBuf::from("/h/.local/share/rustman")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_application_support() {
        let dir = rustman_data_dir_with(vars(&[("HOME", "/Users/u")]));
        assert_eq!(
            dir,
            Some(PathBuf::from(
                "/Users/u/Library/Application Support/rustman"
            ))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_appdata() {
        let dir = rustman_data_dir_with(vars(&[("APPDATA", r"C:\Users\u\AppData\Roaming")]));
        assert_eq!(
            dir,
            Some(PathBuf::from(r"C:\Users\u\AppData\Roaming").join("rustman"))
        );
    }

    #[test]
    fn no_home_means_no_data_dir() {
        assert_eq!(rustman_data_dir_with(vars(&[])), None);
    }
}
