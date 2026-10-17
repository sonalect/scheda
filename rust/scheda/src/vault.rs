//! The `Vault` handle: opening a Vault that exists, and closing it.

// The layers that read and write through the handle use its fields.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the layers that read and write through the handle are not built yet"
    )
)]

use std::fs;
use std::path::{Path, PathBuf};

use scheda_api::v1::{OpenMode, OpenOptions, VaultErrorCode};

use crate::collection::{self, Declarations};
use crate::error::{Context, Error, Fault, io, redact_remote_text};
use crate::fs::restrict_dir;
use crate::{git, marker, stored, writer};

/// An open Vault: a git work tree that holds the marker `.scheda.yaml`, with
/// the collections its root directories declare.
///
/// [`Vault::open`] opens one that exists, and [`Vault::close`] ends the
/// handle.
#[derive(Debug)]
pub struct Vault {
    path: PathBuf,
    options: OpenOptions,
    declarations: Declarations,
    writer: String,
}

/// What `open` found at the path.
enum Spot {
    /// Nothing is there.
    Missing,
    /// An empty folder.
    Empty,
    /// A git work tree that holds a marker of the generation this build
    /// reads.
    Vault,
    /// A git work tree without the marker.
    WorkTree,
    /// A file, or a folder that is neither empty nor a git work tree.
    Other,
}

impl Vault {
    /// Opens the Vault at `path`, or, as `options.mode` allows, makes one.
    ///
    /// `options` is checked first (`INVALID_ARGUMENT`); `options.encryption`
    /// in a build without the `encrypt` feature is `UNIMPLEMENTED`. Then
    /// `options.mode` decides what the path may be (unset is
    /// `OPEN_MODE_OPEN_OR_CREATE`):
    ///
    /// - A Vault, a git work tree with the marker `.scheda.yaml`: the marker
    ///   is read before any other file, and a marker of another generation
    ///   is `GENERATION` in every mode. `OPEN_MODE_CREATE_NEW` is
    ///   `ALREADY_EXISTS`; the other modes open it.
    /// - A git work tree without the marker: `OPEN_MODE_OPEN_EXISTING` is
    ///   `NOT_A_VAULT`, `OPEN_MODE_CREATE_NEW` is `ALREADY_EXISTS`.
    /// - A folder that is neither empty nor a git work tree:
    ///   `OPEN_MODE_OPEN_EXISTING` is `NOT_A_VAULT`,
    ///   `OPEN_MODE_CREATE_NEW` is `ALREADY_EXISTS`, and
    ///   `OPEN_MODE_OPEN_OR_CREATE` is `INVALID_ARGUMENT`.
    /// - An empty folder: `OPEN_MODE_OPEN_EXISTING` is `NOT_A_VAULT`.
    /// - A missing path: `OPEN_MODE_OPEN_EXISTING` is `NOT_FOUND`.
    ///
    /// A refusal writes nothing. When it opens a Vault, `options.mirror`, if
    /// set, must be the fetch URL of the remote `origin`
    /// (`INVALID_ARGUMENT` otherwise, without contacting the network). The
    /// collection files are read (an invalid one fails `open`), `.scheda/`
    /// is created owner-only, and the writer identity is read or made. `open`
    /// takes no write lock, pulls nothing, and commits nothing.
    ///
    /// # Errors
    ///
    /// The codes above, and `STORAGE` or `REPLICATION` when the path or git
    /// cannot be read.
    pub fn open(path: impl AsRef<Path>, options: OpenOptions) -> Result<Vault, Error> {
        open(path.as_ref(), options)
    }

    /// Ends the handle. The handle holds no lock and no background work yet,
    /// so nothing can fail; a later version of the handle may flush before
    /// it returns.
    ///
    /// # Errors
    ///
    /// None at present.
    pub fn close(self) -> Result<(), Error> {
        Ok(())
    }
}

fn open(path: &Path, options: OpenOptions) -> Result<Vault, Error> {
    stored::validate(&options)
        .map_err(|fault| {
            Fault::new(
                VaultErrorCode::InvalidArgument,
                format!("the open options are not valid: {}", fault.message()),
            )
            .with_source(fault)
        })
        .context("Vault::open: check the options")?;
    #[cfg(not(feature = "encrypt"))]
    if options.encryption.is_set() {
        return Err(Fault::new(
            VaultErrorCode::Unimplemented,
            "encryption needs a build with the `encrypt` feature",
        )
        .into());
    }
    let mode = options
        .mode
        .and_then(|mode| mode.as_known())
        .unwrap_or(OpenMode::OPEN_MODE_UNSPECIFIED);
    match classify(path).context("Vault::open: look at the path")? {
        Spot::Vault => {
            if mode == OpenMode::OPEN_MODE_CREATE_NEW {
                return Err(refusal(
                    VaultErrorCode::AlreadyExists,
                    "the path holds a Vault",
                ));
            }
            open_existing(path, options).context("Vault::open: open the Vault")
        }
        Spot::WorkTree => match mode {
            OpenMode::OPEN_MODE_OPEN_EXISTING => Err(refusal(
                VaultErrorCode::NotAVault,
                "the path is a git work tree without the Vault marker",
            )),
            OpenMode::OPEN_MODE_CREATE_NEW => Err(refusal(
                VaultErrorCode::AlreadyExists,
                "the path is a git work tree",
            )),
            _ => todo!("P4 gives a git work tree without the marker its marker in one write"),
        },
        Spot::Other => match mode {
            OpenMode::OPEN_MODE_OPEN_EXISTING => Err(refusal(
                VaultErrorCode::NotAVault,
                "the path is not a Vault",
            )),
            OpenMode::OPEN_MODE_CREATE_NEW => Err(refusal(
                VaultErrorCode::AlreadyExists,
                "the path is not empty",
            )),
            _ => Err(refusal(
                VaultErrorCode::InvalidArgument,
                "the path is neither an empty folder, a git work tree, nor a Vault",
            )),
        },
        Spot::Empty => match mode {
            OpenMode::OPEN_MODE_OPEN_EXISTING => Err(refusal(
                VaultErrorCode::NotAVault,
                "the folder is empty and holds no Vault",
            )),
            _ => todo!("P4 makes a Vault in an empty folder, or clones the mirror into it"),
        },
        Spot::Missing => match mode {
            OpenMode::OPEN_MODE_OPEN_EXISTING => {
                Err(refusal(VaultErrorCode::NotFound, "the path does not exist"))
            }
            _ => todo!("P4 makes a Vault at a missing path, or clones the mirror there"),
        },
    }
}

fn refusal(code: VaultErrorCode, message: &str) -> Error {
    Fault::new(code, message).into()
}

/// What is at `path`. A git work tree is asked for its marker before
/// anything else of it is read: a marker of another generation fails here.
fn classify(path: &Path) -> Result<Spot, Error> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Spot::Missing),
        Err(err) => return Err(io(err)).context("stat the path"),
    };
    if !meta.is_dir() {
        return Ok(Spot::Other);
    }
    let mut entries = fs::read_dir(path).map_err(io).context("read the folder")?;
    if entries.next().is_none() {
        return Ok(Spot::Empty);
    }
    if !git::is_work_tree(path) {
        return Ok(Spot::Other);
    }
    match marker::read(path)? {
        Some(_) => Ok(Spot::Vault),
        None => Ok(Spot::WorkTree),
    }
}

/// Opens the Vault at `path`, whose marker `classify` has accepted.
fn open_existing(path: &Path, options: OpenOptions) -> Result<Vault, Error> {
    let canonical = path
        .canonicalize()
        .map_err(io)
        .context("resolve the path")?;
    if let Some(mirror) = options.mirror.as_deref() {
        check_mirror(&canonical, mirror)?;
    }
    let declarations = collection::load(&canonical)?;
    let scheda = canonical.join(".scheda");
    fs::create_dir_all(&scheda)
        .map_err(io)
        .context("create .scheda")?;
    restrict_dir(&scheda)?;
    let writer = writer::load_or_create(&canonical, options.writer.as_deref())?;
    Ok(Vault {
        path: canonical,
        options,
        declarations,
        writer,
    })
}

/// The mirror a caller names must be the fetch URL of `origin`. Reads the
/// local configuration only.
fn check_mirror(vault: &Path, mirror: &str) -> Result<(), Error> {
    let origin = git::origin_url(vault).context("read the mirror of the Vault")?;
    match origin {
        Some(url) if url == mirror => Ok(()),
        Some(url) => Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            format!(
                "the mirror {} is not the mirror of this Vault, {}",
                redact_remote_text(mirror),
                redact_remote_text(&url)
            ),
        )
        .into()),
        None => Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            format!(
                "the mirror {} is named, but this Vault has no mirror",
                redact_remote_text(mirror)
            ),
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use scheda_api::v1::{OpenOptions, VaultErrorCode};

    use super::Vault;
    use crate::git::{git, isolate_test_git_env};

    /// A Vault on disk, made with plain git, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn vault(name: &str) -> Scratch {
            isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-vault-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("notes")).unwrap();
            git().arg("init").arg("--").arg(&dir).run().unwrap();
            std::fs::write(dir.join(".scheda.yaml"), "version: 2\nfuture: 1\n").unwrap();
            std::fs::write(
                dir.join("notes/.collection.yaml"),
                "name: notes\nschema:\n  standard: {}\n",
            )
            .unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_existing_vault_opens_with_its_collections_and_a_writer_and_closes() {
        let scratch = Scratch::vault("open");
        let vault = Vault::open(&scratch.0, OpenOptions::default()).unwrap();
        assert_eq!(vault.path, scratch.0.canonicalize().unwrap());
        assert_eq!(vault.declarations.len(), 1);
        assert!(vault.declarations.get("notes").is_some());
        assert_eq!(vault.writer.len(), 16);
        assert_eq!(vault.options.mirror, None);
        vault.close().unwrap();
        assert!(scratch.0.join(".scheda/writer").is_file());
        let again = Vault::open(&scratch.0, OpenOptions::default().with_writer("alice")).unwrap();
        assert_eq!(again.writer, "alice");
    }

    #[test]
    fn a_mirror_must_be_the_origin_of_the_vault() {
        let scratch = Scratch::vault("mirror");
        let url = "https://example.test/org/repo.git";
        let options = || OpenOptions::default().with_mirror(url);
        let error = Vault::open(&scratch.0, options()).unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument), "no origin yet");
        git()
            .current_dir(Path::new(&scratch.0))
            .arg("remote")
            .arg("add")
            .arg("origin")
            .arg(url)
            .run()
            .unwrap();
        Vault::open(&scratch.0, options()).unwrap();
        let other = OpenOptions::default().with_mirror("https://example.test/org/other.git");
        let error = Vault::open(&scratch.0, other).unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[test]
    fn options_that_break_a_rule_are_invalid_argument() {
        let scratch = Scratch::vault("options");
        for options in [
            OpenOptions::default().with_writer("Not Valid"),
            OpenOptions::default().with_mirror("not a url"),
            OpenOptions::default().with_index_poll_ms(1),
        ] {
            let error = Vault::open(&scratch.0, options).unwrap_err();
            assert!(error.is(VaultErrorCode::InvalidArgument));
        }
        assert!(
            !scratch.0.join(".scheda").exists(),
            "a refusal writes nothing"
        );
    }
}
