//! Failures of Scheda: one [`Fault`] that carries a [`VaultErrorCode`], and
//! the helpers that turn foreign errors into it.
//!
//! The error a caller receives is [`Error`] (`protocontract::Error`). It
//! holds a [`Fault`] and the stack of operations the failure passed on its
//! way out. Callers branch on the code (`Error::is`, `Error::code_as`),
//! never on the message.

use std::backtrace::Backtrace;
use std::fmt::{self, Write as _};
use std::sync::{Mutex, MutexGuard, PoisonError};

use protocontract::{Code, RuntimeCode};
pub use protocontract::{Context, Error};
use scheda_api::v1::{ErrorReport, VaultErrorCode};

/// A failure of Scheda: a code, a message, the file it is about, whether
/// the same call may succeed later, and the cause.
///
/// It is the one type that implements [`protocontract::Fault`] for Scheda.
/// It converts into [`Error`] with `?`. A caller branches on
/// [`Error::code`], not on the fields of a `Fault`; the fields are for
/// diagnostics, reached with `error.downcast_ref::<Fault>()`.
#[derive(Debug)]
pub struct Fault {
    code: VaultErrorCode,
    message: String,
    path: Option<String>,
    retryable: bool,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    backtrace: Backtrace,
}

impl Fault {
    /// A failure with `code` and `message`, without a path or a source.
    /// It is retryable when the code is one a caller may try again
    /// (`ABORTED`, `UNAVAILABLE`, `INDEX_REBUILDING`).
    /// It captures a backtrace when `RUST_BACKTRACE` or
    /// `RUST_LIB_BACKTRACE` is set.
    #[must_use]
    pub fn new(code: VaultErrorCode, message: impl Into<String>) -> Fault {
        Fault {
            code,
            message: message.into(),
            path: None,
            retryable: default_retryable(code),
            source: None,
            backtrace: Backtrace::capture(),
        }
    }

    /// This failure about the file at `path`, relative to the Vault root
    /// and in POSIX form.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Fault {
        self.path = Some(path.into());
        self
    }

    /// This failure with `source` as its cause, replacing any earlier one.
    #[must_use]
    pub fn with_source(
        mut self,
        source: impl Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    ) -> Fault {
        self.source = Some(source.into());
        self
    }

    /// This failure, retryable or not: the override for a method that
    /// knows better than the default of the code.
    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Fault {
        self.retryable = retryable;
        self
    }

    /// The code of the failure, for the layers that report it as data (a
    /// `Check` problem) instead of raising it.
    #[must_use]
    pub(crate) fn vault_code(&self) -> VaultErrorCode {
        self.code
    }

    /// What happened, for a person. Never replaces the code.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The file the failure is about, relative to the Vault root (POSIX),
    /// or `None` when it is about no one file.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
}

impl fmt::Display for Fault {
    /// `path: message` with a path, `message` without.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            Some(path) => write!(f, "{path}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Fault {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

impl protocontract::Fault for Fault {
    fn code(&self) -> Code {
        Code::of(self.code)
    }

    fn retryable(&self) -> bool {
        self.retryable
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        Some(&self.backtrace)
    }
}

/// Whether a failure with `code` may succeed when the same call is made
/// again, unless the method that makes it knows better.
#[must_use]
pub(crate) fn default_retryable(code: VaultErrorCode) -> bool {
    matches!(
        code,
        VaultErrorCode::Aborted
            | VaultErrorCode::Unavailable
            | VaultErrorCode::IndexRebuilding
            | VaultErrorCode::DeadlineExceeded
    )
}

/// A failure of the storage below the Vault, or of the network below the
/// mirror, from its `std::io::Error`. The error stays the source, so its
/// `ErrorKind` is still reachable.
///
/// Network kinds and timeouts are `UNAVAILABLE` (retryable). Every other
/// kind is `STORAGE`, retryable only for the passing kinds `Interrupted`,
/// `WouldBlock`, and `ResourceBusy`.
#[must_use]
pub(crate) fn io(err: std::io::Error) -> Fault {
    use std::io::ErrorKind;
    let (code, retryable) = match err.kind() {
        ErrorKind::NetworkDown
        | ErrorKind::NetworkUnreachable
        | ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::HostUnreachable
        | ErrorKind::TimedOut => (VaultErrorCode::Unavailable, true),
        ErrorKind::Interrupted | ErrorKind::WouldBlock | ErrorKind::ResourceBusy => {
            (VaultErrorCode::Storage, true)
        }
        _ => (VaultErrorCode::Storage, false),
    };
    Fault::new(code, err.to_string())
        .with_retryable(retryable)
        .with_source(err)
}

/// Fail closed if a previous holder panicked with this mutex.
pub(crate) fn mutex<'a, T>(
    mutex: &'a Mutex<T>,
    frame: &'static str,
) -> Result<MutexGuard<'a, T>, Error> {
    mutex.lock().map_err(|_: PoisonError<MutexGuard<'a, T>>| {
        Error::new(RuntimeCode::Panicked, "mutex poisoned").context(frame)
    })
}

/// The failure `error` written out as an [`ErrorReport`], the form a
/// transport carries: what the command line prints on stderr, and what
/// `IndexState.last_error` and `MirrorInfo.last_auto_sync_error` hold.
///
/// The code is the name of the error's code (`VAULT_ERROR_CODE_NOT_FOUND`,
/// or the code of a port or of the runtime); the retry flag and the stack
/// are the error's; the path is the one a [`Fault`] carries. The message is
/// that of a [`Fault`] or of protocontract's `Failure`, and for a fault of
/// any other type (a port's own) its `Display`, without the stack and the
/// code that the `Display` of the error puts before it.
#[must_use]
pub fn error_report(error: &Error) -> ErrorReport {
    let fault = error.downcast_ref::<Fault>();
    let message = match fault {
        Some(fault) => fault.message().to_owned(),
        None => match error.downcast_ref::<protocontract::Failure>() {
            Some(failure) => failure.message().to_owned(),
            None => foreign_message(error),
        },
    };
    let mut report = ErrorReport::default()
        .with_code(error.code().name())
        .with_message(message)
        .with_retryable(error.retryable());
    report.stack = error.stack().to_vec();
    match fault.and_then(Fault::path) {
        Some(path) => report.with_path(path),
        None => report,
    }
}

/// The message of a fault of a type Scheda does not know: the `Display` of
/// the error without what protocontract writes before the fault's own
/// (`frame: frame: CODE: message`, outermost frame first).
fn foreign_message(error: &Error) -> String {
    let text = error.to_string();
    let mut prefix = String::new();
    for frame in error.stack() {
        let _ = write!(prefix, "{frame}: ");
    }
    let _ = write!(prefix, "{}: ", error.code());
    match text.strip_prefix(&prefix) {
        Some(message) => message.to_owned(),
        None => text,
    }
}

/// The class of a failed git command, read from its `stderr`. Only a
/// refusal of the credentials and a failure of the network are read from
/// there: the git command line has no other signal for them. Everything
/// else git reports has a signal of its own (the status of a reference in
/// `git push --porcelain`, the references a fetch wrote, a repository's
/// configuration) and is classified by it where the command is run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitFailure {
    /// The mirror refused the credentials.
    Auth,
    /// The mirror's TLS certificate was not accepted.
    Certificate,
    /// The mirror did not answer in time.
    Timeout,
    /// The mirror's host, or the network to it, cannot be reached.
    HostUnreachable,
    /// Any other failure.
    Other,
}

impl GitFailure {
    /// Classifies the `stderr` of a git command. Patterns are matched on
    /// lower-cased text (Scheda runs git with `LC_ALL=C`); a text no
    /// pattern knows is [`GitFailure::Other`].
    #[must_use]
    pub(crate) fn classify(stderr: &str) -> GitFailure {
        let text = stderr.to_ascii_lowercase();
        let hay = text.as_str();
        if matches_auth(hay) {
            return GitFailure::Auth;
        }
        if matches_cert(hay) {
            return GitFailure::Certificate;
        }
        if matches_timeout(hay) {
            return GitFailure::Timeout;
        }
        if matches_host_unreachable(hay) {
            return GitFailure::HostUnreachable;
        }
        GitFailure::Other
    }

    /// The code a failure of this class has.
    #[must_use]
    pub(crate) fn code(self) -> VaultErrorCode {
        match self {
            GitFailure::Auth | GitFailure::Certificate => VaultErrorCode::Unauthenticated,
            GitFailure::Timeout | GitFailure::HostUnreachable => VaultErrorCode::Unavailable,
            GitFailure::Other => VaultErrorCode::Replication,
        }
    }
}

/// The cause of a [`Fault`] made by [`git_fault`]: the redacted `stderr`
/// of the git command.
#[derive(Debug)]
pub(crate) struct GitError {
    stderr: String,
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.stderr)
    }
}

impl std::error::Error for GitError {}

/// A [`Fault`] for a git command that failed with `stderr`. The code comes
/// from [`GitFailure::classify`]; the message and the source carry the
/// `stderr` with remote URLs redacted, so no credential reaches a caller.
#[must_use]
pub(crate) fn git_fault(stderr: &str) -> Fault {
    let failure = GitFailure::classify(stderr);
    let redacted = redact_remote_text(stderr.trim());
    Fault::new(failure.code(), redacted.clone()).with_source(GitError { stderr: redacted })
}

/// A [`Fault`] for a failure of the git library. The code comes from what
/// the library says went wrong, its code and class, not from its message:
/// a lock file held by another process is `ABORTED`; an operating-system
/// or file-system failure is `STORAGE`; a missing or damaged object, tree,
/// reference, configuration, index, or repository of the Vault's own data
/// is `DATA_LOSS`; anything else is `REPLICATION`. The library's error
/// stays the source; the message has remote URLs redacted.
#[must_use]
pub(crate) fn lib_fault(err: git2::Error) -> Fault {
    use git2::{ErrorClass, ErrorCode};
    let code = if err.code() == ErrorCode::Locked {
        VaultErrorCode::Aborted
    } else if matches!(
        err.class(),
        ErrorClass::Os | ErrorClass::Filesystem | ErrorClass::NoMemory
    ) {
        VaultErrorCode::Storage
    } else if err.code() == ErrorCode::HashsumMismatch
        || matches!(
            err.class(),
            ErrorClass::Odb
                | ErrorClass::Object
                | ErrorClass::Zlib
                | ErrorClass::Sha1
                | ErrorClass::Tree
                | ErrorClass::Tag
                | ErrorClass::Index
                | ErrorClass::Indexer
                | ErrorClass::Reference
                | ErrorClass::Repository
                | ErrorClass::Config
                | ErrorClass::Worktree
        )
    {
        VaultErrorCode::DataLoss
    } else {
        VaultErrorCode::Replication
    };
    Fault::new(code, redact_remote_text(err.message())).with_source(err)
}

fn matches_auth(hay: &str) -> bool {
    hay.contains("authentication failed")
        || hay.contains("permission denied (publickey)")
        || hay.contains("permission denied (keyboard-interactive)")
        || hay.contains("could not read username")
        || hay.contains("invalid username or password")
        || hay.contains("terminal prompts disabled")
        || hay.contains("support for password authentication was removed")
        || hay.contains("no git credentials available")
}

fn matches_cert(hay: &str) -> bool {
    hay.contains("ssl certificate")
        || hay.contains("certificate verify failed")
        || hay.contains("server certificate verification failed")
        || hay.contains("certificate problem")
        || hay.contains("unknown ssl protocol")
}

fn matches_timeout(hay: &str) -> bool {
    hay.contains("timed out")
        || hay.contains("timeout")
        || hay.contains("couldn't connect to server")
}

fn matches_host_unreachable(hay: &str) -> bool {
    hay.contains("network is unreachable")
        || hay.contains("network unreachable")
        || hay.contains("connection refused")
        || hay.contains("connection reset")
        || hay.contains("could not resolve hostname")
        || hay.contains("could not resolve host")
        || hay.contains("name or service not known")
        || hay.contains("temporary failure in name resolution")
        || hay.contains("nodename nor servname")
        || hay.contains("no route to host")
        || hay.contains("host is unreachable")
}

/// Drop userinfo, path, and query from remote URLs in a foreign error string.
///
/// Git and HTTP libraries put the URL they tried into `Display`. Callers
/// must not echo credentials or a full remote path in an error message.
#[must_use]
pub fn redact_remote_text(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    loop {
        let uri = find_scheme_url(rest);
        let git_at = rest.find("git@");
        let (start, end, scp) = match (uri, git_at) {
            (None, None) => {
                out.push_str(rest);
                break;
            }
            (Some((s, _)), Some(g)) if g < s => (g, scp_git_end(rest, g), true),
            (Some((s, e)), _) => (s, e, false),
            (None, Some(g)) => (g, scp_git_end(rest, g), true),
        };
        out.push_str(&rest[..start]);
        out.push_str(&redact_span(&rest[start..end], scp));
        rest = &rest[end..];
    }
    out
}

fn find_scheme_url(s: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(rel) = s[from..].find("://") {
        let colon = from + rel;
        let scheme_start = s[..colon]
            .rfind(|c: char| !c.is_ascii_alphabetic())
            .map_or(0, |i| i + 1);
        if scheme_start < colon {
            let scheme = &s[scheme_start..colon];
            if scheme.eq_ignore_ascii_case("http")
                || scheme.eq_ignore_ascii_case("https")
                || scheme.eq_ignore_ascii_case("ssh")
                || scheme.eq_ignore_ascii_case("git")
                || scheme.eq_ignore_ascii_case("file")
            {
                let end = url_end(s, colon + 3);
                return Some((scheme_start, end));
            }
        }
        from = colon + 3;
    }
    None
}

fn url_end(s: &str, after_scheme: usize) -> usize {
    s[after_scheme..]
        .find(|c: char| {
            c.is_ascii_whitespace() || matches!(c, '\'' | '"' | '<' | '>' | ')' | ']' | '`' | ',')
        })
        .map_or(s.len(), |i| after_scheme + i)
}

fn scp_git_end(s: &str, start: usize) -> usize {
    url_end(s, start + 4)
}

fn redact_span(span: &str, scp: bool) -> String {
    if scp {
        let rest = span.strip_prefix("git@").unwrap_or(span);
        let host = rest.split([':', '/', '?', '#']).next().unwrap_or(rest);
        return format!("git@{host}");
    }
    let Some((scheme, rest)) = span.split_once("://") else {
        return String::from("remote");
    };
    let hostport = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    let host = hostport.split(['/', '?', '#']).next().unwrap_or(hostport);
    format!("{scheme}://{host}")
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::io::ErrorKind;
    use std::sync::{Arc, Mutex};

    use protocontract::RuntimeCode;
    use scheda_api::v1::VaultErrorCode as C;

    use super::{
        Context, Error, Fault, GitFailure, default_retryable, git_fault, io, mutex,
        redact_remote_text,
    };

    #[test]
    fn a_report_carries_the_code_name_the_flag_the_stack_and_the_path() {
        let error = Error::from(
            Fault::new(C::Storage, "the disk is full")
                .with_path("notes/a.md")
                .with_retryable(true),
        )
        .context("write")
        .context("round");
        let report = super::error_report(&error);
        assert_eq!(report.code.as_deref(), Some("VAULT_ERROR_CODE_STORAGE"));
        assert_eq!(report.message.as_deref(), Some("the disk is full"));
        assert_eq!(report.retryable, Some(true));
        assert_eq!(report.stack, ["round", "write"]);
        assert_eq!(report.path.as_deref(), Some("notes/a.md"));
    }

    #[test]
    fn a_report_of_a_failure_or_a_foreign_fault_has_a_message_and_no_path() {
        let failure = Error::new(RuntimeCode::Cancelled, "the feed broke");
        let report = super::error_report(&failure);
        assert_eq!(report.code.as_deref(), Some("CANCELLED"));
        assert_eq!(report.message.as_deref(), Some("the feed broke"));
        assert_eq!(report.path, None);

        #[derive(Debug)]
        struct Foreign;
        impl std::fmt::Display for Foreign {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a port fell over")
            }
        }
        impl std::error::Error for Foreign {}
        impl protocontract::Fault for Foreign {
            fn code(&self) -> protocontract::Code {
                protocontract::Code::of(C::Unavailable)
            }
            fn retryable(&self) -> bool {
                true
            }
        }
        let report = super::error_report(&Error::from(Foreign).context("embed"));
        assert_eq!(report.code.as_deref(), Some("VAULT_ERROR_CODE_UNAVAILABLE"));
        assert_eq!(report.retryable, Some(true));
        assert_eq!(report.message.as_deref(), Some("a port fell over"));
        assert_eq!(report.stack, ["embed"]);
    }

    /// The "Retryable" column of the code table, one row per code.
    #[test]
    fn every_code_has_the_retry_flag_of_the_table() {
        let rows = [
            (C::Unspecified, false),
            (C::InvalidArgument, false),
            (C::NotFound, false),
            (C::AlreadyExists, false),
            (C::FailedPrecondition, false),
            (C::Unimplemented, false),
            (C::Aborted, true),
            (C::Unavailable, true),
            (C::Unauthenticated, false),
            (C::NotAVault, false),
            (C::Generation, false),
            (C::Crypto, false),
            (C::MirrorAhead, false),
            (C::MirrorConflict, false),
            (C::UnresolvedConflict, false),
            (C::IndexRebuilding, true),
            (C::IndexStale, false),
            (C::IndexBroken, false),
            (C::Storage, false),
            (C::Replication, false),
            (C::DeadlineExceeded, true),
            (C::DataLoss, false),
        ];
        for (code, retryable) in rows {
            assert_eq!(default_retryable(code), retryable, "{code:?}");
            let error = Error::from(Fault::new(code, "x"));
            assert!(error.is(code), "{code:?}");
            assert_eq!(error.retryable(), retryable, "{code:?}");
        }
    }

    #[test]
    fn io_network_kinds_and_timeouts_are_unavailable_and_retryable() {
        for kind in [
            ErrorKind::NetworkDown,
            ErrorKind::NetworkUnreachable,
            ErrorKind::ConnectionRefused,
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::HostUnreachable,
            ErrorKind::TimedOut,
        ] {
            let error = Error::from(io(std::io::Error::from(kind)));
            assert!(error.is(C::Unavailable), "{kind:?}");
            assert!(error.retryable(), "{kind:?}");
        }
    }

    #[test]
    fn io_passing_kinds_are_storage_and_retryable() {
        for kind in [
            ErrorKind::Interrupted,
            ErrorKind::WouldBlock,
            ErrorKind::ResourceBusy,
        ] {
            let error = Error::from(io(std::io::Error::from(kind)));
            assert!(error.is(C::Storage), "{kind:?}");
            assert!(error.retryable(), "{kind:?}");
        }
    }

    #[test]
    fn io_every_other_kind_is_storage_and_not_retryable() {
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::PermissionDenied,
            ErrorKind::AlreadyExists,
            ErrorKind::StorageFull,
            ErrorKind::WriteZero,
            ErrorKind::InvalidData,
            ErrorKind::UnexpectedEof,
            ErrorKind::Other,
        ] {
            let error = Error::from(io(std::io::Error::from(kind)));
            assert!(error.is(C::Storage), "{kind:?}");
            assert!(!error.retryable(), "{kind:?}");
        }
    }

    #[test]
    fn io_failure_keeps_its_error_kind_through_source() {
        let error = Error::from(io(std::io::Error::from(ErrorKind::StorageFull)));
        let kind = error
            .source()
            .and_then(|source| source.downcast_ref::<std::io::Error>())
            .map(std::io::Error::kind);
        assert_eq!(kind, Some(ErrorKind::StorageFull));
        assert!(!error.is(C::Unavailable));
    }

    #[test]
    fn a_file_failure_keeps_its_path_and_shows_it() {
        let result: Result<(), std::io::Error> = Err(std::io::Error::from(ErrorKind::NotFound));
        let error = result
            .map_err(|err| io(err).with_path("notes/a.md"))
            .context("read")
            .unwrap_err();
        let fault = error.downcast_ref::<Fault>();
        assert_eq!(fault.and_then(Fault::path), Some("notes/a.md"));
        assert!(error.to_string().contains("notes/a.md: "));
        assert!(error.is(C::Storage));
    }

    #[test]
    fn display_without_a_path_is_the_message() {
        let fault = Fault::new(C::NotFound, "no such collection");
        assert_eq!(fault.to_string(), "no such collection");
        assert_eq!(fault.message(), "no such collection");
        assert_eq!(fault.path(), None);
        let with = fault.with_path("a/b.md");
        assert_eq!(with.to_string(), "a/b.md: no such collection");
    }

    #[test]
    fn with_retryable_overrides_the_default() {
        let on = Error::from(Fault::new(C::NotFound, "x").with_retryable(true));
        assert!(on.retryable());
        assert!(on.is(C::NotFound));
        let off = Error::from(Fault::new(C::Aborted, "x").with_retryable(false));
        assert!(!off.retryable());
        assert!(off.is(C::Aborted));
    }

    #[test]
    fn context_frames_come_out_outermost_first() {
        let inner: Result<(), Fault> = Err(Fault::new(C::NotFound, "gone"));
        let error = inner
            .context("read")
            .with_context(|| format!("find {}", 7))
            .context("get")
            .unwrap_err();
        assert_eq!(error.stack(), ["get", "find 7", "read"]);
        assert!(error.is(C::NotFound));
    }

    #[test]
    fn a_poisoned_mutex_is_panicked() {
        let lock = Arc::new(Mutex::new(0_u8));
        let held = Arc::clone(&lock);
        let joined = std::thread::spawn(move || {
            let _guard = held.lock();
            panic!("poison the mutex");
        })
        .join();
        assert!(joined.is_err());
        let error = mutex(&lock, "lock the queue").map(|_| ()).unwrap_err();
        assert!(error.is(RuntimeCode::Panicked));
        assert!(!error.is(C::Storage));
        assert_eq!(error.stack(), ["lock the queue"]);
    }

    #[test]
    fn git_stderr_gives_its_class_code_and_retry_flag() {
        let cases: &[(&str, GitFailure)] = &[
            (
                "fatal: Authentication failed for 'https://github.com/org/repo.git'",
                GitFailure::Auth,
            ),
            ("Permission denied (publickey).", GitFailure::Auth),
            (
                "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
                GitFailure::Auth,
            ),
            (
                "remote: Invalid username or password.\nfatal: Authentication failed",
                GitFailure::Auth,
            ),
            (
                "fatal: unable to access 'https://github.com/foo.git/': SSL certificate problem: unable to get local issuer certificate",
                GitFailure::Certificate,
            ),
            (
                "fatal: unable to access 'https://github.com/foo.git/': server certificate verification failed. CAfile: none CRLfile: none",
                GitFailure::Certificate,
            ),
            (
                "ssh: connect to host github.com port 22: Connection timed out",
                GitFailure::Timeout,
            ),
            (
                "fatal: unable to access 'https://github.com/foo.git/': Failed to connect to github.com port 443 after 21000 ms: Couldn't connect to server",
                GitFailure::Timeout,
            ),
            (
                "ssh: Could not resolve hostname github.com: Name or service not known",
                GitFailure::HostUnreachable,
            ),
            (
                "fatal: unable to access 'https://github.com/foo.git/': Could not resolve host: github.com",
                GitFailure::HostUnreachable,
            ),
            (
                "ssh: connect to host github.com port 22: Network is unreachable",
                GitFailure::HostUnreachable,
            ),
            (
                "ssh: connect to host github.com port 22: Connection refused",
                GitFailure::HostUnreachable,
            ),
            // Not credentials or the network: no class of its own, whatever
            // the text says.
            (
                "fatal: not a git repository (or any of the parent directories): .git",
                GitFailure::Other,
            ),
            (
                "fatal: repository 'https://github.com/foo/bar.git' not found",
                GitFailure::Other,
            ),
            (
                "! [rejected]        main -> main (non-fast-forward)\nerror: failed to push some refs to 'https://github.com/org/repo.git'",
                GitFailure::Other,
            ),
            (
                "fatal: Unable to create '/tmp/vault/.git/index.lock': File exists.",
                GitFailure::Other,
            ),
            (
                "error: Your local changes to the following files would be overwritten by merge:",
                GitFailure::Other,
            ),
            (
                "CONFLICT (content): Merge conflict in notes/a.md",
                GitFailure::Other,
            ),
            ("fatal: remote origin already exists.", GitFailure::Other),
        ];
        for (stderr, want) in cases {
            assert_eq!(GitFailure::classify(stderr), *want, "{stderr}");
            let error = Error::from(git_fault(stderr));
            assert!(error.is(want.code()), "{stderr}");
        }
        assert_eq!(
            GitFailure::classify("some unrecognized git failure"),
            GitFailure::Other
        );
    }

    #[test]
    fn git_classes_map_to_the_codes_of_the_table() {
        let rows = [
            (GitFailure::Auth, C::Unauthenticated, false),
            (GitFailure::Certificate, C::Unauthenticated, false),
            (GitFailure::Timeout, C::Unavailable, true),
            (GitFailure::HostUnreachable, C::Unavailable, true),
            (GitFailure::Other, C::Replication, false),
        ];
        for (failure, code, retryable) in rows {
            assert_eq!(failure.code(), code, "{failure:?}");
            let error = Error::from(Fault::new(failure.code(), "x"));
            assert_eq!(error.retryable(), retryable, "{failure:?}");
        }
    }

    #[test]
    fn a_git_fault_carries_no_credentials() {
        let error = Error::from(git_fault(
            "fatal: Authentication failed for 'https://user:token@example.test/org/repo.git'",
        ));
        assert!(error.is(C::Unauthenticated));
        let shown = error.to_string();
        let source = error.source().map(ToString::to_string).unwrap_or_default();
        for text in [shown, source] {
            assert!(!text.contains("token"), "{text}");
            assert!(!text.contains("user:"), "{text}");
            assert!(text.contains("https://example.test"), "{text}");
        }
    }

    #[test]
    fn a_library_failure_maps_by_its_code_and_class() {
        use git2::{ErrorClass, ErrorCode};
        use scheda_api::v1::VaultErrorCode;

        use super::lib_fault;

        let fault = |code, class| {
            Error::from(lib_fault(git2::Error::new(
                code,
                class,
                "the library said so",
            )))
        };
        let locked = fault(ErrorCode::Locked, ErrorClass::Index);
        assert!(locked.is(VaultErrorCode::Aborted));
        assert!(locked.retryable());
        for class in [ErrorClass::Os, ErrorClass::Filesystem, ErrorClass::NoMemory] {
            let storage = fault(ErrorCode::GenericError, class);
            assert!(storage.is(VaultErrorCode::Storage), "{class:?}");
        }
        for class in [
            ErrorClass::Odb,
            ErrorClass::Object,
            ErrorClass::Zlib,
            ErrorClass::Sha1,
            ErrorClass::Tree,
            ErrorClass::Tag,
            ErrorClass::Index,
            ErrorClass::Indexer,
            ErrorClass::Reference,
            ErrorClass::Repository,
            ErrorClass::Config,
            ErrorClass::Worktree,
        ] {
            let damaged = fault(ErrorCode::NotFound, class);
            assert!(damaged.is(VaultErrorCode::DataLoss), "{class:?}");
            assert!(!damaged.retryable());
        }
        let mismatch = fault(ErrorCode::HashsumMismatch, ErrorClass::None);
        assert!(mismatch.is(VaultErrorCode::DataLoss));
        for class in [
            ErrorClass::Net,
            ErrorClass::Ssh,
            ErrorClass::Http,
            ErrorClass::Merge,
        ] {
            let other = fault(ErrorCode::GenericError, class);
            assert!(other.is(VaultErrorCode::Replication), "{class:?}");
            assert!(!other.retryable());
        }
        // The library's error stays reachable, and the message has no
        // credential in a URL.
        let leaked = Error::from(lib_fault(git2::Error::new(
            ErrorCode::GenericError,
            ErrorClass::Net,
            "failed to connect to https://user:secret@example.test/repo.git",
        )));
        assert!(!leaked.to_string().contains("secret"));
        let source = std::error::Error::source(&leaked).unwrap();
        assert!(source.downcast_ref::<git2::Error>().is_some());
    }

    #[test]
    fn redact_remote_text_drops_userinfo_path_and_query() {
        let out = redact_remote_text(
            "could not find repository from 'https://user:token@example.test/org/repo.git?token=leak'",
        );
        assert!(!out.contains("token"));
        assert!(!out.contains("user:"));
        assert!(!out.contains("/org/"));
        assert!(out.contains("https://example.test"));
        let scp = redact_remote_text("failed git@host.example:org/secret.git");
        assert_eq!(scp, "failed git@host.example");
        assert_eq!(redact_remote_text("no remotes here"), "no remotes here");
    }
}
