//! Helpers shared by the CLI test binaries: a scratch directory, the
//! command line run as a child process, the Projector of the command line
//! written again as a library port, and a Vault opened by the library on
//! the same folder.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use buffa_types::google::protobuf::Empty;
use scheda::v1::projector_ref::ProjectorRef;
use scheda::v1::{
    CollectionKey, CollectionSync, ColumnKind, IndexColumn, IndexDoc, IndexSchema, IndexValue,
    OpenOptions, ProjectorDescriptor, ProjectorSync, RawDocument, SchedaSync, VaultOpening,
    VaultSync, index_value,
};
use scheda::{Error, Scheda};
use serde_json::Value;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch directory that is removed when it is dropped.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A new, empty scratch directory.
    pub fn new(name: &str) -> Scratch {
        let base = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = base.join(format!("scheda-cli-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Scratch { root }
    }

    /// The Vault folder of the test: not made until something opens it.
    pub fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    /// A path below the scratch directory, outside the Vault.
    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Writes `bytes` to `rel` below the scratch directory and returns the
    /// path as text.
    pub fn file(&self, rel: &str, bytes: impl AsRef<[u8]>) -> String {
        let path = self.path(rel);
        std::fs::write(&path, bytes).unwrap();
        path.display().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// What a run of the command line left.
pub struct Run {
    /// The exit status.
    pub status: i32,
    /// What it wrote on stdout.
    pub stdout: String,
    /// What it wrote on stderr.
    pub stderr: String,
}

impl Run {
    /// The one JSON line of a successful run.
    pub fn json(&self) -> Value {
        assert_eq!(self.status, 0, "stderr={}", self.stderr);
        assert!(self.stderr.is_empty(), "stderr={}", self.stderr);
        serde_json::from_str(self.stdout.trim())
            .unwrap_or_else(|err| panic!("stdout is not one JSON value ({err}): {}", self.stdout))
    }

    /// The JSON lines of a successful run.
    pub fn lines(&self) -> Vec<Value> {
        assert_eq!(self.status, 0, "stderr={}", self.stderr);
        assert!(self.stderr.is_empty(), "stderr={}", self.stderr);
        self.stdout
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|err| panic!("a line of stdout is not JSON ({err}): {line}"))
            })
            .collect()
    }

    /// The `ErrorReport` of a failed run: one JSON line on stderr and
    /// exit status 1.
    pub fn failure(&self) -> Value {
        assert_eq!(
            self.status, 1,
            "stdout={} stderr={}",
            self.stdout, self.stderr
        );
        assert_eq!(self.stderr.lines().count(), 1, "stderr={}", self.stderr);
        serde_json::from_str(self.stderr.trim())
            .unwrap_or_else(|err| panic!("stderr is not an ErrorReport ({err}): {}", self.stderr))
    }

    /// The `code` string of the `ErrorReport` of a failed run.
    pub fn code(&self) -> String {
        self.failure()["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
}

/// How a run of the command line is set up.
#[derive(Default)]
pub struct Setup<'a> {
    /// Environment variables to set.
    pub env: &'a [(&'a str, &'a str)],
    /// Environment variables to remove.
    pub unset: &'a [&'a str],
    /// What stdin holds; none: stdin is closed at once.
    pub stdin: Option<&'a [u8]>,
}

/// The command line, with the user's and the system's git configuration
/// left out.
pub fn command(args: &[&str]) -> Command {
    let absent = std::env::temp_dir().join("scheda-absent-gitconfig");
    let mut cmd = Command::new(binary());
    cmd.args(args)
        .env("GIT_CONFIG_GLOBAL", &absent)
        .env("GIT_CONFIG_SYSTEM", &absent)
        .env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

fn binary() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_scheda") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_scheda").expect("CARGO_BIN_EXE_scheda"))
}

/// Runs the command line with `args`.
pub fn run(args: &[&str]) -> Run {
    run_with(args, &Setup::default())
}

/// Runs the command line with `args` as `setup` says.
pub fn run_with(args: &[&str], setup: &Setup) -> Run {
    let mut cmd = command(args);
    for name in setup.unset {
        cmd.env_remove(name);
    }
    for (name, value) in setup.env {
        cmd.env(name, value);
    }
    cmd.stdin(if setup.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn scheda");
    if let (Some(bytes), Some(mut stdin)) = (setup.stdin, child.stdin.take()) {
        use std::io::Write;
        let _ = stdin.write_all(bytes);
    }
    let out = child.wait_with_output().expect("wait for scheda");
    Run {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8(out.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(out.stderr).expect("stderr is UTF-8"),
    }
}

/// The global flags every run on the Vault at `vault` carries: its path and
/// an author, and no config of the machine.
pub fn on(vault: &Path) -> Vec<String> {
    [
        "--vault",
        &vault.display().to_string(),
        "--author-name",
        "Test",
        "--author-email",
        "test@example.test",
    ]
    .map(String::from)
    .to_vec()
}

/// Runs the command line on the Vault at `vault`: the flags of [`on`], then
/// `args`.
pub fn run_on(vault: &Path, args: &[&str]) -> Run {
    run_on_with(vault, args, &Setup::default())
}

/// [`run_on`] as `setup` says.
pub fn run_on_with(vault: &Path, args: &[&str], setup: &Setup) -> Run {
    let mut all = on(vault);
    all.extend(args.iter().map(|arg| (*arg).to_owned()));
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    run_with(&refs, setup)
}

/// The projection id of the Projector of the command line.
pub const PROJECTION_ID: &str = "scheda-cli/whole-file@1";

/// The Projector of the command line written again as a port: one text
/// column, `body`, that holds the whole Document. A Vault opened with it
/// shares its Index with the command line.
pub fn whole_file() -> ProjectorRef {
    ProjectorRef::from_sync(WholeFile)
}

struct WholeFile;

impl ProjectorSync for WholeFile {
    fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
        Ok(ProjectorDescriptor::default().with_projection_id(PROJECTION_ID))
    }

    fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
        Ok(IndexSchema {
            columns: vec![
                IndexColumn::default()
                    .with_name("body")
                    .with_kind(ColumnKind::COLUMN_KIND_TEXT)
                    .with_find(true)
                    .with_fts(true)
                    .with_vector(true),
            ],
            ..IndexSchema::default()
        })
    }

    fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
        let mut doc = IndexDoc::default();
        if let Ok(text) = String::from_utf8(document.raw.unwrap_or_default()) {
            doc.values.insert(
                "body".to_owned(),
                IndexValue {
                    value: Some(index_value::Value::Text(text)),
                    ..IndexValue::default()
                },
            );
        }
        Ok(doc)
    }
}

/// The request that opens the Vault at `path` as the command line does
/// when given [`on`]: the same Projector, an author, and the writer `id`.
pub fn opening(path: &Path, writer: &str) -> VaultOpening {
    let options = OpenOptions::default()
        .with_writer(writer)
        .with_index_poll_ms(10)
        .with_write_timeout_ms(60_000);
    let mut request = VaultOpening {
        path: Some(path.display().to_string()),
        options: options.into(),
        ..VaultOpening::default()
    };
    request.projector = whole_file().into();
    request
}

/// A Vault opened by the library on `path`, with the Projector of the
/// command line and no other port.
pub fn library(path: &Path) -> Arc<dyn VaultSync> {
    Scheda
        .open(opening(path, "library"))
        .unwrap()
        .to_sync()
        .unwrap()
}

/// The collection `name` of `vault`.
pub fn collection(vault: &Arc<dyn VaultSync>, name: &str) -> Arc<dyn CollectionSync> {
    vault
        .get_collection(CollectionKey::default().with_name(name))
        .unwrap()
        .to_sync()
        .unwrap()
}

/// The source text of the command line.
pub fn source() -> String {
    [
        include_str!("../../src/args.rs"),
        include_str!("../../src/call.rs"),
        include_str!("../../src/exec.rs"),
        include_str!("../../src/http.rs"),
        include_str!("../../src/main.rs"),
        include_str!("../../src/model2vec.rs"),
        include_str!("../../src/open.rs"),
        include_str!("../../src/projector.rs"),
        include_str!("../../src/stdio.rs"),
    ]
    .join("\n")
}

/// Whether `haystack` names `name` as a whole identifier.
pub fn names(haystack: &str, name: &str) -> bool {
    haystack.match_indices(name).any(|(i, _)| {
        let before = haystack[..i].chars().next_back();
        let after = haystack[i + name.len()..].chars().next();
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        before.is_none_or(|c| !ident(c)) && after.is_none_or(|c| !ident(c))
    })
}
