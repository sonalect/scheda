//! Record `id` and collection-name slugs. Not proto.

use std::sync::LazyLock;

use regex::Regex;

use crate::Error;
use crate::error::bail;

const MAX_BYTES: usize = 255;

/// YAML/JSON key for Record identity. Always `id`.
pub(crate) const FIELD: &str = "id";

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
pub(crate) fn require_slug(label: &str, value: &str) -> Result<(), Error> {
    if value.is_empty() {
        bail!(Error::invalid(format!("{label} is empty")));
    }
    if value.len() > MAX_BYTES {
        bail!(Error::invalid(format!("{label} exceeds {MAX_BYTES} bytes")));
    }
    if value != value.trim() {
        bail!(Error::invalid(format!(
            "{label} has leading or trailing whitespace"
        )));
    }
    if value.ends_with('.') {
        bail!(Error::invalid(format!("{label} must not end with '.'")));
    }
    if is_windows_device(value) {
        bail!(Error::invalid(format!("{label} is a reserved device name")));
    }
    if !SLUG.is_match(value) {
        bail!(Error::invalid(format!("{label} is not a valid slug")));
    }
    Ok(())
}

/// `document` Record `id`: one slug, or `/`-separated slugs (each a directory,
/// last is the filename and may include an extension). The whole string is
/// at most 255 bytes, same as a flat slug. `journal` / `singleton` / collection
/// names still use [`require_slug`].
pub(crate) fn require_document_id(id: &str) -> Result<(), Error> {
    if id.is_empty() {
        bail!(Error::invalid("id is empty"));
    }
    if id.len() > MAX_BYTES {
        bail!(Error::invalid(format!("id exceeds {MAX_BYTES} bytes")));
    }
    if id.contains('\\') {
        bail!(Error::invalid("id is not a valid slug"));
    }
    if !id.contains('/') {
        return require_slug("id", id);
    }
    if id.starts_with('/') || id.ends_with('/') || id.contains("//") {
        bail!(Error::invalid("id is not a valid slug"));
    }
    for component in id.split('/') {
        if component == "." || component == ".." {
            bail!(Error::invalid("id is not a valid slug"));
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
    use super::{require_document_id, require_slug};
    use crate::error::Code;

    #[test]
    fn accepts_lib7_examples() {
        require_slug("id", "n1").unwrap();
        require_slug("id", "alpha").unwrap();
        require_slug("id", "team-notes").unwrap();
    }

    #[test]
    fn rejects_empty_slash_and_device() {
        assert_eq!(
            require_slug("id", "").unwrap_err().known_code(),
            Some(Code::Invalid)
        );
        assert_eq!(
            require_slug("id", "a/b").unwrap_err().known_code(),
            Some(Code::Invalid)
        );
        assert_eq!(
            require_slug("id", "CON").unwrap_err().known_code(),
            Some(Code::Invalid)
        );
        assert_eq!(
            require_slug("id", "alpha ").unwrap_err().known_code(),
            Some(Code::Invalid)
        );
        assert_eq!(
            require_slug("id", ".hidden").unwrap_err().known_code(),
            Some(Code::Invalid)
        );
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
            "CON/a",
            "a/CON",
            "a//b",
            "a/",
            "/a",
            r"a\b",
            ".hidden/a",
        ] {
            assert_eq!(
                require_document_id(id).unwrap_err().known_code(),
                Some(Code::Invalid),
                "{id:?}"
            );
        }
        assert_eq!(
            require_slug("id", "a/b").unwrap_err().known_code(),
            Some(Code::Invalid)
        );
    }
}
