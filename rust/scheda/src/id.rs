//! Document `id` and collection-name slugs: the rules the library checks
//! because the contract cannot express them.

use std::sync::LazyLock;

use regex::Regex;

use scheda_api::v1::VaultErrorCode;

use crate::error::Fault;

fn invalid(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::InvalidArgument, message)
}

const MAX_BYTES: usize = 255;

static SLUG: LazyLock<Regex> =
    LazyLock::new(
        || match Regex::new(r"^[\p{L}\p{N}][\p{L}\p{N}\p{M}._+\-() @]*$") {
            Ok(re) => re,
            Err(_) => unreachable!("slug regex is static"),
        },
    );

/// Unicode letter or digit, then letters, digits, marks, `.`, `_`, `-`, `+`,
/// `(`, `)`, space, or `@`. At most 255 bytes. No leading/trailing whitespace.
/// No trailing `.`. Not a Windows device name.
pub(crate) fn require_slug(label: &str, value: &str) -> Result<(), Fault> {
    if value.is_empty() {
        return Err(invalid(format!("{label} is empty")));
    }
    if value.len() > MAX_BYTES {
        return Err(invalid(format!("{label} exceeds {MAX_BYTES} bytes")));
    }
    if value != value.trim() {
        return Err(invalid(format!(
            "{label} has leading or trailing whitespace"
        )));
    }
    if value.ends_with('.') {
        return Err(invalid(format!("{label} must not end with '.'")));
    }
    if is_windows_device(value) {
        return Err(invalid(format!("{label} is a reserved device name")));
    }
    if !SLUG.is_match(value) {
        return Err(invalid(format!("{label} is not a valid slug")));
    }
    Ok(())
}

/// A Document `id`: one slug, or `/`-separated slugs (each a directory,
/// last is the filename and may include an extension). The whole string is
/// at most 255 bytes, same as a flat slug. `journal` / `singleton` / collection
/// names still use [`require_slug`].
pub(crate) fn require_document_id(id: &str) -> Result<(), Fault> {
    if id.is_empty() {
        return Err(invalid("id is empty"));
    }
    if id.len() > MAX_BYTES {
        return Err(invalid(format!("id exceeds {MAX_BYTES} bytes")));
    }
    if id.contains('\\') {
        return Err(invalid("id is not a valid slug"));
    }
    if !id.contains('/') {
        return require_slug("id", id);
    }
    if id.starts_with('/') || id.ends_with('/') || id.contains("//") {
        return Err(invalid("id is not a valid slug"));
    }
    for component in id.split('/') {
        if component == "." || component == ".." {
            return Err(invalid("id is not a valid slug"));
        }
        require_slug("id", component)?;
    }
    Ok(())
}

fn is_windows_device(value: &str) -> bool {
    let stem = value.split('.').next().unwrap_or(value);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

#[cfg(test)]
mod tests {
    use scheda_api::v1::VaultErrorCode;

    use super::{require_document_id, require_slug};
    use crate::error::{Error, Fault};

    fn refused(result: Result<(), Fault>) -> bool {
        result.is_err_and(|fault| Error::from(fault).is(VaultErrorCode::InvalidArgument))
    }

    #[test]
    fn accepts_plain_slugs() {
        require_slug("id", "n1").unwrap();
        require_slug("id", "alpha").unwrap();
        require_slug("id", "team-notes").unwrap();
        require_slug("id", "Ünïcode (1)+@x_y.z").unwrap();
    }

    #[test]
    fn rejects_empty_slash_device_and_edges() {
        assert!(refused(require_slug("id", "")));
        assert!(refused(require_slug("id", "a/b")));
        assert!(refused(require_slug("id", "CON")));
        assert!(refused(require_slug("id", "com1.txt")));
        assert!(refused(require_slug("id", "alpha ")));
        assert!(refused(require_slug("id", ".hidden")));
        assert!(refused(require_slug("id", "dot.")));
        assert!(refused(require_slug("id", &"a".repeat(256))));
        require_slug("id", &"a".repeat(255)).unwrap();
    }

    #[test]
    fn document_id_allows_slash_separated_slugs() {
        require_document_id("alpha").unwrap();
        require_document_id("work/meeting").unwrap();
        require_document_id("2026-01-15/meeting").unwrap();
        require_document_id("2026/01/15/meeting").unwrap();
        require_document_id("foo.md").unwrap();
        require_document_id("work/meeting.toml").unwrap();
    }

    #[test]
    fn document_id_rejects_injection() {
        for id in [
            "",
            "../x",
            "x/../y",
            "./x",
            "CON/a",
            "a/CON",
            "a//b",
            "a/",
            "/a",
            r"a\b",
            ".hidden/a",
        ] {
            assert!(refused(require_document_id(id)), "{id:?}");
        }
        assert!(refused(require_document_id(&format!(
            "a/{}",
            "b".repeat(255)
        ))));
    }
}
