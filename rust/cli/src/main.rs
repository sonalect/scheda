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

use clap::{error::ErrorKind as ClapErrorKind, Parser};
use scheda::{
    parse_age_identity, schema_from_json, Author, Embedder, Encryption, Error, Filter,
    FindQuery, OpenOptions, Reranker, SearchQuery, Vault,
};
use serde::de::DeserializeOwned;
use serde::Serialize;

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
                err.print()
                    .map_err(|io_err| Error::io_other(io_err.to_string()))?;
                return Ok(());
            }
            return Err(Error::invalid(err.to_string()));
        }
    };
    dispatch(cli)
}

fn dispatch(cli: Cli) -> Result<(), Error> {
    let vault = cli.vault.ok_or_else(|| Error::invalid("missing --vault"))?;
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
        command,
    } = cli;
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
    match command {
        Command::Open { remote_repo } => {
            let _vault = open_vault(
                &vault,
                auto_sync,
                remote_repo,
                author_name,
                author_email,
                encryption,
                embedder,
            )?;
            Ok(())
        }
        Command::Info => emit_json(
            &open_existing(
                &vault,
                auto_sync,
                author_name,
                author_email,
                encryption,
                embedder,
            )?
            .info()?,
        ),
        Command::CreateCollection {
            name,
            schema,
            message,
        } => {
            let vault = open_existing(
                &vault,
                auto_sync,
                author_name,
                author_email,
                encryption,
                embedder,
            )?;
            let schema = schema_from_json(&json_input(
                schema.as_deref(),
                "missing Schema JSON (pass --schema or stdin)",
            )?)?;
            let collection = vault.create_collection(&name, schema, message.as_deref())?;
            emit_json(&collection.schema()?)
        }
        Command::Collection { name, command } => collection_dispatch(
            &open_existing(
                &vault,
                auto_sync,
                author_name,
                author_email,
                encryption,
                embedder,
            )?,
            &name,
            command,
        ),
        Command::Check => open_existing(
            &vault,
            auto_sync,
            author_name,
            author_email,
            encryption,
            embedder,
        )?
        .check(),
        Command::Reindex => emit_json(
            &open_existing(
                &vault,
                auto_sync,
                author_name,
                author_email,
                encryption,
                embedder,
            )?
            .reindex()?,
        ),
        Command::Find {
            collections,
            filter,
            offset,
            count,
            include_raw,
        } => {
            let vault = open_existing(
                &vault,
                auto_sync,
                author_name,
                author_email,
                encryption,
                embedder,
            )?;
            let names = collection_names(&collections);
            let query = parse_find_query(filter.as_deref(), include_raw)?;
            emit_json(&vault.find(&names, &query, offset, count)?)
        }
        Command::Search {
            collections,
            search,
        } => {
            let vault = open_existing(
                &vault,
                auto_sync,
                author_name,
                author_email,
                encryption,
                embedder,
            )?;
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
            emit_json(&vault.search(&names, &query, reranker.as_deref())?)
        }
        Command::Push => open_existing(
            &vault,
            auto_sync,
            author_name,
            author_email,
            encryption,
            embedder,
        )?
        .push(),
        Command::Pull => open_existing(
            &vault,
            auto_sync,
            author_name,
            author_email,
            encryption,
            embedder,
        )?
        .pull(),
        Command::Sync => open_existing(
            &vault,
            auto_sync,
            author_name,
            author_email,
            encryption,
            embedder,
        )?
        .sync(),
        Command::PushIsolated => open_existing(
            &vault,
            auto_sync,
            author_name,
            author_email,
            encryption,
            embedder,
        )?
        .push_isolated(),
        Command::PullIsolated => open_existing(
            &vault,
            auto_sync,
            author_name,
            author_email,
            encryption,
            embedder,
        )?
        .pull_isolated(),
        Command::SyncIsolated => open_existing(
            &vault,
            auto_sync,
            author_name,
            author_email,
            encryption,
            embedder,
        )?
        .sync_isolated(),
    }
}

fn collection_dispatch(vault: &Vault, name: &str, command: CollectionCommand) -> Result<(), Error> {
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

fn read_raw_bytes(file: Option<&Path>) -> Result<Vec<u8>, Error> {
    match file {
        Some(path) => {
            refuse_stdin_with_file()?;
            std::fs::read(path).map_err(|err| Error::io_other(format!("{}: {err}", path.display())))
        }
        None => {
            let mut buf = Vec::new();
            io::stdin()
                .read_to_end(&mut buf)
                .map_err(|err| Error::io_other(err.to_string()))?;
            Ok(buf)
        }
    }
}

fn refuse_stdin_with_file() -> Result<(), Error> {
    use std::io::IsTerminal;
    if io::stdin().is_terminal() {
        return Ok(());
    }
    let mut buf = Vec::new();
    io::stdin()
        .read_to_end(&mut buf)
        .map_err(|err| Error::io_other(err.to_string()))?;
    if buf.is_empty() {
        Ok(())
    } else {
        Err(Error::invalid("--file and stdin together"))
    }
}

fn json_input(flag: Option<&str>, missing: &str) -> Result<String, Error> {
    match flag {
        Some(text) => Ok(text.to_owned()),
        None => {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .map_err(|err| Error::io_other(err.to_string()))?;
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
        .map_err(|err| Error::io_other(err.to_string()))
}

fn emit_json(value: &impl Serialize) -> Result<(), Error> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(|err| Error::invalid(err.to_string()))?;
    stdout
        .write_all(b"\n")
        .map_err(|err| Error::io_other(err.to_string()))
}

fn emit_error(err: &Error) {
    let mut stderr = io::stderr().lock();
    if serde_json::to_writer(&mut stderr, err).is_err() {
        let _ = stderr.write_all(br#"{"code":"CODE_INVALID","message":"failed to encode error"}"#);
    }
    let _ = stderr.write_all(b"\n");
}
