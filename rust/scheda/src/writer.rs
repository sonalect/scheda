//! The machine-local writer identity of one Vault clone: the token in
//! `{vault}/.scheda/writer`, gitignored with the rest of `.scheda/`.
//!
//! Journal segments are named with it, so two clones of one Vault never
//! write the same segment file.

use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use protovalidate_buffa::Validate;
use scheda_api::v1::{OpenOptions, VaultErrorCode};

use crate::error::{Context, Error, Fault, io};
use crate::fs::{atomic_write_private, create_private, restrict_dir, restrict_file};

/// Vault-relative name of the token file.
const FILE: &str = ".scheda/writer";

/// `{vault}/.scheda/writer`.
pub(crate) fn path(vault: &Path) -> PathBuf {
    vault.join(".scheda").join("writer")
}

/// The one rule of the token that the contract cannot express: its first
/// part must not be eight hexadecimal digits, which is how a journal
/// segment sequence looks in a file name. The form of the token (length,
/// characters) is the contract's rule on `OpenOptions.writer`.
pub(crate) fn check_not_a_sequence(token: &str) -> Result<(), Fault> {
    if token.split('-').next().is_some_and(is_hex_seq) {
        return Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            "writer identity must not start with eight hexadecimal digits",
        ));
    }
    Ok(())
}

/// A token read from the file: the contract's rule on the form, then
/// [`check_not_a_sequence`]. The file is not an `OpenOptions`, so its token
/// is checked here by the same rule.
fn check_file_token(token: &str) -> Result<(), Fault> {
    OpenOptions::default()
        .with_writer(token)
        .validate()
        .map_err(|err| {
            Fault::new(
                VaultErrorCode::InvalidArgument,
                "writer identity must be 1 to 32 lowercase letters, digits, or hyphens, \
                 starting and ending with a letter or digit",
            )
            .with_source(err)
        })?;
    check_not_a_sequence(token)
}

/// Identity of the process that writes this Vault clone.
///
/// Unique across independent clones of one Vault, so journal segments named
/// with this token cannot collide. Sixteen lowercase hex digits. Generated
/// when `{vault}/.scheda/writer` is absent.
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

/// Loads `{vault}/.scheda/writer`, or generates and persists it.
///
/// A set `requested` token (`OpenOptions.writer`, already validated against
/// the contract) wins, is checked by [`check_not_a_sequence`], and is
/// written, so a later `open` without the option keeps the same identity. A
/// token in the file that breaks the rules is `INVALID_ARGUMENT`.
///
/// A generated token is written only where no file is: of two first opens
/// of one clone at once, one makes the file and the other takes its token,
/// so every handle of a clone has one identity.
pub(crate) fn load_or_create(vault: &Path, requested: Option<&str>) -> Result<String, Error> {
    if let Some(token) = requested {
        check_not_a_sequence(token)?;
        // Written when the file holds anything else: two opens that ask for
        // one identity at once do not replace each other's file.
        let line = format!("{token}\n");
        if fs::read(path(vault)).ok().as_deref() != Some(line.as_bytes()) {
            persist(vault, token)?;
        }
        return Ok(token.to_owned());
    }
    if let Some(token) = read(vault)? {
        return Ok(token);
    }
    let token = generate();
    if create(vault, &token)? {
        return Ok(token);
    }
    read(vault)?.ok_or_else(|| {
        Error::from(
            Fault::new(
                VaultErrorCode::Storage,
                "the writer identity file went while another open made it",
            )
            .with_path(FILE),
        )
    })
}

/// The token in `{vault}/.scheda/writer`, `None` when there is no file.
fn read(vault: &Path) -> Result<Option<String>, Error> {
    match fs::read(path(vault)) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|err| {
                    Fault::new(
                        VaultErrorCode::InvalidArgument,
                        "the writer identity file is not UTF-8",
                    )
                    .with_source(err)
                    .with_path(FILE)
                })
                .context("read the writer identity")?;
            let token = text.trim();
            check_file_token(token)
                .map_err(|fault| fault.with_path(FILE))
                .context("read the writer identity")?;
            Ok(Some(token.to_owned()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io(err))
            .map_err(|fault| fault.with_path(FILE))
            .context("read the writer identity"),
    }
}

/// Writes `token`, replacing what is there.
fn persist(vault: &Path, token: &str) -> Result<(), Error> {
    let file = prepare(vault)?;
    atomic_write_private(&file, format!("{token}\n").as_bytes())
        .context("write the writer identity")?;
    restrict_file(&file)
}

/// Writes `token` when no file is there; whether it did.
fn create(vault: &Path, token: &str) -> Result<bool, Error> {
    let file = prepare(vault)?;
    let made = create_private(&file, format!("{token}\n").as_bytes())
        .context("write the writer identity")?;
    if made {
        restrict_file(&file)?;
    }
    Ok(made)
}

/// Makes `{vault}/.scheda/` owner-only; the path of the token file.
fn prepare(vault: &Path) -> Result<PathBuf, Error> {
    let scheda = vault.join(".scheda");
    fs::create_dir_all(&scheda)
        .map_err(io)
        .context("create .scheda for the writer identity")?;
    restrict_dir(&scheda)?;
    Ok(path(vault))
}

/// True when `part` is exactly eight hex digits (a journal sequence token).
fn is_hex_seq(part: &str) -> bool {
    part.len() == 8 && part.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use scheda_api::v1::VaultErrorCode;

    use super::{check_file_token, check_not_a_sequence, generate, load_or_create, path};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-writer-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    #[test]
    fn a_generated_token_is_valid() {
        let token = generate();
        assert_eq!(token.len(), 16);
        check_file_token(&token).unwrap();
    }

    #[test]
    fn the_form_comes_from_the_contract_and_the_sequence_rule_from_here() {
        for bad in [
            "",
            "Alice",
            "a_b",
            "-abc",
            "abc-",
            "00000001",
            "abcdef01-node",
        ] {
            assert!(check_file_token(bad).is_err(), "{bad:?}");
        }
        for good in ["alice", "a", "host-1", "node-abcdef01", "0000000a1"] {
            check_file_token(good).unwrap();
        }
        let fault = check_not_a_sequence("abcdef01-node").unwrap_err();
        assert!(crate::error::Error::from(fault).is(VaultErrorCode::InvalidArgument));
        check_not_a_sequence("node-abcdef01").unwrap();
    }

    #[test]
    fn the_token_is_generated_once_and_kept() {
        let vault = Scratch::new("keep");
        let first = load_or_create(&vault.0, None).unwrap();
        assert_eq!(load_or_create(&vault.0, None).unwrap(), first);
        assert_eq!(
            std::fs::read_to_string(path(&vault.0)).unwrap(),
            format!("{first}\n")
        );
    }

    /// Loads of one clone at once that find no identity each end with the
    /// same token: the first to make the file, the others from it.
    #[test]
    fn first_loads_at_once_agree_on_one_token() {
        let vault = Scratch::new("race");
        let start = std::sync::Barrier::new(16);
        let tokens: Vec<String> = std::thread::scope(|scope| {
            let loads: Vec<_> = (0..16)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        load_or_create(&vault.0, None).unwrap()
                    })
                })
                .collect();
            loads.into_iter().map(|load| load.join().unwrap()).collect()
        });
        assert!(tokens.iter().all(|token| *token == tokens[0]), "{tokens:?}");
        assert_eq!(
            std::fs::read_to_string(path(&vault.0)).unwrap(),
            format!("{}\n", tokens[0])
        );
    }

    #[test]
    fn a_requested_token_wins_and_is_written() {
        let vault = Scratch::new("request");
        assert_eq!(load_or_create(&vault.0, Some("alice")).unwrap(), "alice");
        assert_eq!(load_or_create(&vault.0, None).unwrap(), "alice");
        let error = load_or_create(&vault.0, Some("00000001")).unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        assert_eq!(load_or_create(&vault.0, None).unwrap(), "alice");
    }

    #[test]
    fn a_broken_token_file_is_invalid_argument_with_its_path() {
        let vault = Scratch::new("broken");
        std::fs::create_dir_all(vault.0.join(".scheda")).unwrap();
        for content in [&b"Bad Token\n"[..], &b"\xff\xfe"[..], &b""[..]] {
            std::fs::write(path(&vault.0), content).unwrap();
            let error = load_or_create(&vault.0, None).unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument));
            let fault = error.downcast_ref::<crate::error::Fault>().unwrap();
            assert_eq!(fault.path(), Some(".scheda/writer"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_scheda_directory_and_the_token_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let vault = Scratch::new("modes");
        load_or_create(&vault.0, None).unwrap();
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&vault.0.join(".scheda")), 0o700);
        assert_eq!(mode(&path(&vault.0)), 0o600);
    }
}
