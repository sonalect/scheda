//! Shared test isolation. Not a test binary.
//!
//! Points this process at a git config path that does not exist so the
//! developer's `~/.gitconfig` cannot change outcomes. Creates no files.

use std::path::PathBuf;
use std::process;
use std::sync::Arc;

use scheda::{ColumnKind, Error, IndexColumn, IndexDoc, IndexSchema, IndexValue, Projector};
use serde_json::Value;

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
    let _: fn(&[&str]) -> Arc<dyn Projector> = json_text_projector;
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

/// Test projector: named JSON object keys as find `Text` columns.
///
/// Walks nested objects so a column such as `city` under `meta` is found.
/// Non-JSON or a non-object root yields an empty [`IndexDoc`].
pub(crate) fn json_text_projector(columns: &[&str]) -> Arc<dyn Projector> {
    Arc::new(JsonTextProjector {
        columns: columns.iter().map(|name| (*name).to_owned()).collect(),
    })
}

struct JsonTextProjector {
    columns: Vec<String>,
}

impl Projector for JsonTextProjector {
    fn schema(&self, _collection: &str) -> Result<IndexSchema, Error> {
        Ok(IndexSchema {
            columns: self
                .columns
                .iter()
                .map(|name| {
                    IndexColumn::default()
                        .with_name(name.clone())
                        .with_kind(ColumnKind::Text)
                        .with_find(true)
                })
                .collect(),
            ..IndexSchema::default()
        })
    }

    fn project(&self, _collection: &str, _id: &str, raw: &[u8]) -> Result<IndexDoc, Error> {
        let Ok(text) = std::str::from_utf8(raw) else {
            return Ok(IndexDoc::default());
        };
        let Ok(Value::Object(map)) = serde_json::from_str(text) else {
            return Ok(IndexDoc::default());
        };
        let wanted: std::collections::BTreeSet<&str> =
            self.columns.iter().map(String::as_str).collect();
        let mut values = std::collections::BTreeMap::new();
        collect_named(&map, &wanted, &mut values);
        Ok(IndexDoc { values })
    }
}

fn collect_named(
    map: &serde_json::Map<String, Value>,
    wanted: &std::collections::BTreeSet<&str>,
    out: &mut std::collections::BTreeMap<String, IndexValue>,
) {
    for (key, value) in map {
        if wanted.contains(key.as_str())
            && !out.contains_key(key)
            && let Some(projected) = json_index_value(value)
        {
            out.insert(key.clone(), projected);
        }
        match value {
            Value::Object(nested) => collect_named(nested, wanted, out),
            Value::Array(items) => {
                for item in items {
                    if let Value::Object(nested) = item {
                        collect_named(nested, wanted, out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn json_index_value(value: &Value) -> Option<IndexValue> {
    match value {
        Value::Null => Some(IndexValue::Null),
        Value::String(text) => Some(IndexValue::Text(text.clone())),
        Value::Number(n) => Some(IndexValue::Text(n.to_string())),
        Value::Bool(b) => Some(IndexValue::Text(b.to_string())),
        Value::Array(items) if items.iter().all(Value::is_string) => Some(IndexValue::TextList(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        )),
        _ => None,
    }
}
