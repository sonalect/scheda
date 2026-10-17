//! Age v1 ASCII armor: seal any bytes, open them again. The only module
//! that names `age`.
//!
//! A Document is bytes (D20), so the plaintext is not asked to be text. The
//! armor is PEM, `\n` only, with no byte order mark. A failure here is
//! `CRYPTO`, and its message carries neither the plaintext nor the secret
//! nor key material.

use std::io::Read;

use age::secrecy::SecretString;
use scheda_api::v1::encryption::Secret;
use scheda_api::v1::{Encryption, VaultErrorCode};

use crate::error::{Error, Fault};

const ARMOR_BEGIN: &[u8] = b"-----BEGIN AGE ENCRYPTED FILE-----";
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

fn crypto(message: &str) -> Fault {
    Fault::new(VaultErrorCode::Crypto, message)
}

/// The secret of a handle, or `CRYPTO` when it has none.
fn secret(encryption: Option<&Encryption>) -> Result<&Secret, Fault> {
    encryption
        .and_then(|encryption| encryption.secret.as_ref())
        .ok_or_else(|| crypto("an encrypted collection needs a secret on the handle"))
}

/// Seals `plaintext` into age v1 ASCII armor.
///
/// A passphrase wraps the key with age scrypt; an identity
/// (`AGE-SECRET-KEY-…`) with X25519. No secret, or one that is not usable,
/// is `CRYPTO`.
///
/// The `age` crate writes `\r\n` on Windows. The CR is dropped here, so the
/// bytes on disk are LF-only on every system.
pub(crate) fn seal(plaintext: &[u8], encryption: Option<&Encryption>) -> Result<Vec<u8>, Fault> {
    let armored = match secret(encryption)? {
        Secret::Passphrase(passphrase) => {
            let recipient = age::scrypt::Recipient::new(SecretString::from(passphrase.clone()));
            age::encrypt_and_armor(&recipient, plaintext)
        }
        Secret::Identity(identity) => {
            let recipient = parse_x25519(identity)?.to_public();
            age::encrypt_and_armor(&recipient, plaintext)
        }
    };
    let armored =
        armored.map_err(|err| crypto("the bytes cannot be encrypted").with_source(err))?;
    Ok(lf_only(armored))
}

/// Opens age v1 ASCII armor into the plaintext bytes.
///
/// Bytes that are not armor, a truncated file, no secret, and a wrong secret
/// are `CRYPTO`.
pub(crate) fn open(armor: &[u8], encryption: Option<&Encryption>) -> Result<Vec<u8>, Fault> {
    if armor.starts_with(UTF8_BOM) || armor.contains(&b'\r') || !armor.starts_with(ARMOR_BEGIN) {
        return Err(crypto("the file is not age armor"));
    }
    let secret = secret(encryption)?;
    let reader = age::armor::ArmoredReader::new(armor);
    let decryptor = age::Decryptor::new_buffered(reader)
        .map_err(|err| crypto("the file will not decrypt").with_source(err))?;
    match secret {
        Secret::Passphrase(passphrase) => {
            let identity = age::scrypt::Identity::new(SecretString::from(passphrase.clone()));
            finish(decryptor, &identity)
        }
        Secret::Identity(identity) => finish(decryptor, &parse_x25519(identity)?),
    }
}

fn finish<R: Read>(
    decryptor: age::Decryptor<R>,
    identity: &dyn age::Identity,
) -> Result<Vec<u8>, Fault> {
    let mut reader = decryptor
        .decrypt(std::iter::once(identity))
        .map_err(|err| crypto("the file will not decrypt").with_source(err))?;
    let mut plaintext = Vec::new();
    reader
        .read_to_end(&mut plaintext)
        .map_err(|err| crypto("the file will not decrypt").with_source(err))?;
    Ok(plaintext)
}

/// Drops CR so the armor is LF-only. The `age` crate uses `\r\n` on Windows.
fn lf_only(armor: String) -> Vec<u8> {
    let mut bytes = armor.into_bytes();
    bytes.retain(|&byte| byte != b'\r');
    bytes
}

/// The first `AGE-SECRET-KEY-…` line of an `age-keygen` file, for
/// `Encryption.identity`.
///
/// Comment lines (`#`), blank lines, and a byte order mark are skipped.
/// More text after the key, or a key that does not parse, is
/// `INVALID_ARGUMENT`. The returned string is the key line only. The error
/// does not include the secret.
///
/// # Errors
///
/// `INVALID_ARGUMENT` when the text holds no usable key.
pub fn parse_age_identity(text: &str) -> Result<String, Error> {
    identity_line(text).ok_or_else(|| {
        Fault::new(
            VaultErrorCode::InvalidArgument,
            "the age identity file is malformed",
        )
        .into()
    })
}

fn parse_x25519(identity: &str) -> Result<age::x25519::Identity, Fault> {
    identity_line(identity)
        .and_then(|line| line.parse().ok())
        .ok_or_else(|| crypto("the secret is missing or wrong"))
}

fn identity_line(text: &str) -> Option<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if found.is_some() || !line.starts_with("AGE-SECRET-KEY-") {
            return None;
        }
        found = Some(line);
    }
    let line = found?;
    line.parse::<age::x25519::Identity>().ok()?;
    Some(line.to_owned())
}

/// A fresh X25519 identity as an [`Encryption`], for the tests of the
/// modules that read and write encrypted files.
#[cfg(test)]
pub(crate) fn test_identity() -> Encryption {
    use age::secrecy::ExposeSecret;
    Encryption {
        secret: Some(Secret::Identity(
            age::x25519::Identity::generate()
                .to_string()
                .expose_secret()
                .to_owned(),
        )),
        ..Encryption::default()
    }
}

/// A passphrase as an [`Encryption`], for the same tests.
#[cfg(test)]
pub(crate) fn test_passphrase(passphrase: &str) -> Encryption {
    Encryption {
        secret: Some(Secret::Passphrase(passphrase.to_owned())),
        ..Encryption::default()
    }
}

#[cfg(test)]
mod tests {
    use scheda_api::v1::VaultErrorCode;

    use super::{
        ARMOR_BEGIN, lf_only, open, parse_age_identity, seal, test_identity, test_passphrase,
    };
    use crate::error::Error;

    const PASS: &str = "unit-test-passphrase";

    /// The bytes of a PNG header, a chunk, and the trailer, then bytes that
    /// are not UTF-8 and every value of a byte.
    fn binary() -> Vec<u8> {
        let mut bytes = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xAE,
            0x42, 0x60, 0x82, 0xFF, 0xFE, 0xC3, 0x28,
        ];
        bytes.extend(0..=255u8);
        bytes
    }

    fn crypto_error<T: std::fmt::Debug>(result: Result<T, crate::error::Fault>) -> bool {
        Error::from(result.unwrap_err()).is(VaultErrorCode::Crypto)
    }

    #[test]
    fn any_bytes_round_trip_with_a_passphrase_and_with_an_identity() {
        for secret in [test_passphrase(PASS), test_identity()] {
            for plain in [binary(), b"hello\n".to_vec(), Vec::new()] {
                let cipher = seal(&plain, Some(&secret)).unwrap();
                assert!(cipher.starts_with(ARMOR_BEGIN));
                assert!(!cipher.contains(&b'\r'));
                assert!(!cipher.starts_with(&[0xEF, 0xBB, 0xBF]));
                assert_eq!(open(&cipher, Some(&secret)).unwrap(), plain);
            }
        }
    }

    #[test]
    fn the_secret_is_not_in_the_armor() {
        let cipher = seal(b"x", Some(&test_passphrase(PASS))).unwrap();
        assert!(!cipher.windows(PASS.len()).any(|w| w == PASS.as_bytes()));
    }

    #[test]
    fn lf_only_drops_cr_and_keeps_lf() {
        let out = lf_only(
            "-----BEGIN AGE ENCRYPTED FILE-----\r\nabc\r\n-----END AGE ENCRYPTED FILE-----\r\n"
                .to_owned(),
        );
        assert_eq!(
            out,
            b"-----BEGIN AGE ENCRYPTED FILE-----\nabc\n-----END AGE ENCRYPTED FILE-----\n"
        );
    }

    #[test]
    fn crlf_armor_is_crypto() {
        let secret = test_passphrase(PASS);
        let lf = seal(b"hello\n", Some(&secret)).unwrap();
        let crlf = String::from_utf8(lf).unwrap().replace('\n', "\r\n");
        assert!(crypto_error(open(crlf.as_bytes(), Some(&secret))));
    }

    #[test]
    fn a_wrong_secret_is_crypto_and_leaks_nothing() {
        let cipher = seal(b"plain-text-marker", Some(&test_passphrase(PASS))).unwrap();
        let fault = open(&cipher, Some(&test_passphrase("nope"))).unwrap_err();
        assert!(!fault.message().contains(PASS));
        assert!(!fault.message().contains("plain-text-marker"));
        assert!(Error::from(fault).is(VaultErrorCode::Crypto));
        assert!(crypto_error(open(&cipher, Some(&test_identity()))));
    }

    #[test]
    fn a_missing_secret_is_crypto() {
        assert!(crypto_error(seal(b"x", None)));
        let cipher = seal(b"x", Some(&test_passphrase(PASS))).unwrap();
        assert!(crypto_error(open(&cipher, None)));
    }

    #[test]
    fn a_secret_that_is_not_a_key_is_crypto() {
        let secret = super::Encryption {
            secret: Some(super::Secret::Identity("not-a-key".to_owned())),
            ..super::Encryption::default()
        };
        assert!(crypto_error(seal(b"x", Some(&secret))));
    }

    #[test]
    fn bytes_that_are_not_armor_are_crypto() {
        let secret = test_passphrase(PASS);
        assert!(crypto_error(open(b"not armor", Some(&secret))));
        assert!(crypto_error(open(b"aGVsbG8=", Some(&secret))));
        let cipher = seal(b"x", Some(&secret)).unwrap();
        assert!(crypto_error(open(
            &cipher[..cipher.len() / 2],
            Some(&secret)
        )));
    }

    #[test]
    fn an_age_keygen_file_is_accepted() {
        use age::secrecy::ExposeSecret;
        let key = age::x25519::Identity::generate()
            .to_string()
            .expose_secret()
            .to_owned();
        let file = format!("# created: 2026-01-01T00:00:00Z\n# public key: age1test\n{key}\n");
        assert_eq!(parse_age_identity(&file).unwrap(), key);
        let secret = super::Encryption {
            secret: Some(super::Secret::Identity(file)),
            ..super::Encryption::default()
        };
        let cipher = seal(b"file-body", Some(&secret)).unwrap();
        assert_eq!(open(&cipher, Some(&secret)).unwrap(), b"file-body");
    }

    #[test]
    fn a_malformed_identity_file_is_invalid_argument() {
        for text in ["not-a-key\n", ""] {
            let error = parse_age_identity(text).unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument));
        }
        let error = parse_age_identity("AGE-SECRET-KEY-nope\n").unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        assert!(!error.to_string().contains("AGE-SECRET-KEY"));
    }
}
