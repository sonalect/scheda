use thiserror::Error;

/// Stable failure code shared by tests and agent transport (`07` LIB-5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Code {
    /// Missing Record (`find_one`).
    NotFound,
    /// Caller or Vault input is not usable.
    Invalid,
    /// Git conflict markers or a duplicate `id`.
    Conflict,
    /// Remote update is not a fast-forward.
    NotFastForward,
    /// Another writer holds the Vault lock.
    Locked,
    /// Filesystem failure.
    Io,
    /// git subprocess or repository failure.
    Git,
    /// Index is missing, corrupt, or the wrong format.
    IndexUnusable,
    /// Operation is not offered for this kind or phase (`replace_one` on
    /// `journal`; `search` until the search phase).
    Unsupported,
}

impl Code {
    /// Wire string for tests and `08`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Invalid => "invalid",
            Self::Conflict => "conflict",
            Self::NotFastForward => "not_fast_forward",
            Self::Locked => "locked",
            Self::Io => "io",
            Self::Git => "git",
            Self::IndexUnusable => "index_unusable",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Recoverable failure from a Scheda operation.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{message}")]
pub struct Error {
    code: Code,
    message: String,
}

impl Error {
    #[must_use]
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> Code {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(Code::NotFound, message)
    }

    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(Code::Invalid, message)
    }

    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(Code::Conflict, message)
    }

    #[must_use]
    pub fn not_fast_forward(message: impl Into<String>) -> Self {
        Self::new(Code::NotFastForward, message)
    }

    #[must_use]
    pub fn locked(message: impl Into<String>) -> Self {
        Self::new(Code::Locked, message)
    }

    #[must_use]
    pub fn io(message: impl Into<String>) -> Self {
        Self::new(Code::Io, message)
    }

    #[must_use]
    pub fn git(message: impl Into<String>) -> Self {
        Self::new(Code::Git, message)
    }

    #[must_use]
    pub fn index_unusable(message: impl Into<String>) -> Self {
        Self::new(Code::IndexUnusable, message)
    }

    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(Code::Unsupported, message)
    }
}

#[cfg(test)]
mod tests {
    use super::{Code, Error};

    #[test]
    fn code_strings_are_stable() {
        assert_eq!(Code::NotFound.as_str(), "not_found");
        assert_eq!(Code::Invalid.as_str(), "invalid");
        assert_eq!(Code::Conflict.as_str(), "conflict");
        assert_eq!(Code::NotFastForward.as_str(), "not_fast_forward");
        assert_eq!(Code::Locked.as_str(), "locked");
        assert_eq!(Code::Io.as_str(), "io");
        assert_eq!(Code::Git.as_str(), "git");
        assert_eq!(Code::IndexUnusable.as_str(), "index_unusable");
        assert_eq!(Code::Unsupported.as_str(), "unsupported");
    }

    #[test]
    fn negative_tests_assert_code_not_only_message() {
        let err = Error::not_found("missing id");
        assert_eq!(err.code(), Code::NotFound);
        assert_eq!(err.code().as_str(), "not_found");
        assert_eq!(err.message(), "missing id");
        assert_eq!(err.to_string(), "missing id");
    }

    #[test]
    fn each_constructor_sets_its_code() {
        assert_eq!(Error::invalid("x").code(), Code::Invalid);
        assert_eq!(Error::conflict("x").code(), Code::Conflict);
        assert_eq!(Error::not_fast_forward("x").code(), Code::NotFastForward);
        assert_eq!(Error::locked("x").code(), Code::Locked);
        assert_eq!(Error::io("x").code(), Code::Io);
        assert_eq!(Error::git("x").code(), Code::Git);
        assert_eq!(Error::index_unusable("x").code(), Code::IndexUnusable);
        assert_eq!(Error::unsupported("x").code(), Code::Unsupported);
    }
}
