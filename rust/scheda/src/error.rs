use thiserror::Error;

/// Recoverable failure from a Scheda operation.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum Error {
    /// The request was refused. The message is for humans; a typed code comes later.
    #[error("{message}")]
    Rejected { message: String },
}

impl Error {
    pub fn rejected(message: impl Into<String>) -> Self {
        Self::Rejected {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn rejected_displays_message() {
        let err = Error::rejected("missing id");
        assert_eq!(err.to_string(), "missing id");
    }
}
