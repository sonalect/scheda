//! The Vault marker: `{vault}/.scheda.yaml`, committed to git.
//!
//! The file holds `version: 1`, the generation of the Vault format. It is
//! tracked, so a clone carries it; `.scheda/` is gitignored and does not. A
//! git work tree that holds it is a Vault.

use std::path::Path;

use crate::error::{Code, ContextAs, io_code};
use crate::json::vault_marker_from_value;
use crate::{Error, store};
use serde_json::Value;

/// Vault-relative name of the marker file.
pub(crate) const FILE: &str = ".scheda.yaml";

/// Marker text written by this version.
const CONTENT: &str = "version: 1\n";

/// Whether `vault` holds a marker. A marker that is there is read and
/// validated: not YAML, an unknown key, a missing `version`, or a `version`
/// other than 1 is `invalid`; text that is not UTF-8 is `not_utf8`; an I/O
/// failure keeps its own code. `path` is the marker. Reads only.
pub(crate) fn read(vault: &Path) -> Result<bool, Error> {
    let file = vault.join(FILE);
    let bytes = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(Error::new(io_code(&err), err.to_string())
                .context("[Marker::read] read marker")
                .with_path(FILE));
        }
    };
    parse(&bytes).map_err(|err| err.with_path(FILE))?;
    Ok(true)
}

fn parse(bytes: &[u8]) -> Result<(), Error> {
    let text = std::str::from_utf8(bytes).context(Code::NotUtf8, "[Marker::read] marker text")?;
    let value: Value =
        yaml_serde::from_str(text).context(Code::Invalid, "[Marker::read] parse marker YAML")?;
    vault_marker_from_value(&value).map_err(|err| err.context("[Marker::read] validate marker"))?;
    Ok(())
}

/// Write the marker unless the file is already there. Returns `true` when it
/// wrote. The caller commits.
pub(crate) fn ensure(vault: &Path) -> Result<bool, Error> {
    let file = vault.join(FILE);
    if file.exists() {
        return Ok(false);
    }
    store::atomic_write(&file, CONTENT.as_bytes()).map_err(|err| err.with_path(FILE))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{CONTENT, parse};
    use crate::Code;

    #[test]
    fn written_content_is_accepted() {
        parse(CONTENT.as_bytes()).unwrap();
    }

    #[test]
    fn other_version_is_invalid() {
        let err = parse(b"version: 2\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn missing_version_is_invalid() {
        let err = parse(b"{}\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn unknown_key_is_invalid() {
        let err = parse(b"version: 1\nname: x\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn unreadable_yaml_is_invalid() {
        let err = parse(b"version: [1\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn empty_file_is_invalid() {
        let err = parse(b"").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn not_utf8_is_not_utf8() {
        let err = parse(&[0xff, 0xfe]).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::NotUtf8));
    }
}
