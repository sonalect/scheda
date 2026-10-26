//! The `scheda` command line. Each subcommand is one rpc of the contract:
//! it opens the Vault, makes the call, prints the reply as Protobuf JSON,
//! one message per line, closes the Vault, and exits. A failure is the JSON
//! of its `ErrorReport` on stderr and exit status 1.
//!
//! The flags and the input (a file, stdin) are read first, on the main
//! thread; the call then runs on a multi-thread tokio runtime, as Scheda's
//! objects are async.

use mimalloc::MiMalloc;

mod args;
mod call;
mod exec;
mod http;
mod open;
mod projector;
mod stdio;

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind as ClapErrorKind;
use protocontract::ErrorCode;
use scheda::v1::{ErrorReport, VaultErrorCode};
use scheda::{Error, Fault};

use args::Cli;
use call::Call;
use exec::Failed;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> ExitCode {
    match run(std::env::args_os()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failed) => {
            let report = match failed {
                Failed::Error(error) => scheda::error_report(&error),
                Failed::Report(report) => *report,
            };
            emit_report(&with_hint(report));
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<(), Failed> {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            if matches!(
                err.kind(),
                ClapErrorKind::DisplayHelp | ClapErrorKind::DisplayVersion
            ) {
                // A closed reader of the help text is not a failure either.
                let _ = err.print();
                return Ok(());
            }
            return Err(stdio::invalid(err.to_string()).into());
        }
    };
    let opening = match cli.command {
        args::Command::SchedaInfo => None,
        _ => Some(open::opening(&cli)?),
    };
    let wait = !cli.no_wait;
    let call = Call::prepare(cli.command)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            Error::from(
                Fault::new(VaultErrorCode::Storage, "cannot start the async runtime")
                    .with_source(err),
            )
        })?;
    runtime.block_on(exec::execute(call, opening, wait))
}

/// The report with what a person does about it, when there is one: an Index
/// built by other ports than this command line gives (its Projector is
/// `scheda-cli/whole-file@2` now) is read again by one `index reindex`.
fn with_hint(mut report: ErrorReport) -> ErrorReport {
    if report.code.as_deref() == Some(ErrorCode::name(VaultErrorCode::IndexStale)) {
        let message = report.message.take().unwrap_or_default();
        report.message = Some(format!(
            "{message}; run `scheda index reindex` once to build the Index again"
        ));
    }
    report
}

/// Writes the report as one line on stderr. There is nowhere else to say
/// that this failed, so a failure to write it is dropped.
fn emit_report(report: &ErrorReport) {
    let mut stderr = std::io::stderr().lock();
    if serde_json::to_writer(&mut stderr, report).is_ok() {
        let _ = stderr.write_all(b"\n");
    }
}
