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
    /// git2 or repository failure.
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

    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "not_found" => Some(Self::NotFound),
            "invalid" => Some(Self::Invalid),
            "conflict" => Some(Self::Conflict),
            "not_fast_forward" => Some(Self::NotFastForward),
            "locked" => Some(Self::Locked),
            "io" => Some(Self::Io),
            "git" => Some(Self::Git),
            "index_unusable" => Some(Self::IndexUnusable),
            "unsupported" => Some(Self::Unsupported),
            _ => None,
        }
    }

    #[must_use]
    pub fn of(err: &crate::Error) -> Option<Self> {
        err.code.as_deref().and_then(Self::parse)
    }
}

#[cfg(test)]
mod tests {
    use super::Code;
    use crate::Error;

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
        assert_eq!(Code::of(&err), Some(Code::NotFound));
        assert_eq!(err.code.as_deref(), Some("not_found"));
        assert_eq!(err.message.as_deref(), Some("missing id"));
        assert_eq!(err.to_string(), "missing id");
    }

    #[test]
    fn each_constructor_sets_its_code() {
        assert_eq!(Code::of(&Error::invalid("x")), Some(Code::Invalid));
        assert_eq!(Code::of(&Error::conflict("x")), Some(Code::Conflict));
        assert_eq!(
            Code::of(&Error::not_fast_forward("x")),
            Some(Code::NotFastForward)
        );
        assert_eq!(Code::of(&Error::locked("x")), Some(Code::Locked));
        assert_eq!(Code::of(&Error::io("x")), Some(Code::Io));
        assert_eq!(Code::of(&Error::git("x")), Some(Code::Git));
        assert_eq!(
            Code::of(&Error::index_unusable("x")),
            Some(Code::IndexUnusable)
        );
        assert_eq!(Code::of(&Error::unsupported("x")), Some(Code::Unsupported));
    }
}
