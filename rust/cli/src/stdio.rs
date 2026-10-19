//! What the command line reads and writes: the replies on stdout, the
//! request text from flags, files and stdin, and the failures of both.

use std::future::poll_fn;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::pin::pin;

use protocontract::Stream;
use scheda::v1::VaultErrorCode;
use scheda::{Error, Fault};
use serde::Serialize;

/// Whether the reader of stdout is still there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// The line was written.
    Open,
    /// The reader closed stdout: stop writing, quietly.
    Closed,
}

/// `INVALID_ARGUMENT` with `message`: the caller's flags or input are wrong.
pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Fault::new(VaultErrorCode::InvalidArgument, message).into()
}

/// `INVALID_ARGUMENT` for an input the caller named that cannot be read.
/// The message names `what` as the user typed it; the `io::Error` stays the
/// source. `Fault.path` is for Vault-relative paths and stays unset.
pub(crate) fn unreadable(what: &str, err: io::Error) -> Error {
    Fault::new(
        VaultErrorCode::InvalidArgument,
        format!("cannot read {what}: {err}"),
    )
    .with_source(err)
    .into()
}

/// Writes `value` as one line of Protobuf JSON on stdout and flushes it.
///
/// A closed reader (`BrokenPipe`) is not a failure: it is [`Flow::Closed`].
///
/// # Errors
///
/// `STORAGE` when the reply cannot be encoded or stdout fails otherwise.
pub(crate) fn emit(value: &impl Serialize) -> Result<Flow, Error> {
    let mut line = serde_json::to_vec(value).map_err(|err| {
        Error::from(Fault::new(VaultErrorCode::Storage, "cannot encode the reply").with_source(err))
    })?;
    line.push(b'\n');
    let mut stdout = io::stdout().lock();
    match stdout.write_all(&line).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(Flow::Open),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(Flow::Closed),
        Err(err) => Err(
            Fault::new(VaultErrorCode::Storage, "cannot write to stdout")
                .with_source(err)
                .into(),
        ),
    }
}

/// Writes every item of `stream` with [`emit`], each as it comes. The items
/// before a failure stay written.
///
/// # Errors
///
/// The failure of the stream, or of [`emit`].
pub(crate) async fn emit_stream<T: Serialize>(
    stream: impl Stream<Item = Result<T, Error>>,
) -> Result<(), Error> {
    let mut stream = pin!(stream);
    while let Some(item) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        if emit(&item?)? == Flow::Closed {
            break;
        }
    }
    Ok(())
}

/// The JSON of a request: the flag when given, all of stdin otherwise,
/// parsed strictly by `parse`.
///
/// # Errors
///
/// `INVALID_ARGUMENT` when stdin is empty or cannot be read, and the
/// failure of `parse`.
pub(crate) fn json_arg<T>(
    flag: Option<&str>,
    name: &str,
    parse: fn(&[u8]) -> Result<T, Error>,
) -> Result<T, Error> {
    match flag {
        Some(text) => parse(text.as_bytes()),
        None => {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .map_err(|err| unreadable("stdin", err))?;
            if buf.iter().all(u8::is_ascii_whitespace) {
                return Err(invalid(format!("missing {name} (pass {name} or stdin)")));
            }
            parse(&buf)
        }
    }
}

/// The bytes of a Document: the file when given, all of stdin otherwise.
/// With a file, stdin is not read at all: an open pipe nobody closes must
/// not block the command.
///
/// # Errors
///
/// `INVALID_ARGUMENT` when the file or stdin cannot be read.
pub(crate) fn raw_bytes(file: Option<&Path>) -> Result<Vec<u8>, Error> {
    match file {
        Some(path) => {
            std::fs::read(path).map_err(|err| unreadable(&path.display().to_string(), err))
        }
        None => {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .map_err(|err| unreadable("stdin", err))?;
            Ok(buf)
        }
    }
}

/// The non-blank lines of stdin, each parsed as the JSON of one message,
/// read whole before the Vault is opened. A line that does not parse, or
/// stdin failing, is an `Err` item that names the line, and the items end
/// there: the batch that gets them fails with the index of that item and
/// writes nothing.
pub(crate) fn json_lines<T>(parse: fn(&[u8]) -> Result<T, Error>) -> Vec<Result<T, Error>> {
    JsonLines {
        reader: BufReader::new(io::stdin()),
        line: 0,
        parse,
        ended: false,
    }
    .collect()
}

/// The iterator of [`json_lines`].
struct JsonLines<T> {
    reader: BufReader<io::Stdin>,
    /// The number of lines read so far, blank ones included.
    line: usize,
    parse: fn(&[u8]) -> Result<T, Error>,
    ended: bool,
}

impl<T> Iterator for JsonLines<T> {
    type Item = Result<T, Error>;

    fn next(&mut self) -> Option<Result<T, Error>> {
        if self.ended {
            return None;
        }
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match self.reader.read_until(b'\n', &mut buf) {
                Ok(0) => {
                    self.ended = true;
                    return None;
                }
                Ok(_) => {}
                Err(err) => {
                    self.ended = true;
                    return Some(Err(unreadable("stdin", err)));
                }
            }
            self.line += 1;
            if buf.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            return Some((self.parse)(&buf).map_err(|err| {
                self.ended = true;
                let message = scheda::error_report(&err).message.unwrap_or_default();
                Error::from(
                    Fault::new(
                        VaultErrorCode::InvalidArgument,
                        format!("line {} of stdin: {message}", self.line),
                    )
                    .with_source(err),
                )
            }));
        }
    }
}
