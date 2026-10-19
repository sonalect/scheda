//! The `scheda` command line. Each subcommand is one rpc of the contract:
//! it opens the Vault, makes the call, prints the reply as Protobuf JSON,
//! one message per line, closes the Vault, and exits. A failure is the JSON
//! of its `ErrorReport` on stderr and exit status 1.

use mimalloc::MiMalloc;

mod args;
mod call;
mod exec;
mod http;
mod model2vec;
mod open;
mod stdio;

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind as ClapErrorKind;
use scheda::v1::ErrorReport;

use args::Cli;
use call::Call;
use scheda::Error;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> ExitCode {
    match run(std::env::args_os()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            emit_report(&scheda::error_report(&error));
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<(), Error> {
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
            return Err(stdio::invalid(err.to_string()));
        }
    };
    let opening = match cli.command {
        args::Command::SchedaInfo => None,
        _ => Some(open::opening(&cli)?),
    };
    let call = Call::prepare(cli.command)?;
    exec::execute(call, opening)
}

/// Writes the report as one line on stderr. There is nowhere else to say
/// that this failed, so a failure to write it is dropped.
fn emit_report(report: &ErrorReport) {
    let mut stderr = std::io::stderr().lock();
    if serde_json::to_writer(&mut stderr, report).is_ok() {
        let _ = stderr.write_all(b"\n");
    }
}
