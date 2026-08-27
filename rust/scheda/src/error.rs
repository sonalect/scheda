//! Result helpers for generated [`crate::Error`].
//!
//! Constructors, [`crate::Error::context`], and [`crate::Error::with_context`]
//! live on the generated type. This module adds `Result` bubbling (`Context`,
//! `ContextAs`, IO/git maps) and `ensure!` / `bail!`.

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

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

#[must_use]
pub(crate) fn git_code(err: &git2::Error) -> Code {
    use git2::ErrorCode;
    match err.code() {
        ErrorCode::GenericError => Code::GitGeneric,
        ErrorCode::NotFound => Code::GitNotFound,
        ErrorCode::Exists => Code::GitExists,
        ErrorCode::Ambiguous => Code::GitAmbiguous,
        ErrorCode::BufSize => Code::GitBufSize,
        ErrorCode::User => Code::GitUser,
        ErrorCode::BareRepo => Code::GitBareRepo,
        ErrorCode::UnbornBranch => Code::GitUnbornBranch,
        ErrorCode::Unmerged => Code::GitUnmerged,
        ErrorCode::NotFastForward => Code::NotFastForward,
        ErrorCode::InvalidSpec => Code::GitInvalidSpec,
        ErrorCode::Conflict => Code::GitConflict,
        ErrorCode::Locked => Code::GitLocked,
        ErrorCode::Modified => Code::GitModified,
        ErrorCode::Auth => Code::GitAuth,
        ErrorCode::Certificate => Code::GitCertificate,
        ErrorCode::Applied => Code::GitApplied,
        ErrorCode::Peel => Code::GitPeel,
        ErrorCode::Eof => Code::GitEof,
        ErrorCode::Invalid => Code::GitInvalid,
        ErrorCode::Uncommitted => Code::GitUncommitted,
        ErrorCode::Directory => Code::GitDirectory,
        ErrorCode::MergeConflict => Code::GitMergeConflict,
        ErrorCode::HashsumMismatch => Code::GitHashsumMismatch,
        ErrorCode::IndexDirty => Code::GitIndexDirty,
        ErrorCode::ApplyFail => Code::GitApplyFail,
        ErrorCode::Owner => Code::GitOwner,
        ErrorCode::Timeout => Code::GitTimeout,
    }
}

/// Prepend a `stack` frame while bubbling [`crate::Error`].
pub(crate) trait Context<T> {
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
/// by arity, so that would collide with [`ContextIo`] / [`ContextGit`] /
/// [`Context`].
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

/// Map `git2::Error` by [`git_code`] and prepend a frame.
pub(crate) trait ContextGit<T> {
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

impl<T> ContextGit<T> for Result<T, git2::Error> {
    fn with_context<F, C>(self, f: F) -> Result<T, crate::Error>
    where
        F: FnOnce() -> C,
        C: fmt::Display,
    {
        self.map_err(|err| crate::Error::new(git_code(&err), err.message()).context(f()))
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
}
