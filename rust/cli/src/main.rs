//! Thin CLI of the library. JSON is the Protobuf JSON mapping of `api/v1`,
//! except `collection … get`, which writes `Record.raw` bytes.

use mimalloc::MiMalloc;

mod args;
mod http;
mod model2vec;
mod projector;

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;

use clap::{Parser, error::ErrorKind as ClapErrorKind};
use scheda::{
    Author, Code, Embedder, Encryption, Error, Filter, FindQuery, OpenOptions, Reranker,
    SearchQuery, Vault, parse_age_identity, schema_from_json,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use args::{Cli, CollectionCommand, Command, SearchArgs};
use projector::cli_projector;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> std::process::ExitCode {
    match run(std::env::args_os()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            emit_error(&err);
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: impl IntoIterator<Item = impl Into<OsString> + Clone>) -> Result<(), Error> {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            if matches!(
                err.kind(),
                ClapErrorKind::DisplayHelp
                    | ClapErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    | ClapErrorKind::DisplayVersion
            ) {
                err.print().map_err(|io_err| io_error(&io_err, None))?;
                return Ok(());
            }
            return Err(Error::invalid(err.to_string()));
        }
    };
    dispatch(cli)
}

fn dispatch(cli: Cli) -> Result<(), Error> {
    let path = cli.vault.ok_or_else(|| Error::invalid("missing --vault"))?;
    let Cli {
        vault: _,
        auto_sync,
        embedder_openai_url,
        embedder_model,
        embedder_dimensions,
        embedder_api_key_env_var,
        model2vec_embedder,
        author_name,
        author_email,
        encryption_passphrase_env_var,
        encryption_identity_file,
        no_wait,
        command,
    } = cli;
    let wait = if no_wait { None } else { Some(INDEX_WAIT) };
    let encryption = load_encryption(
        encryption_passphrase_env_var.as_deref(),
        encryption_identity_file.as_deref(),
    )?;
    let embedder = load_embedder(
        embedder_openai_url.as_deref(),
        embedder_model.as_deref(),
        embedder_dimensions,
        embedder_api_key_env_var.as_deref(),
        model2vec_embedder.as_deref(),
    )?;
    if let Command::Open { remote_repo } = command {
        open_vault(
            &path,
            auto_sync,
            remote_repo,
            author_name,
            author_email,
            encryption,
            embedder,
        )?;
        return Ok(());
    }
    let vault = open_existing(
        &path,
        auto_sync,
        author_name,
        author_email,
        encryption,
        embedder,
    )?;
    match command {
        // Handled above: `open` is the one command that may clone.
        Command::Open { .. } => Ok(()),
        Command::Info => emit_json(&vault.info()?),
        Command::CreateCollection {
            name,
            schema,
            message,
        } => {
            let schema = schema_from_json(&json_input(
                schema.as_deref(),
                "missing Schema JSON (pass --schema or stdin)",
            )?)?;
            let collection = vault.create_collection(&name, schema, message.as_deref())?;
            emit_json(&collection.schema()?)
        }
        Command::Collection { name, command } => collection_dispatch(&vault, &name, command, wait),
        Command::Check => vault.check(),
        Command::Reindex => emit_json(&vault.reindex()?),
        Command::IndexStatus => emit_json(&status_json(&vault.index_status()?)),
        Command::WaitIndexed { timeout_secs } => emit_json(&status_json(
            &vault.wait_indexed(std::time::Duration::from_secs(timeout_secs))?,
        )),
        Command::Find {
            collections,
            filter,
            offset,
            count,
            include_raw,
        } => {
            let names = collection_names(&collections);
            let query = parse_find_query(filter.as_deref(), include_raw)?;
            wait_indexed(&vault, wait)?;
            emit_json(&vault.find(&names, &query, offset, count)?)
        }
        Command::Search {
            collections,
            search,
        } => {
            let names = collection_names(&collections);
            let query = parse_search_query(
                search.query.as_deref(),
                search.include_raw,
                search.rerank_limit,
            )?;
            let reranker = load_reranker(
                search.reranker_cohere_url.as_deref(),
                search.reranker_model.as_deref(),
                search.reranker_api_key_env_var.as_deref(),
            )?;
            wait_indexed(&vault, wait)?;
            emit_json(&vault.search(&names, &query, reranker.as_deref())?)
        }
        Command::Push => vault.push(),
        Command::Pull => vault.pull(),
        Command::Sync => vault.sync(),
        Command::PushIsolated => vault.push_isolated(),
        Command::PullIsolated => vault.pull_isolated(),
        Command::SyncIsolated => vault.sync_isolated(),
    }
}

/// How long `find` / `search` wait for the Index to reach `HEAD`. A CLI
/// process is short-lived: commits made by the previous command are indexed
/// by this one's background indexer, so the query waits for it.
const INDEX_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Wait for the Index before a query, unless `--no-wait` cleared `wait`.
fn wait_indexed(vault: &Vault, wait: Option<std::time::Duration>) -> Result<(), Error> {
    if let Some(timeout) = wait {
        vault.wait_indexed(timeout)?;
    }
    Ok(())
}

/// `IndexStatus` as JSON in the style of the Protobuf JSON mapping the
/// rest of the CLI prints: lowerCamelCase keys, unset fields left out,
/// `lastError` as an `api.v1.Error`.
fn status_json(status: &scheda::IndexStatus) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    let mut put = |key: &str, value: serde_json::Value| {
        if !value.is_null() {
            out.insert(key.to_owned(), value);
        }
    };
    put("head", status.head.clone().into());
    put("isolatedHead", status.isolated_head.clone().into());
    put("lexical", status.lexical.clone().into());
    put("vector", status.vector.clone().into());
    put("isolatedLexical", status.isolated_lexical.clone().into());
    put("isolatedVector", status.isolated_vector.clone().into());
    put("journalIds", status.journal_ids.clone().into());
    put(
        "isolatedJournalIds",
        status.isolated_journal_ids.clone().into(),
    );
    // Proto JSON writes 64-bit integers as strings.
    put(
        "lexicalPending",
        status.lexical_pending.map(|n| n.to_string()).into(),
    );
    put(
        "vectorPending",
        status.vector_pending.map(|n| n.to_string()).into(),
    );
    put("modelMismatch", status.model_mismatch.into());
    put("lexicalRole", status.lexical_role.into());
    put("vectorRole", status.vector_role.into());
    put(
        "lastError",
        status
            .last_error
            .as_ref()
            .and_then(|err| serde_json::to_value(err).ok())
            .into(),
    );
    serde_json::Value::Object(out)
}

fn collection_dispatch(
    vault: &Vault,
    name: &str,
    command: CollectionCommand,
    wait: Option<std::time::Duration>,
) -> Result<(), Error> {
    let collection = vault.collection(name)?;
    match command {
        CollectionCommand::Schema => emit_json(&collection.schema()?),
        CollectionCommand::Add { id, file, message } => {
            collection.add(&id, read_raw_bytes(file.as_deref())?, message.as_deref())
        }
        CollectionCommand::Get { id } => {
            let record = collection.get(&id)?;
            emit_raw(record.raw.as_deref().unwrap_or(&[]))
        }
        CollectionCommand::History { id, offset, count } => {
            emit_json(&collection.history(&id, offset, count)?)
        }
        CollectionCommand::Replace { id, file, message } => {
            collection.replace(&id, read_raw_bytes(file.as_deref())?, message.as_deref())
        }
        CollectionCommand::Remove { id, message } => collection.remove(&id, message.as_deref()),
        CollectionCommand::Documents => {
            let mut records = Vec::new();
            for item in collection.documents()? {
                records.push(item?);
            }
            emit_json(&records)
        }
        CollectionCommand::Ids => emit_json(&collection.ids()?),
        CollectionCommand::Find {
            filter,
            offset,
            count,
            include_raw,
        } => {
            let query = parse_find_query(filter.as_deref(), include_raw)?;
            wait_indexed(vault, wait)?;
            emit_json(&collection.find(&query, offset, count)?)
        }
        CollectionCommand::Search {
            search:
                SearchArgs {
                    query,
                    reranker_cohere_url,
                    reranker_model,
                    reranker_api_key_env_var,
                    rerank_limit,
                    include_raw,
                },
        } => {
            let query = parse_search_query(query.as_deref(), include_raw, rerank_limit)?;
            let reranker = load_reranker(
                reranker_cohere_url.as_deref(),
                reranker_model.as_deref(),
                reranker_api_key_env_var.as_deref(),
            )?;
            wait_indexed(vault, wait)?;
            emit_json(&collection.search(&query, reranker.as_deref())?)
        }
    }
}

fn open_options(
    auto_sync: bool,
    remote_repo: Option<String>,
    author_name: Option<String>,
    author_email: Option<String>,
    encryption: Option<Encryption>,
) -> OpenOptions {
    let mut options = OpenOptions::new().auto_sync(auto_sync);
    if let Some(url) = remote_repo {
        options = options.remote_repo(url);
    }
    if let Some(encryption) = encryption {
        options = options.encryption(encryption);
    }
    match (author_name, author_email) {
        (None, None) => options,
        (name, email) => {
            let mut author = Author::new();
            if let Some(name) = name {
                author = author.name(name);
            }
            if let Some(email) = email {
                author = author.email(email);
            }
            options.author(author)
        }
    }
}

fn open_vault(
    path: &Path,
    auto_sync: bool,
    remote_repo: Option<String>,
    author_name: Option<String>,
    author_email: Option<String>,
    encryption: Option<Encryption>,
    embedder: Option<Arc<dyn Embedder>>,
) -> Result<Vault, Error> {
    Vault::open_with(
        path,
        open_options(
            auto_sync,
            remote_repo,
            author_name,
            author_email,
            encryption,
        ),
        embedder,
        Some(cli_projector()),
    )
}

fn open_existing(
    path: &Path,
    auto_sync: bool,
    author_name: Option<String>,
    author_email: Option<String>,
    encryption: Option<Encryption>,
    embedder: Option<Arc<dyn Embedder>>,
) -> Result<Vault, Error> {
    Vault::open_with(
        path,
        open_options(auto_sync, None, author_name, author_email, encryption),
        embedder,
        Some(cli_projector()),
    )
}

fn load_encryption(
    pass_env: Option<&str>,
    identity_file: Option<&Path>,
) -> Result<Option<Encryption>, Error> {
    let pass_env = nonempty(pass_env);
    match (pass_env, identity_file) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(Error::invalid(
            "use --encryption-passphrase-env-var or --encryption-identity-file, not both",
        )),
        (Some(name), None) => match std::env::var(name) {
            Ok(value) if !value.is_empty() => Ok(Some(Encryption::new().passphrase(value))),
            Ok(_) | Err(_) => Err(Error::invalid(format!(
                "environment variable {name} is not set or empty"
            ))),
        },
        (None, Some(path)) => {
            let text = std::fs::read_to_string(path)
                .map_err(|_| Error::invalid("encryption identity file is unreadable"))?;
            let identity = parse_age_identity(&text)?;
            Ok(Some(Encryption::new().identity(identity)))
        }
    }
}

fn collection_names(names: &[String]) -> Vec<&str> {
    names.iter().map(String::as_str).collect()
}

fn load_embedder(
    url: Option<&str>,
    model: Option<&str>,
    dimensions: Option<usize>,
    key_env: Option<&str>,
    model2vec_dir: Option<&Path>,
) -> Result<Option<Arc<dyn Embedder>>, Error> {
    let url = nonempty(url);
    let model = nonempty(model);
    let key_env = nonempty(key_env);
    if model2vec_dir.is_some()
        && (url.is_some() || model.is_some() || dimensions.is_some() || key_env.is_some())
    {
        return Err(Error::invalid(
            "use HTTP embedder flags or --model2vec-embedder, not both",
        ));
    }
    match (url, model, dimensions, model2vec_dir) {
        (None, None, None, None) => {
            if key_env.is_some() {
                return Err(Error::invalid(
                    "--embedder-api-key-env-var requires the HTTP embedder flags",
                ));
            }
            Ok(None)
        }
        (None, None, None, Some(dir)) => Ok(Some(model2vec::load_embedder(dir)?)),
        (Some(url), Some(model), Some(dimensions), None) => {
            Ok(Some(http::load_embedder(url, model, dimensions, key_env)?))
        }
        _ => Err(Error::invalid(
            "--embedder-openai-url, --embedder-model, and --embedder-dimensions must all be set, or none",
        )),
    }
}

fn load_reranker(
    url: Option<&str>,
    model: Option<&str>,
    key_env: Option<&str>,
) -> Result<Option<Arc<dyn Reranker>>, Error> {
    let url = nonempty(url);
    let model = nonempty(model);
    let key_env = nonempty(key_env);
    match (url, model) {
        (None, None) => {
            if key_env.is_some() {
                return Err(Error::invalid(
                    "--reranker-api-key-env-var requires the HTTP reranker flags",
                ));
            }
            Ok(None)
        }
        (Some(url), Some(model)) => Ok(Some(http::load_reranker(url, model, key_env)?)),
        _ => Err(Error::invalid(
            "--reranker-cohere-url and --reranker-model must both be set, or none",
        )),
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn parse_filter(json: Option<&str>) -> Result<Filter, Error> {
    let filter: Filter = parse_json(&json_input(
        json,
        "missing filter JSON (pass --filter or stdin)",
    )?)?;
    filter.require_valid()?;
    Ok(filter)
}

fn parse_find_query(json: Option<&str>, include_raw: bool) -> Result<FindQuery, Error> {
    let mut query = FindQuery::from(parse_filter(json)?);
    if include_raw {
        query = query.with_include_raw(true);
    }
    query.require_valid()?;
    Ok(query)
}

fn parse_search_query(
    json: Option<&str>,
    include_raw: bool,
    rerank_limit: Option<u32>,
) -> Result<SearchQuery, Error> {
    let mut query: SearchQuery = parse_json(&json_input(
        json,
        "missing query JSON (pass --query or stdin)",
    )?)?;
    if include_raw {
        query = query.with_include_raw(true);
    }
    if let Some(limit) = rerank_limit {
        query = query.with_rerank_limit(limit);
    }
    query.require_valid()?;
    Ok(query)
}

/// Record bytes from `--file`, or from stdin when no file is given.
/// With `--file`, stdin is not read at all: an open pipe that nobody
/// closes must not block the command.
fn read_raw_bytes(file: Option<&Path>) -> Result<Vec<u8>, Error> {
    match file {
        Some(path) => std::fs::read(path).map_err(|err| io_error(&err, Some(path))),
        None => {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .map_err(|err| io_error(&err, None))?;
            Ok(buf)
        }
    }
}

/// `std::io::Error` as a typed `Error` through the library's I/O code
/// table. The code follows `ErrorKind`; the message carries the OS text
/// and, for `--file`, the path as the user typed it. `Error.path` stays
/// unset: it is for Vault-relative paths only.
fn io_error(err: &io::Error, file: Option<&Path>) -> Error {
    let code = match err.kind() {
        io::ErrorKind::NotFound => Code::IoNotFound,
        io::ErrorKind::PermissionDenied => Code::IoPermission,
        io::ErrorKind::IsADirectory => Code::IoIsADirectory,
        io::ErrorKind::NotADirectory => Code::IoNotADirectory,
        io::ErrorKind::InvalidData => Code::IoInvalidData,
        io::ErrorKind::InvalidInput => Code::IoInvalidInput,
        io::ErrorKind::Interrupted => Code::IoInterrupted,
        io::ErrorKind::BrokenPipe => Code::IoBrokenPipe,
        io::ErrorKind::TimedOut => Code::IoTimedOut,
        io::ErrorKind::UnexpectedEof => Code::IoUnexpectedEof,
        _ => Code::IoOther,
    };
    match file {
        Some(path) => Error::new(code, format!("{}: {err}", path.display())),
        None => Error::new(code, err.to_string()),
    }
}

fn json_input(flag: Option<&str>, missing: &str) -> Result<String, Error> {
    match flag {
        Some(text) => Ok(text.to_owned()),
        None => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .map_err(|err| io_error(&err, None))?;
            if buf.trim().is_empty() {
                Err(Error::invalid(missing))
            } else {
                Ok(buf)
            }
        }
    }
}

fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, Error> {
    serde_json::from_str(text).map_err(|err| Error::invalid(err.to_string()))
}

fn emit_raw(bytes: &[u8]) -> Result<(), Error> {
    io::stdout()
        .lock()
        .write_all(bytes)
        .map_err(|err| io_error(&err, None))
}

fn emit_json(value: &impl Serialize) -> Result<(), Error> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(|err| Error::invalid(err.to_string()))?;
    stdout.write_all(b"\n").map_err(|err| io_error(&err, None))
}

fn emit_error(err: &Error) {
    let mut stderr = io::stderr().lock();
    if serde_json::to_writer(&mut stderr, err).is_err() {
        let _ = stderr.write_all(br#"{"code":"CODE_INVALID","message":"failed to encode error"}"#);
    }
    let _ = stderr.write_all(b"\n");
}
