//! Age v1 ASCII-armor encrypt and decrypt. The only module that names `age`.

#![cfg(feature = "encrypt")]

use std::io::Read;

use age::secrecy::SecretString;
use scheda_generated::api::v1::encryption::Secret;

use crate::error::bail;
use crate::{Encryption, Error};

const ARMOR_BEGIN: &[u8] = b"-----BEGIN AGE ENCRYPTED FILE-----";
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Encrypt `plaintext` to age v1 ASCII armor (PEM, UTF-8, `\n` only, no BOM).
///
/// Passphrase wrapping is age scrypt. Identity wrapping is X25519
/// (`AGE-SECRET-KEY-…`). Missing or unusable secret → `crypto`. The error
/// does not include plaintext, the secret, or key material.
///
/// The `age` crate emits `\r\n` on Windows. CR is dropped here so the
/// bytes on disk are LF-only on every OS.
pub(crate) fn encrypt(plaintext: &[u8], encryption: Option<&Encryption>) -> Result<Vec<u8>, Error> {
    let Some(secret) = encryption.and_then(|enc| enc.secret.as_ref()) else {
        bail!(
            Error::crypto("encrypted collection requires a secret on the handle")
                .context("[crypto::encrypt] encrypt")
        );
    };
    let armored = match secret {
        Secret::Passphrase(passphrase) => {
            let recipient = age::scrypt::Recipient::new(SecretString::from(passphrase.clone()));
            age::encrypt_and_armor(&recipient, plaintext).map_err(|_| {
                Error::crypto("encryption failed").context("[crypto::encrypt] encrypt")
            })?
        }
        Secret::Identity(identity) => {
            let identity = parse_x25519(identity)?;
            let recipient = identity.to_public();
            age::encrypt_and_armor(&recipient, plaintext).map_err(|_| {
                Error::crypto("encryption failed").context("[crypto::encrypt] encrypt")
            })?
        }
    };
    Ok(lf_only(armored))
}

/// Decrypt age v1 ASCII armor to plaintext bytes.
///
/// Bytes that are not armor, a truncated file, a missing secret, or a wrong
/// secret → `crypto`. The error does not include plaintext, the secret, or
/// key material.
pub(crate) fn decrypt(raw: &[u8], encryption: Option<&Encryption>) -> Result<Vec<u8>, Error> {
    require_armor(raw)?;
    let Some(secret) = encryption.and_then(|enc| enc.secret.as_ref()) else {
        bail!(
            Error::crypto("encrypted collection requires a secret on the handle")
                .context("[crypto::decrypt] decrypt")
        );
    };
    let reader = age::armor::ArmoredReader::new(raw);
    let decryptor = match age::Decryptor::new_buffered(reader) {
        Ok(decryptor) => decryptor,
        Err(_) => {
            bail!(Error::crypto("file will not decrypt").context("[crypto::decrypt] decrypt"));
        }
    };
    let plaintext = match secret {
        Secret::Passphrase(passphrase) => {
            let identity = age::scrypt::Identity::new(SecretString::from(passphrase.clone()));
            finish_decrypt(decryptor, &identity)?
        }
        Secret::Identity(identity) => {
            let identity = parse_x25519(identity)?;
            finish_decrypt(decryptor, &identity)?
        }
    };
    Ok(plaintext)
}

#[cfg(test)]
pub(crate) fn test_identity_secret() -> Encryption {
    use age::secrecy::ExposeSecret;
    Encryption::new().identity(
        age::x25519::Identity::generate()
            .to_string()
            .expose_secret()
            .to_owned(),
    )
}

fn require_armor(raw: &[u8]) -> Result<(), Error> {
    if raw.starts_with(UTF8_BOM) || raw.contains(&b'\r') || !raw.starts_with(ARMOR_BEGIN) {
        bail!(Error::crypto("file is not age armor").context("[crypto::decrypt] armor"));
    }
    Ok(())
}

/// Drop CR so armor is LF-only. The `age` crate uses `\r\n` on Windows.
fn lf_only(armor: String) -> Vec<u8> {
    let mut bytes = armor.into_bytes();
    bytes.retain(|&b| b != b'\r');
    bytes
}

fn parse_x25519(identity: &str) -> Result<age::x25519::Identity, Error> {
    match identity.trim().parse::<age::x25519::Identity>() {
        Ok(identity) => Ok(identity),
        Err(_) => {
            bail!(Error::crypto("secret is missing or wrong").context("[crypto] parse identity"));
        }
    }
}

fn finish_decrypt<R: Read>(
    decryptor: age::Decryptor<R>,
    identity: &dyn age::Identity,
) -> Result<Vec<u8>, Error> {
    let mut reader = match decryptor.decrypt(std::iter::once(identity)) {
        Ok(reader) => reader,
        Err(_) => {
            bail!(Error::crypto("file will not decrypt").context("[crypto::decrypt] decrypt"));
        }
    };
    let mut plaintext = Vec::new();
    match reader.read_to_end(&mut plaintext) {
        Ok(_) => Ok(plaintext),
        Err(_) => {
            bail!(Error::crypto("file will not decrypt").context("[crypto::decrypt] decrypt"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ARMOR_BEGIN, decrypt, encrypt, lf_only};
    use crate::error::Code;
    use crate::{Encryption, Error};

    const PASS: &str = "p14-unit-passphrase";

    fn passphrase() -> Encryption {
        Encryption::new().passphrase(PASS)
    }

    #[test]
    fn passphrase_round_trip() {
        let secret = passphrase();
        let cipher = encrypt(b"hello\n", Some(&secret)).unwrap();
        assert!(cipher.starts_with(ARMOR_BEGIN));
        assert!(!cipher.contains(&b'\r'));
        assert!(!cipher.starts_with(&[0xEF, 0xBB, 0xBF]));
        assert!(!cipher.windows(PASS.len()).any(|w| w == PASS.as_bytes()));
        let plain = decrypt(&cipher, Some(&secret)).unwrap();
        assert_eq!(plain, b"hello\n");
    }

    #[test]
    fn identity_round_trip() {
        let secret = super::test_identity_secret();
        let cipher = encrypt(b"id-body", Some(&secret)).unwrap();
        assert!(cipher.starts_with(ARMOR_BEGIN));
        assert!(!cipher.contains(&b'\r'));
        let plain = decrypt(&cipher, Some(&secret)).unwrap();
        assert_eq!(plain, b"id-body");
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
        let secret = passphrase();
        let lf = encrypt(b"hello\n", Some(&secret)).unwrap();
        let crlf = String::from_utf8(lf).unwrap().replace('\n', "\r\n");
        let err = decrypt(crlf.as_bytes(), Some(&secret)).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Crypto));
    }

    #[test]
    fn wrong_passphrase_is_crypto() {
        let cipher = encrypt(b"x", Some(&passphrase())).unwrap();
        let err = decrypt(&cipher, Some(&Encryption::new().passphrase("nope"))).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Crypto));
        let msg = err.message.as_deref().unwrap_or("");
        assert!(!msg.contains(PASS));
        assert!(!msg.contains("x"));
    }

    #[test]
    fn missing_secret_is_crypto() {
        let err = encrypt(b"x", None).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Crypto));
        let cipher = encrypt(b"x", Some(&passphrase())).unwrap();
        let err = decrypt(&cipher, None).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Crypto));
    }

    #[test]
    fn non_armor_is_crypto() {
        let err = decrypt(b"not armor", Some(&passphrase())).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Crypto));
        let err = decrypt(b"aGVsbG8=", Some(&passphrase())).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Crypto));
    }

    #[test]
    fn crypto_errors_are_forbidden_to_retry() {
        let err = Error::crypto("file will not decrypt");
        assert_eq!(err.known_code(), Some(Code::Crypto));
        assert_eq!(err.known_repeat(), Some(crate::error::Repeat::Forbidden));
    }
}
