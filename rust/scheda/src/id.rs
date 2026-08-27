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
    use super::require_slug;
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
}
