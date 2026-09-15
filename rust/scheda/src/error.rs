//! Result helpers for generated [`crate::Error`].
//!
//! Constructors, [`crate::Error::context`], and [`crate::Error::with_context`]
//! live on the generated type. This module adds `Result` bubbling (`Context`,
//! `ContextAs`, IO and git-CLI maps) and `ensure!` / `bail!`.

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
#[cfg(feature = "find")]
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub use scheda_generated::api::v1::error::{Code, Repeat};

/// Fail closed if a previous holder paniced with this mutex.
pub(crate) fn mutex<'a, T>(
    mutex: &'a Mutex<T>,
    frame: &'static str,
) -> Result<MutexGuard<'a, T>, crate::Error> {
    mutex.lock().map_err(|_: PoisonError<MutexGuard<'a, T>>| {
        crate::Error::io_other("mutex poisoned").context(frame)
    })
}

/// Fail closed if a previous holder paniced with this `RwLock`.
#[cfg(feature = "find")]
pub(crate) fn rw_read<'a, T>(
    lock: &'a RwLock<T>,
    frame: &'static str,
) -> Result<RwLockReadGuard<'a, T>, crate::Error> {
    lock.read()
        .map_err(|_: PoisonError<RwLockReadGuard<'a, T>>| {
            crate::Error::io_other("rwlock poisoned").context(frame)
        })
}

/// Fail closed if a previous holder paniced with this `RwLock`.
#[cfg(feature = "find")]
pub(crate) fn rw_write<'a, T>(
    lock: &'a RwLock<T>,
    frame: &'static str,
) -> Result<RwLockWriteGuard<'a, T>, crate::Error> {
    lock.write()
        .map_err(|_: PoisonError<RwLockWriteGuard<'a, T>>| {
            crate::Error::io_other("rwlock poisoned").context(frame)
        })
}

#[must_use]
pub(crate) fn io_code(err: &std::io::Error) -> Code {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::NotFound => Code::IoNotFound,
        ErrorKind::PermissionDenied => Code::IoPermission,
        ErrorKind::ConnectionRefused => Code::IoConnectionRefused,
        ErrorKind::ConnectionReset => Code::IoConnectionReset,
        ErrorKind::HostUnreachable => Code::IoHostUnreachable,
        ErrorKind::NetworkUnreachable => Code::IoNetworkUnreachable,
        ErrorKind::ConnectionAborted => Code::IoConnectionAborted,
        ErrorKind::NotConnected => Code::IoNotConnected,
        ErrorKind::AddrInUse => Code::IoAddrInUse,
        ErrorKind::AddrNotAvailable => Code::IoAddrNotAvailable,
        ErrorKind::NetworkDown => Code::IoNetworkDown,
        ErrorKind::BrokenPipe => Code::IoBrokenPipe,
        ErrorKind::AlreadyExists => Code::IoAlreadyExists,
        ErrorKind::WouldBlock => Code::IoWouldBlock,
        ErrorKind::NotADirectory => Code::IoNotADirectory,
        ErrorKind::IsADirectory => Code::IoIsADirectory,
        ErrorKind::DirectoryNotEmpty => Code::IoDirectoryNotEmpty,
        ErrorKind::ReadOnlyFilesystem => Code::IoReadOnlyFilesystem,
        ErrorKind::StaleNetworkFileHandle => Code::IoStaleNetworkFileHandle,
        ErrorKind::InvalidInput => Code::IoInvalidInput,
        ErrorKind::InvalidData => Code::IoInvalidData,
        ErrorKind::TimedOut => Code::IoTimedOut,
        ErrorKind::WriteZero => Code::IoWriteZero,
        ErrorKind::StorageFull => Code::IoStorageFull,
        ErrorKind::NotSeekable => Code::IoNotSeekable,
        ErrorKind::QuotaExceeded => Code::IoQuotaExceeded,
        ErrorKind::FileTooLarge => Code::IoFileTooLarge,
        ErrorKind::ResourceBusy => Code::IoResourceBusy,
        ErrorKind::ExecutableFileBusy => Code::IoExecutableFileBusy,
        ErrorKind::Deadlock => Code::IoDeadlock,
        ErrorKind::CrossesDevices => Code::IoCrossesDevices,
        ErrorKind::TooManyLinks => Code::IoTooManyLinks,
        ErrorKind::InvalidFilename => Code::IoInvalidFilename,
        ErrorKind::ArgumentListTooLong => Code::IoArgumentListTooLong,
        ErrorKind::Interrupted => Code::IoInterrupted,
        ErrorKind::Unsupported => Code::IoUnsupported,
        ErrorKind::UnexpectedEof => Code::IoUnexpectedEof,
        ErrorKind::OutOfMemory => Code::IoOutOfMemory,
        _ => Code::IoOther,
    }
}

/// Map a git CLI failure (`exit` + `stderr`) onto [`Code`].
///
/// Patterns are matched on lower-cased `stderr` (Scheda runs git with
/// `LC_ALL=C`). More specific classes win over [`Code::GitGeneric`].
#[must_use]
pub(crate) fn git_code(status: Option<i32>, stderr: &str) -> Code {
    let _ = status;
    let text = stderr.to_ascii_lowercase();
    let hay = text.as_str();
    if matches_auth(hay) {
        return Code::GitAuth;
    }
    if matches_cert(hay) {
        return Code::GitCertificate;
    }
    if matches_timeout(hay) {
        return Code::GitTimeout;
    }
    if matches_nff(hay) {
        return Code::NotFastForward;
    }
    if matches_dirty(hay) {
        return Code::GitUncommitted;
    }
    if matches_unborn(hay) {
        return Code::GitUnbornBranch;
    }
    if matches_locked(hay) {
        return Code::GitLocked;
    }
    if hay.contains("already exists") {
        return Code::GitExists;
    }
    if hay.contains("bare repository") || hay.contains("bare repo") {
        return Code::GitBareRepo;
    }
    if hay.contains("merge conflict") || hay.contains("fix conflicts") {
        return Code::GitMergeConflict;
    }
    if hay.contains("conflict") {
        return Code::GitConflict;
    }
    if hay.contains("not a symbolic ref") || hay.contains("detached") {
        return Code::GitHead;
    }
    if matches_host_unreachable(hay) {
        return Code::IoHostUnreachable;
    }
    if hay.contains("network is unreachable") || hay.contains("network unreachable") {
        return Code::IoNetworkUnreachable;
    }
    if hay.contains("connection refused") {
        return Code::IoConnectionRefused;
    }
    if hay.contains("connection reset") {
        return Code::IoConnectionReset;
    }
    if matches_not_found(hay) {
        return Code::GitNotFound;
    }
    Code::GitGeneric
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

fn matches_nff(hay: &str) -> bool {
    hay.contains("non-fast-forward")
        || hay.contains("not possible to fast-forward")
        || hay.contains("not a fast-forward")
        || hay.contains("updates were rejected because the remote contains work")
        || (hay.contains("failed to push some refs") && hay.contains("rejected"))
}

fn matches_dirty(hay: &str) -> bool {
    hay.contains("your local changes")
        || hay.contains("please commit your changes or stash")
        || hay.contains("not uptodate")
        || hay.contains("uncommitted changes")
        || hay.contains("would be overwritten by")
}

fn matches_unborn(hay: &str) -> bool {
    hay.contains("does not have any commits yet")
        || hay.contains("needed a single revision")
        || hay.contains("unknown revision or path not in the working tree")
        || hay.contains("ambiguous argument 'head'")
}

fn matches_locked(hay: &str) -> bool {
    hay.contains("index.lock")
        || (hay.contains("unable to create") && hay.contains(".lock"))
        || hay.contains("unable to lock")
}

fn matches_host_unreachable(hay: &str) -> bool {
    hay.contains("could not resolve hostname")
        || hay.contains("could not resolve host")
        || hay.contains("name or service not known")
        || hay.contains("temporary failure in name resolution")
        || hay.contains("nodename nor servname")
        || hay.contains("no route to host")
        || hay.contains("host is unreachable")
}

fn matches_not_found(hay: &str) -> bool {
    hay.contains("not a git repository")
        || hay.contains("repository not found")
        || (hay.contains("repository '") && hay.contains("' not found"))
        || (hay.contains("pathspec") && hay.contains("did not match"))
        || hay.contains("no such remote")
        || hay.contains("couldn't find remote ref")
        || hay.contains("could not find remote")
        || hay.contains("no such file or directory")
        || hay.contains("cannot change to")
}

/// Drop userinfo, path, and query from remote URLs in a foreign error string.
///
/// Git and HTTP libraries put the URL they tried into `Display`. Callers
/// must not echo credentials or a full remote path in `Error.message`.
#[doc(hidden)]
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

/// Prepend a `stack` frame while bubbling [`crate::Error`].
pub(crate) trait Context<T> {
    // A build without `find` has no call site of its own. An uncalled
    // provided method is a denied warning, so this method is compiled
    // only when a caller exists.
    #[cfg(any(test, feature = "find"))]
    fn context(self, frame: impl fmt::Display) -> Result<T, crate::Error>
    where
        Self: Sized,
    {
        self.with_context(|| frame)
    }

    fn with_context<F, C>(self, f: F) -> Result<T, crate::Error>
    where
        F: FnOnce() -> C,
        C: fmt::Display;
}

impl<T> Context<T> for Result<T, crate::Error> {
    fn with_context<F, C>(self, f: F) -> Result<T, crate::Error>
    where
        F: FnOnce() -> C,
        C: fmt::Display,
    {
        self.map_err(|err| err.context(f()))
    }
}

/// Convert a listed foreign `Display` error into [`crate::Error`] and prepend a
/// frame. Not a blanket `E: Display` impl: Rust does not disambiguate `context`
/// by arity, so that would collide with [`ContextIo`] / [`Context`].
pub(crate) trait ContextAs<T> {
    fn context(self, code: Code, frame: impl fmt::Display) -> Result<T, crate::Error>
    where
        Self: Sized,
    {
        self.with_context(code, || frame)
    }

    fn with_context<F, C>(self, code: Code, f: F) -> Result<T, crate::Error>
    where
        F: FnOnce() -> C,
        C: fmt::Display;
}

pub(crate) trait ForeignError: fmt::Display {}

impl ForeignError for serde_json::Error {}
impl ForeignError for yaml_serde::Error {}
#[cfg(feature = "find")]
impl ForeignError for tantivy::TantivyError {}
impl ForeignError for std::time::SystemTimeError {}
#[cfg(feature = "search")]
impl ForeignError for cxx::Exception {}
impl ForeignError for std::str::Utf8Error {}
impl ForeignError for std::string::FromUtf8Error {}
impl ForeignError for String {}
impl ForeignError for &str {}

impl<T, E: ForeignError> ContextAs<T> for Result<T, E> {
    fn with_context<F, C>(self, code: Code, f: F) -> Result<T, crate::Error>
    where
        F: FnOnce() -> C,
        C: fmt::Display,
    {
        self.map_err(|err| crate::Error::new(code, err.to_string()).context(f()))
    }
}

/// Map `std::io::Error` by [`io_code`] and prepend a frame.
pub(crate) trait ContextIo<T> {
    fn context(self, frame: impl fmt::Display) -> Result<T, crate::Error>;
}

impl<T> ContextIo<T> for Result<T, std::io::Error> {
    fn context(self, frame: impl fmt::Display) -> Result<T, crate::Error> {
        self.map_err(|err| crate::Error::new(io_code(&err), err.to_string()).context(frame))
    }
}

macro_rules! bail {
    ($err:expr $(,)?) => {
        return ::core::result::Result::Err($err)
    };
}

#[cfg(any(test, feature = "search"))]
macro_rules! ensure {
    ($cond:expr, $err:expr $(,)?) => {
        if !$cond {
            return ::core::result::Result::Err($err);
        }
    };
}

pub(crate) use bail;
#[cfg(feature = "search")]
pub(crate) use ensure;

#[cfg(test)]
mod tests {
    use super::{Code, Context, ContextAs, Repeat};
    use crate::Error;

    #[test]
    fn new_sets_code_message_and_default_repeat() {
        let err = Error::invalid("id is empty");
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert_eq!(err.message.as_deref(), Some("id is empty"));
        assert_eq!(err.known_repeat(), Some(Repeat::Forbidden));
        assert!(err.path.is_none());
        assert!(err.stack.is_empty());
    }

    #[test]
    fn default_repeat_allows_transient_codes() {
        assert_eq!(Error::default_repeat(Code::Locked), Repeat::Allowed);
        assert_eq!(Error::default_repeat(Code::IoNotFound), Repeat::Allowed);
        assert_eq!(
            Error::default_repeat(Code::IoConnectionRefused),
            Repeat::Allowed
        );
        assert_eq!(Error::default_repeat(Code::GitTimeout), Repeat::Allowed);
        assert_eq!(Error::default_repeat(Code::IndexMissing), Repeat::Allowed);
    }

    #[test]
    fn io_code_keeps_error_kinds_distinct() {
        use super::io_code;
        let full = std::io::Error::from(std::io::ErrorKind::StorageFull);
        let zero = std::io::Error::from(std::io::ErrorKind::WriteZero);
        assert_eq!(io_code(&full), Code::IoStorageFull);
        assert_eq!(io_code(&zero), Code::IoWriteZero);
    }

    #[test]
    fn default_repeat_forbids_input_and_identity() {
        assert_eq!(Error::default_repeat(Code::NotFound), Repeat::Forbidden);
        assert_eq!(Error::default_repeat(Code::Invalid), Repeat::Forbidden);
        assert_eq!(Error::default_repeat(Code::Unsupported), Repeat::Forbidden);
        assert_eq!(
            Error::default_repeat(Code::UnsupportedEncryption),
            Repeat::Forbidden
        );
        assert_eq!(Error::default_repeat(Code::Crypto), Repeat::Forbidden);
        assert_eq!(Error::default_repeat(Code::DuplicateId), Repeat::Forbidden);
        assert_eq!(
            Error::default_repeat(Code::NotFastForward),
            Repeat::Forbidden
        );
    }

    #[test]
    fn with_repeat_overrides_the_code_default() {
        let err = Error::io_not_found("gone").with_repeat(Repeat::Forbidden);
        assert_eq!(err.known_code(), Some(Code::IoNotFound));
        assert_eq!(err.known_repeat(), Some(Repeat::Forbidden));
    }

    #[test]
    fn with_path_does_not_change_code_or_repeat() {
        let err = Error::io_not_found("No such file or directory").with_path("notes/a.md");
        assert_eq!(err.path.as_deref(), Some("notes/a.md"));
        assert_eq!(err.known_code(), Some(Code::IoNotFound));
        assert_eq!(err.known_repeat(), Some(Repeat::Allowed));
    }

    #[test]
    fn context_prepends_outermost_frame_first() {
        let err = Error::io_not_found("No such file or directory")
            .with_path("notes/a.md")
            .context("read")
            .context("find_one");
        assert_eq!(err.stack, ["find_one", "read"]);
        assert_eq!(err.path.as_deref(), Some("notes/a.md"));
    }

    #[test]
    fn result_context_prepends_the_same_way() {
        let inner: Result<(), Error> = Err(Error::io_not_found("No such file or directory"));
        let err = inner.context("read").context("find_one").unwrap_err();
        assert_eq!(err.stack, ["find_one", "read"]);
    }

    #[test]
    fn context_as_codes_a_foreign_display_error() {
        let inner: Result<(), &str> = Err("usearch boom");
        let err = inner
            .context(Code::IndexVector, "[new] create new index")
            .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::IndexVector));
        assert_eq!(err.message.as_deref(), Some("usearch boom"));
        assert_eq!(err.stack, ["[new] create new index"]);
        assert_eq!(err.known_repeat(), Some(Repeat::Allowed));
    }

    #[test]
    fn empty_context_frame_is_skipped() {
        let err = Error::invalid("x").context("");
        assert!(err.stack.is_empty());
    }

    #[test]
    fn with_context_formats_interpolated_frame() {
        let n = 16;
        let err = Error::index_vector("x").with_context(|| format!("[new] reserve capacity {n}"));
        assert_eq!(err.stack, ["[new] reserve capacity 16"]);
    }

    #[test]
    fn result_with_context_skips_the_closure_on_ok() {
        let mut ran = false;
        let ok: Result<u8, Error> = Ok(1);
        let value = ok
            .with_context(|| {
                ran = true;
                "frame"
            })
            .unwrap();
        assert_eq!(value, 1);
        assert!(!ran);
    }

    #[test]
    fn result_with_context_runs_the_closure_on_err() {
        let inner: Result<(), Error> = Err(Error::invalid("x"));
        let err = inner
            .with_context(|| format!("[new] reserve capacity {}", 16))
            .unwrap_err();
        assert_eq!(err.stack, ["[new] reserve capacity 16"]);
    }

    fn require_id(id: &str) -> Result<(), Error> {
        ensure!(!id.is_empty(), Error::invalid("id is empty"));
        Ok(())
    }

    fn give_up() -> Result<(), Error> {
        bail!(Error::unsupported("replace_one on journal"));
    }

    #[test]
    fn ensure_returns_the_error_when_the_condition_fails() {
        let err = require_id("").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert_eq!(err.message.as_deref(), Some("id is empty"));
        assert!(require_id("n1").is_ok());
    }

    #[test]
    fn bail_returns_the_error() {
        let err = give_up().unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Unsupported));
        assert_eq!(err.known_repeat(), Some(Repeat::Forbidden));
    }

    #[test]
    fn redact_remote_text_drops_userinfo_path_and_query() {
        use super::redact_remote_text;
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

    #[test]
    fn git_code_maps_literal_stderr_samples() {
        use super::git_code;
        let cases: &[(&str, Code)] = &[
            (
                "fatal: Authentication failed for 'https://github.com/org/repo.git'",
                Code::GitAuth,
            ),
            ("Permission denied (publickey).", Code::GitAuth),
            (
                "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
                Code::GitAuth,
            ),
            (
                "remote: Invalid username or password.\nfatal: Authentication failed",
                Code::GitAuth,
            ),
            (
                "fatal: unable to access 'https://github.com/foo.git/': SSL certificate problem: unable to get local issuer certificate",
                Code::GitCertificate,
            ),
            (
                "fatal: unable to access 'https://github.com/foo.git/': server certificate verification failed. CAfile: none CRLfile: none",
                Code::GitCertificate,
            ),
            (
                "ssh: connect to host github.com port 22: Connection timed out",
                Code::GitTimeout,
            ),
            (
                "fatal: unable to access 'https://github.com/foo.git/': Failed to connect to github.com port 443 after 21000 ms: Couldn't connect to server",
                Code::GitTimeout,
            ),
            (
                "ssh: Could not resolve hostname github.com: Name or service not known",
                Code::IoHostUnreachable,
            ),
            (
                "fatal: unable to access 'https://github.com/foo.git/': Could not resolve host: github.com",
                Code::IoHostUnreachable,
            ),
            (
                "ssh: connect to host github.com port 22: Network is unreachable",
                Code::IoNetworkUnreachable,
            ),
            (
                "ssh: connect to host github.com port 22: Connection refused",
                Code::IoConnectionRefused,
            ),
            (
                "fatal: not a git repository (or any of the parent directories): .git",
                Code::GitNotFound,
            ),
            (
                "fatal: repository 'https://github.com/foo/bar.git' not found",
                Code::GitNotFound,
            ),
            (
                "fatal: pathspec 'notes/missing.md' did not match any files",
                Code::GitNotFound,
            ),
            ("error: No such remote 'origin'", Code::GitNotFound),
            (
                "! [rejected]        main -> main (non-fast-forward)\nerror: failed to push some refs to 'https://github.com/org/repo.git'",
                Code::NotFastForward,
            ),
            (
                "fatal: Not possible to fast-forward, aborting.",
                Code::NotFastForward,
            ),
            (
                "fatal: ambiguous argument 'HEAD': unknown revision or path not in the working tree.",
                Code::GitUnbornBranch,
            ),
            (
                "fatal: your current branch 'main' does not have any commits yet",
                Code::GitUnbornBranch,
            ),
            ("fatal: Needed a single revision", Code::GitUnbornBranch),
            (
                "error: Your local changes to the following files would be overwritten by merge:",
                Code::GitUncommitted,
            ),
            (
                "error: Entry 'notes/a.md' not uptodate. Cannot merge.",
                Code::GitUncommitted,
            ),
            (
                "fatal: Unable to create '/tmp/vault/.git/index.lock': File exists.",
                Code::GitLocked,
            ),
            ("fatal: remote origin already exists.", Code::GitExists),
            (
                "fatal: this operation must be run in a work tree",
                Code::GitGeneric,
            ),
        ];
        for (stderr, want) in cases {
            assert_eq!(git_code(Some(128), stderr), *want, "{stderr}");
        }
        assert_eq!(
            git_code(Some(1), "some unrecognized git failure"),
            Code::GitGeneric
        );
    }
}
