//! The request that opens the Vault, built from the global flags: the
//! options, the secrets they name, and the ports. Everything wrong with the
//! flags is `INVALID_ARGUMENT` here, before the Vault is touched.

use std::path::Path;

use scheda::Error;
use scheda::v1::embedder_ref::EmbedderRef;
use scheda::v1::encryption::Secret;
use scheda::v1::reranker_ref::RerankerRef;
use scheda::v1::{Author, Encryption, OpenMode, OpenOptions, VaultOpening};

use crate::args::{Cli, ModeArg};
use crate::stdio::{invalid, unreadable};
use crate::{http, model2vec, projector};

/// The request that opens the Vault the flags name.
///
/// # Errors
///
/// `INVALID_ARGUMENT` for a missing `--vault`, an author with only a name
/// or only an e-mail, a secret that cannot be read, and a partial or
/// contradictory set of port flags.
pub(crate) fn opening(cli: &Cli) -> Result<VaultOpening, Error> {
    let path = cli
        .vault
        .as_deref()
        .ok_or_else(|| invalid("missing --vault"))?
        .to_str()
        .ok_or_else(|| invalid("--vault is not valid UTF-8"))?;
    Ok(VaultOpening {
        path: Some(path.to_owned()),
        options: options(cli)?.into(),
        projector: projector::whole_file().into(),
        embedder: embedder(cli)?.into(),
        reranker: reranker(cli)?.into(),
        ..VaultOpening::default()
    })
}

fn options(cli: &Cli) -> Result<OpenOptions, Error> {
    let mut options = OpenOptions::default();
    if let Some(mode) = cli.mode {
        options = options.with_mode(match mode {
            ModeArg::OpenOrCreate => OpenMode::OPEN_MODE_OPEN_OR_CREATE,
            ModeArg::OpenExisting => OpenMode::OPEN_MODE_OPEN_EXISTING,
            ModeArg::CreateNew => OpenMode::OPEN_MODE_CREATE_NEW,
        });
    }
    if let Some(mirror) = &cli.mirror {
        options = options.with_mirror(mirror.as_str());
    }
    if cli.auto_sync {
        options = options.with_auto_sync(true);
    }
    match (&cli.author_name, &cli.author_email) {
        (None, None) => {}
        (Some(name), Some(email)) => {
            options.author = Author::default()
                .with_name(name.as_str())
                .with_email(email.as_str())
                .into();
        }
        _ => {
            return Err(invalid(
                "--author-name and --author-email must both be set, or neither",
            ));
        }
    }
    if let Some(writer) = &cli.writer {
        options = options.with_writer(writer.as_str());
    }
    if let Some(millis) = cli.write_timeout_ms {
        options = options.with_write_timeout_ms(millis);
    }
    if let Some(millis) = cli.index_wait_timeout_ms {
        options = options.with_index_wait_timeout_ms(millis);
    }
    if let Some(depth) = cli.filter_max_depth {
        options = options.with_filter_max_depth(depth);
    }
    options.encryption = encryption(
        cli.encryption_passphrase_env_var.as_deref(),
        cli.encryption_identity_file.as_deref(),
    )?
    .into();
    Ok(options)
}

/// The secret of the encrypted collections: the passphrase in an env var,
/// or the age identity in a file. The secret itself is never an argument.
fn encryption(
    passphrase_env: Option<&str>,
    identity_file: Option<&Path>,
) -> Result<Option<Encryption>, Error> {
    let secret = match (passphrase_env, identity_file) {
        (None, None) => return Ok(None),
        (Some(_), Some(_)) => {
            return Err(invalid(
                "use --encryption-passphrase-env-var or --encryption-identity-file, not both",
            ));
        }
        (Some(name), None) => Secret::Passphrase(env_secret(name)?),
        (None, Some(path)) => {
            let text = std::fs::read_to_string(path)
                .map_err(|err| unreadable(&path.display().to_string(), err))?;
            Secret::Identity(scheda::parse_age_identity(&text)?)
        }
    };
    Ok(Some(Encryption {
        secret: Some(secret),
        ..Encryption::default()
    }))
}

/// The value of the env var `name`; it must be set and not empty.
fn env_secret(name: &str) -> Result<String, Error> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        Ok(_) | Err(_) => Err(invalid(format!(
            "environment variable {name} is not set or empty"
        ))),
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn embedder(cli: &Cli) -> Result<Option<EmbedderRef>, Error> {
    let url = nonempty(cli.embedder_openai_url.as_deref());
    let model = nonempty(cli.embedder_model.as_deref());
    let key_env = nonempty(cli.embedder_api_key_env_var.as_deref());
    let dir = cli.model2vec_embedder.as_deref();
    if dir.is_some()
        && (url.is_some()
            || model.is_some()
            || cli.embedder_dimensions.is_some()
            || key_env.is_some())
    {
        return Err(invalid(
            "use HTTP embedder flags or --model2vec-embedder, not both",
        ));
    }
    match (url, model, cli.embedder_dimensions, dir) {
        (None, None, None, None) if key_env.is_some() => Err(invalid(
            "--embedder-api-key-env-var requires the HTTP embedder flags",
        )),
        (None, None, None, None) => Ok(None),
        (None, None, None, Some(dir)) => model2vec::embedder(dir).map(Some),
        (Some(url), Some(model), Some(dimensions), None) => {
            http::embedder(url, model, dimensions, key_env).map(Some)
        }
        _ => Err(invalid(
            "--embedder-openai-url, --embedder-model, and --embedder-dimensions must all be set, or none",
        )),
    }
}

fn reranker(cli: &Cli) -> Result<Option<RerankerRef>, Error> {
    let url = nonempty(cli.reranker_cohere_url.as_deref());
    let model = nonempty(cli.reranker_model.as_deref());
    let key_env = nonempty(cli.reranker_api_key_env_var.as_deref());
    match (url, model) {
        (None, None) if key_env.is_some() => Err(invalid(
            "--reranker-api-key-env-var requires the HTTP reranker flags",
        )),
        (None, None) => Ok(None),
        (Some(url), Some(model)) => http::reranker(url, model, key_env).map(Some),
        _ => Err(invalid(
            "--reranker-cohere-url and --reranker-model must both be set, or none",
        )),
    }
}
