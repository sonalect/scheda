//! Shared test isolation. Not a test binary.
//!
//! Points this process at a git config path that does not exist so the
//! developer's `~/.gitconfig` cannot change outcomes. Creates no files.

use std::path::PathBuf;
use std::process;

/// Isolate this process from the host git configuration.
pub(crate) fn isolate_test_git_env() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let absent = std::env::temp_dir().join("scheda-absent-gitconfig");
        // SAFETY: Once serializes callers. Other test threads wait here
        // before they read these keys, so the environment is not torn.
        unsafe {
            std::env::set_var("GIT_CONFIG_GLOBAL", &absent);
            std::env::set_var("GIT_CONFIG_SYSTEM", &absent);
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        }
    });
}

/// Unique directory for one test's Vault tree.
///
/// Prefers Bazel `TEST_TMPDIR` so parallel `local` actions do not share
/// `/tmp` with linux-sandbox PID namespaces (`process::id()` is often 2).
pub(crate) fn scratch_root(prefix: &str, n: u64) -> PathBuf {
    let base = std::env::var_os("TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let tid = format!("{:?}", std::thread::current().id()).replace(['(', ')'], "");
    let root = base.join(format!("{prefix}-{}-{n}-{tid}", process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}
