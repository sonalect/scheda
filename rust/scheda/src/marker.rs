//! The Vault marker: `{vault}/.scheda.yaml`, committed to git.
//!
//! The file is the YAML of `VaultMarker` and holds `version: 2`, the
//! generation of the Vault format. It is tracked, so a clone carries it;
//! `.scheda/` is gitignored and does not. A git work tree that holds it is
//! a Vault.
//!
//! The marker is read before any other file of the Vault. Its `version` is
//! looked at before the rest of the file is decoded, so a later generation
//! may change everything but that one key and an earlier Scheda still says
//! which generation it met.

// `make` is the writer of a new Vault, which the next layer brings.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the layer that makes a Vault is not built yet")
)]

use std::path::Path;

use scheda_api::v1::{VaultErrorCode, VaultMarker};
use yaml_serde::Value;

use crate::error::{Context, Error, Fault, io};
use crate::stored::{self, Format, Stored, Unknowns};

/// Vault-relative name of the marker file.
pub(crate) const FILE: &str = ".scheda.yaml";

/// The format generation this build reads and writes.
pub(crate) const GENERATION: i32 = 2;

/// The marker of `vault`, or `None` when the file is not there.
///
/// A marker that is there is parsed, its `version` is compared with
/// [`GENERATION`] (another generation is `GENERATION`), and only then is the
/// rest decoded, with keys it does not know read past, and validated (not
/// YAML, a missing or mistyped `version`: `INVALID_ARGUMENT`). Reads only.
/// Every failure is about the path [`FILE`].
pub(crate) fn read(vault: &Path) -> Result<Option<Stored<VaultMarker>>, Error> {
    let bytes = match std::fs::read(vault.join(FILE)) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(io(err))
                .map_err(|fault| fault.with_path(FILE))
                .context("read the marker");
        }
    };
    parse(&bytes)
        .map(Some)
        .map_err(|fault| fault.with_path(FILE))
        .context("read the marker")
}

fn parse(bytes: &[u8]) -> Result<Stored<VaultMarker>, Fault> {
    let tree = stored::parse_tree(bytes, Format::Yaml)?;
    if let Value::Mapping(map) = &tree
        && let Some(Value::Number(found)) = map.get("version")
        && found.as_i64() != Some(i64::from(GENERATION))
    {
        return Err(Fault::new(
            VaultErrorCode::Generation,
            format!(
                "the Vault is of generation {found}; this version reads generation {GENERATION}"
            ),
        ));
    }
    stored::decode(&tree)
}

/// The bytes of a marker of this build's generation, for a new Vault.
pub(crate) fn make() -> Result<Vec<u8>, Error> {
    let marker = VaultMarker::default().with_version(GENERATION);
    stored::encode(&marker, &Unknowns::none(), Format::Yaml)
        .map_err(|fault| fault.with_path(FILE))
        .context("encode the marker")
}

#[cfg(test)]
mod tests {
    use scheda_api::v1::VaultErrorCode;

    use super::{FILE, GENERATION, make, parse, read};
    use crate::error::Error;

    fn code(bytes: &[u8]) -> Error {
        Error::from(parse(bytes).unwrap_err())
    }

    #[test]
    fn the_marker_made_is_accepted_and_is_of_this_generation() {
        let bytes = make().unwrap();
        assert_eq!(bytes, b"version: 2\n");
        let stored = parse(&bytes).unwrap();
        assert_eq!(stored.message.version, Some(GENERATION));
        assert!(stored.unknown.is_empty());
    }

    #[test]
    fn another_generation_is_generation() {
        for text in [
            "version: 1\n",
            "version: 3\n",
            "version: 0\n",
            "version: 2.5\n",
        ] {
            assert!(
                code(text.as_bytes()).is(VaultErrorCode::Generation),
                "{text}"
            );
        }
        // The rest of the file is not looked at.
        assert!(code(b"version: 1\nfuture: [\n  x: y\n").is(VaultErrorCode::InvalidArgument));
        assert!(code(b"version: 1\nfuture: {a: 1}\n").is(VaultErrorCode::Generation));
    }

    #[test]
    fn an_unknown_key_is_read_past() {
        let stored = parse(b"version: 2\nname: x\nlater:\n  a: 1\n").unwrap();
        let paths: Vec<String> = stored
            .unknown
            .iter()
            .map(|field| crate::stored::path_string(field.path()))
            .collect();
        assert_eq!(paths, ["name", "later"]);
    }

    #[test]
    fn a_missing_or_mistyped_version_is_invalid_argument() {
        for text in [
            "{}\n",
            "",
            "version: two\n",
            "- 2\n",
            "version: [2]\n",
            "version: [2\n",
        ] {
            assert!(
                code(text.as_bytes()).is(VaultErrorCode::InvalidArgument),
                "{text:?}"
            );
        }
    }

    #[test]
    fn read_finds_the_file_or_none_and_names_the_path_of_a_failure() {
        let base = std::env::var_os("TEST_TMPDIR")
            .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
        let dir = base.join(format!("scheda-marker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read(&dir).unwrap().is_none());
        std::fs::write(dir.join(FILE), b"version: 2\n").unwrap();
        assert!(read(&dir).unwrap().is_some());
        std::fs::write(dir.join(FILE), b"version: 1\n").unwrap();
        let error = read(&dir).map(|_| ()).unwrap_err();
        assert!(error.is(VaultErrorCode::Generation));
        let fault = error.downcast_ref::<crate::error::Fault>().unwrap();
        assert_eq!(fault.path(), Some(FILE));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
