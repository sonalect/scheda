//! Shared test isolation. Not a test binary.
//!
//! Points this process at a git config path that does not exist so the
//! developer's `~/.gitconfig` cannot change outcomes. Creates no files.

use std::path::PathBuf;
use std::process;
use std::sync::Arc;

use scheda::{ColumnKind, Error, IndexColumn, IndexDoc, IndexSchema, IndexValue, Projector};

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

/// Matches the CLI whole-file Projector: one `body` text column, find + fts +
/// vector, value is the entire UTF-8 Record (non-UTF-8 → empty `IndexDoc`).
pub(crate) fn cli_body_projector() -> Arc<dyn Projector> {
    Arc::new(CliBodyProjector)
}

struct CliBodyProjector;

impl Projector for CliBodyProjector {
    fn schema(&self, _collection: &str) -> Result<IndexSchema, Error> {
        Ok(IndexSchema {
            columns: vec![
                IndexColumn::default()
                    .with_name("body")
                    .with_kind(ColumnKind::Text)
                    .with_find(true)
                    .with_fts(true)
                    .with_vector(true),
            ],
            ..IndexSchema::default()
        })
    }

    fn project(&self, _collection: &str, _id: &str, raw: &[u8]) -> Result<IndexDoc, Error> {
        let Ok(text) = str::from_utf8(raw) else {
            return Ok(IndexDoc::default());
        };
        Ok(IndexDoc {
            values: [(String::from("body"), IndexValue::Text(text.to_owned()))].into(),
        })
    }
}
