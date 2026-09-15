//! Machine-local writer identity for one Vault clone.

use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::path::Path;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Error;
use crate::error::{ContextIo, bail};
use crate::store;

/// Longest writer token that still fits in a journal segment file name.
pub(crate) const MAX_LEN: usize = 32;

/// `{vault}/.scheda/writer` — gitignored with the rest of `.scheda/`.
pub(crate) fn path(vault: &Path) -> std::path::PathBuf {
    vault.join(".scheda").join("writer")
}

/// Reject a caller- or file-supplied token that cannot sit in a segment name.
pub(crate) fn validate(token: &str) -> Result<(), Error> {
    if token.is_empty() {
        bail!(Error::invalid("writer identity is empty"));
    }
    if token.len() > MAX_LEN {
        bail!(Error::invalid(format!(
            "writer identity exceeds {MAX_LEN} bytes"
        )));
    }
    if starts_with_hex_seq(token) {
        bail!(Error::invalid(
            "writer identity must not start with eight hexadecimal digits"
        ));
    }
    let bytes = token.as_bytes();
    if !is_alnum(bytes[0])
        || !is_alnum(bytes[bytes.len() - 1])
        || !bytes.iter().copied().all(is_alnum_or_hyphen)
    {
        bail!(Error::invalid(
            "writer identity must be lowercase alphanumeric plus hyphen"
        ));
    }
    Ok(())
}

/// Identity of the process that writes this Vault clone.
///
/// Unique across independent clones of one Vault, so journal segments
/// named with this token cannot collide. Sixteen lowercase hex digits.
/// Generated when `{vault}/.scheda/writer` is absent.
pub(crate) fn generate() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = u128::from(process::id());
    let hashed = u128::from(RandomState::new().hash_one((pid, nanos)));
    let mixed = hashed ^ nanos ^ pid.wrapping_shl(32);
    format!("{:016x}", mixed as u64)
}

/// Load `{vault}/.scheda/writer`, or generate and persist it.
///
/// A set `override_token` wins, is validated, and is written so a later
/// `open` without the option keeps the same identity.
pub(crate) fn load_or_create(vault: &Path, override_token: Option<&str>) -> Result<String, Error> {
    if let Some(token) = override_token {
        validate(token)?;
        persist(vault, token)?;
        return Ok(token.to_owned());
    }
    match fs::read(path(vault)) {
        Ok(bytes) => {
            let text = str::from_utf8(&bytes)
                .map_err(|_| Error::invalid("writer identity file is not UTF-8"))?;
            let token = text.trim();
            validate(token)?;
            Ok(token.to_owned())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let token = generate();
            persist(vault, &token)?;
            Ok(token)
        }
        Err(err) => Err(err).context("[Vault::open] read writer identity"),
    }
}

fn persist(vault: &Path, token: &str) -> Result<(), Error> {
    let scheda = vault.join(".scheda");
    fs::create_dir_all(&scheda).context("[Vault::open] create .scheda for writer")?;
    store::restrict_dir(&scheda)?;
    let path = path(vault);
    store::atomic_write(&path, format!("{token}\n").as_bytes())?;
    store::restrict_file(&path)
}

/// True when the first hyphen-separated part is exactly eight hex digits
/// (a journal sequence token). A bare eight-hex identity is the same rule
/// with no hyphen.
fn starts_with_hex_seq(token: &str) -> bool {
    token.split('-').next().is_some_and(is_hex_seq)
}

fn is_hex_seq(part: &str) -> bool {
    part.len() == 8 && part.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

fn is_alnum_or_hyphen(b: u8) -> bool {
    is_alnum(b) || b == b'-'
}

#[cfg(test)]
mod tests {
    use super::{generate, validate};

    #[test]
    fn generated_token_is_valid() {
        let token = generate();
        assert_eq!(token.len(), 16);
        validate(&token).unwrap();
    }

    #[test]
    fn empty_and_illegal_tokens_are_invalid() {
        assert!(validate("").is_err());
        assert!(validate("Alice").is_err());
        assert!(validate("a_b").is_err());
        assert!(validate("-abc").is_err());
        assert!(validate("abc-").is_err());
        assert!(validate("00000001").is_err());
        validate("alice").unwrap();
        validate("a").unwrap();
        validate("host-1").unwrap();
        validate("node-abcdef01").unwrap();
    }

    #[test]
    fn eight_hex_prefix_plus_hyphen_is_invalid() {
        let err = validate("abcdef01-node").unwrap_err();
        assert_eq!(err.known_code(), Some(crate::error::Code::Invalid));
        assert_eq!(
            err.message.as_deref(),
            Some("writer identity must not start with eight hexadecimal digits")
        );
        let err = validate("00000001").unwrap_err();
        assert_eq!(
            err.message.as_deref(),
            Some("writer identity must not start with eight hexadecimal digits")
        );
    }
}
