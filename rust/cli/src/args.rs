//! Clap surface for the thin CLI (`14`). Command names are `07`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Thin encoding of the Scheda library API (`07`).
///
/// JSON of Records and errors is the Protobuf JSON mapping of `api/v1`.
#[derive(Parser, Debug)]
#[command(
    name = "scheda",
    version = env!("CARGO_PKG_VERSION"),
    about = "Thin encoding of the Scheda library API (07).",
    subcommand_required = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Vault working tree. The only filesystem path the caller supplies.
    #[arg(long, global = true, value_name = "PATH")]
    pub vault: Option<PathBuf>,

    /// GIT-10: pull on open and push after `commit` when a Remote exists.
    /// Default off (ISO-1 / GIT-8).
    #[arg(long, global = true, default_value_t = false)]
    pub auto_sync: bool,

    /// OpenAI-compatible API root (`14`). Joined with `embeddings`.
    /// Must be set with `--embedder-model` and `--embedder-dimensions`, or none.
    #[arg(
        long,
        global = true,
        value_name = "URL",
        conflicts_with = "model2vec_embedder"
    )]
    pub embedder_openai_url: Option<String>,

    /// Embeddings `model` JSON field. With `--embedder-openai-url` and
    /// `--embedder-dimensions`, or none.
    #[arg(
        long,
        global = true,
        value_name = "NAME",
        conflicts_with = "model2vec_embedder"
    )]
    pub embedder_model: Option<String>,

    /// Declared vector length (`Embedder.dimensions`). With
    /// `--embedder-openai-url` and `--embedder-model`, or none.
    #[arg(
        long,
        global = true,
        value_name = "N",
        conflicts_with = "model2vec_embedder"
    )]
    pub embedder_dimensions: Option<usize>,

    /// Env var whose value is `Authorization: Bearer`. Unset → no auth.
    /// Only with the HTTP embedder flags. Missing/empty var → `invalid`.
    #[arg(
        long,
        global = true,
        value_name = "NAME",
        conflicts_with = "model2vec_embedder"
    )]
    pub embedder_api_key_env_var: Option<String>,

    /// Directory with Model2Vec files (`config.json`, `tokenizer.json`,
    /// `model.safetensors`). Unset → vector leg off unless the HTTP
    /// embedder flags are set.
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        conflicts_with_all = [
            "embedder_openai_url",
            "embedder_model",
            "embedder_dimensions",
            "embedder_api_key_env_var"
        ]
    )]
    pub model2vec_embedder: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Open or create a Vault (`07` LIB-4 / LIB-13).
    Open {
        #[arg(long)]
        remote_repo: Option<String>,
    },
    /// `api.v1.VaultInfo` (AGT-1 listing).
    Info,
    /// Write one collection declaration. `--schema` is Protobuf JSON `Schema`.
    #[command(name = "create-collection")]
    CreateCollection {
        name: String,
        #[arg(long)]
        schema: Option<String>,
    },
    /// Collection handle (`07`).
    Collection {
        name: String,
        #[command(subcommand)]
        command: CollectionCommand,
    },
    /// Start a transaction. Later writes do not autocommit until `commit` or `discard`.
    Begin,
    /// One git commit of paths this handle wrote. Ends the transaction.
    Commit {
        #[arg(long, short)]
        message: String,
    },
    /// Restore Scheda-recorded paths to HEAD and end the transaction if any.
    Discard,
    /// Validate Schema, encodings, ids, and conflict markers.
    Check,
    /// Rebuild the derived Index.
    Reindex,
    /// Tantivy Query, then files (`07` LIB-10). Unset `--collections` → this Vault.
    Find {
        #[arg(long, value_delimiter = ',', value_name = "NAME")]
        collections: Vec<String>,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        offset: u32,
        #[arg(long)]
        count: u32,
        /// `FindQuery.include_raw`. Default false: omit `Record.raw`.
        #[arg(long, default_value_t = false)]
        include_raw: bool,
    },
    /// Hybrid search (`07` LIB-11). `--query` is Protobuf JSON `SearchQuery`.
    Search {
        #[arg(long, value_delimiter = ',', value_name = "NAME")]
        collections: Vec<String>,
        #[command(flatten)]
        search: SearchArgs,
    },
    /// Push to the Remote (default `origin`).
    Push,
    /// Fetch and fast-forward from the Remote.
    Pull,
    /// `pull` then `push`.
    Sync,
}

#[derive(Subcommand, Debug)]
pub enum CollectionCommand {
    /// Collection Schema as Protobuf JSON.
    Schema,
    /// Create a Record. `--record` is Protobuf JSON `RecordAdd`.
    Add {
        #[arg(long)]
        id: String,
        #[arg(long)]
        record: Option<String>,
    },
    /// Read one Record by id.
    Get {
        #[arg(long)]
        id: String,
    },
    /// Git history of that Record file (LIB-17).
    History {
        #[arg(long)]
        id: String,
        #[arg(long)]
        offset: u32,
        #[arg(long)]
        count: u32,
    },
    /// Replace a `document` or `singleton`. `--record` is Protobuf JSON `RecordReplace`.
    Replace {
        #[arg(long)]
        id: String,
        #[arg(long)]
        record: Option<String>,
    },
    /// Tantivy Query on this collection (`07` LIB-10).
    Find {
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        offset: u32,
        #[arg(long)]
        count: u32,
        /// `FindQuery.include_raw`. Default false: omit `Record.raw`.
        #[arg(long, default_value_t = false)]
        include_raw: bool,
    },
    /// Hybrid search on this collection (`07` LIB-11).
    Search {
        #[command(flatten)]
        search: SearchArgs,
    },
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Protobuf JSON `SearchQuery`. Omit to read stdin.
    #[arg(long)]
    pub query: Option<String>,
    /// Cohere/Jina API root. Joined with `rerank`. With `--reranker-model`,
    /// or none. Unset → RRF.
    #[arg(long, value_name = "URL")]
    pub reranker_cohere_url: Option<String>,
    /// Rerank `model` JSON field. With `--reranker-cohere-url`, or none.
    #[arg(long, value_name = "NAME")]
    pub reranker_model: Option<String>,
    /// Env var whose value is `Authorization: Bearer`. Unset → no auth.
    /// Only with the HTTP reranker flags. Missing/empty var → `invalid`.
    #[arg(long, value_name = "NAME")]
    pub reranker_api_key_env_var: Option<String>,
    /// `SearchQuery.include_raw`. Default false: omit `Hit.raw`.
    #[arg(long, default_value_t = false)]
    pub include_raw: bool,
}
