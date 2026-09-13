//! Where the socket lives, and who may reach it — §7.1.

use std::io;
use std::path::{Path, PathBuf};

/// The socket's name inside the runtime directory.
pub const SOCKET: &str = "engine.sock";
/// The single-instance lock, beside it.
pub const LOCK: &str = "engine.lock";

/// Where the engine listens.
///
/// **`$GMUSE_SOCKET` wins outright**, because a test and a second engine both
/// need somewhere else to be and neither should have to move the user's real
/// one. Otherwise `$XDG_RUNTIME_DIR/gmuse/`, which is the right answer on Linux
/// — it is per-user, mode `0700` already, and cleared on logout.
///
/// `$TMPDIR` is the fallback and is not a bad one on macOS, where it is
/// per-user (`/var/folders/…`) rather than the shared `/tmp`. On a system where
/// it is shared, the `0700` directory below is what keeps this private.
pub fn default_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os("GMUSE_SOCKET") {
        return PathBuf::from(explicit);
    }
    runtime_dir().join(SOCKET)
}

/// The directory the socket and its lock share.
pub fn runtime_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("gmuse");
    }
    std::env::temp_dir().join("gmuse")
}

/// Create the directory a socket will live in, private to this user.
///
/// **`0700`, set after creation rather than trusted from the umask.** A umask
/// of `022` would leave it world-readable, and the socket's own `0600` does not
/// help if the directory can be listed and the socket connected to. Applied
/// even when the directory already existed, because an earlier run under a
/// laxer umask is exactly the case that matters.
pub fn prepare_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Restrict a bound socket to this user.
///
/// On Linux the permissions on a socket file are honoured by `connect`; on some
/// BSDs they historically were not, which is why the `0700` directory above is
/// the load-bearing half and this is the belt to its braces.
pub fn restrict(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialised: these mutate the process environment, and two running at
    /// once would read each other's writes.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (k, v) in &self.0 {
                match v {
                    Some(v) => unsafe { std::env::set_var(k, v) },
                    None => unsafe { std::env::remove_var(k) },
                }
            }
        }
    }
    fn scoped(keys: &[&'static str]) -> Restore {
        Restore(keys.iter().map(|k| (*k, std::env::var_os(k))).collect())
    }

    #[test]
    fn an_explicit_socket_wins_outright() {
        let _guard = ENV.lock().unwrap();
        let _restore = scoped(&["GMUSE_SOCKET", "XDG_RUNTIME_DIR"]);
        unsafe {
            std::env::set_var("GMUSE_SOCKET", "/somewhere/else.sock");
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        }
        assert_eq!(default_path(), PathBuf::from("/somewhere/else.sock"));
    }

    #[test]
    fn otherwise_the_runtime_dir_is_used() {
        let _guard = ENV.lock().unwrap();
        let _restore = scoped(&["GMUSE_SOCKET", "XDG_RUNTIME_DIR"]);
        unsafe {
            std::env::remove_var("GMUSE_SOCKET");
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        }
        assert_eq!(
            default_path(),
            PathBuf::from("/run/user/1000/gmuse/engine.sock")
        );
    }

    /// **An empty `XDG_RUNTIME_DIR` is not a directory.** Some login setups
    /// export it blank, and joining onto it would produce `gmuse/engine.sock`
    /// relative to the working directory — a socket in whatever folder the
    /// engine happened to start in.
    #[test]
    fn an_empty_runtime_dir_falls_back_to_the_temp_dir() {
        let _guard = ENV.lock().unwrap();
        let _restore = scoped(&["GMUSE_SOCKET", "XDG_RUNTIME_DIR"]);
        unsafe {
            std::env::remove_var("GMUSE_SOCKET");
            std::env::set_var("XDG_RUNTIME_DIR", "");
        }
        let path = default_path();
        assert!(path.is_absolute(), "{path:?} must not be relative");
        assert!(path.starts_with(std::env::temp_dir()), "{path:?}");
    }

    #[test]
    fn the_directory_is_private_even_if_it_already_existed() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("gmuse-sock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // A laxer mode, as an earlier run under a permissive umask would leave.
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();

        prepare_dir(&base).unwrap();
        let mode = std::fs::metadata(&base).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "got {mode:o}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
