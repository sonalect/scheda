//! Clap surface for the thin CLI. Command names match the library methods.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Thin encoding of the Scheda library API.
///
/// JSON of Records and errors is the Protobuf JSON mapping of `api/v1`.
#[derive(Parser, Debug)]
#[command(
    name = "scheda",
    version = env!("CARGO_PKG_VERSION"),
    about = "Thin encoding of the Scheda library API.",
    subcommand_required = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Vault working tree. The only filesystem path the caller supplies.
    #[arg(long, global = true, value_name = "PATH")]
    pub vault: Option<PathBuf>,

    /// Pull on open and push after a local commit when a Remote exists.
    /// Default off: `open` stays local.
    #[arg(long, global = true, default_value_t = false)]
    pub auto_sync: bool,

    /// OpenAI-compatible API root. Joined with `embeddings`.
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

    /// Git author and committer name. With `--author-email`, or neither.
    #[arg(long, global = true, value_name = "NAME")]
    pub author_name: Option<String>,

    /// Git author and committer address. With `--author-name`, or neither.
    #[arg(long, global = true, value_name = "EMAIL")]
    pub author_email: Option<String>,

    /// Env var whose value is `OpenOptions.encryption.passphrase`.
    /// Missing or empty var → `invalid` before `open`. Not with
    /// `--encryption-identity-file`.
    #[arg(
        long,
        global = true,
        value_name = "NAME",
        conflicts_with = "encryption_identity_file"
    )]
    pub encryption_passphrase_env_var: Option<String>,

    /// Age identity file (`AGE-SECRET-KEY-…`, `age-keygen` comments allowed).
    /// Unreadable or malformed → `invalid` before `open`. Not with
    /// `--encryption-passphrase-env-var`.
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        conflicts_with = "encryption_passphrase_env_var"
    )]
    pub encryption_identity_file: Option<PathBuf>,

    /// `find` / `search` query the Index as it is instead of waiting up to
    /// 60 s for it to reach `HEAD`. Commits this or a previous command made
    /// may then be missing from the result.
    #[arg(long, global = true, default_value_t = false)]
    pub no_wait: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Open or create a Vault. `--remote-repo` fills a missing path from that
    /// URL (absolute URI or scp-style `git@host:path`); on an existing Vault
    /// it must match `origin`.
    Open {
        #[arg(long)]
        remote_repo: Option<String>,
    },
    /// `api.v1.VaultInfo`: path, Index path, `remote`, `auto_sync`, collections.
    Info,
    /// Write one collection Schema. `--schema` is Protobuf JSON `Schema`.
    #[command(name = "create-collection")]
    CreateCollection {
        name: String,
        #[arg(long)]
        schema: Option<String>,
        /// Git commit subject. Unset → `scheda`. Empty → `invalid`.
        #[arg(long)]
        message: Option<String>,
    },
    /// Collection handle.
    Collection {
        name: String,
        #[command(subcommand)]
        command: CollectionCommand,
    },
    /// Validate declarations, encodings, ids, and conflict markers.
    Check,
    /// Rebuild the derived Index from the commits at `HEAD`.
    Reindex,
    /// Where the Index stands against `HEAD`: each leg's commit, Records
    /// still to index, roles this process holds, `modelMismatch`, and the
    /// last indexing error. Does not wait.
    #[command(name = "index-status")]
    IndexStatus,
    /// Wait until the Index reaches `HEAD`, then print `index-status`.
    /// Past the timeout: `io_timed_out`.
    #[command(name = "wait-indexed")]
    WaitIndexed {
        /// Longest wait.
        #[arg(long, default_value_t = 60, value_name = "N")]
        timeout_secs: u64,
    },
    /// Tantivy Query, then files. Unset `--collections` → every non-isolated,
    /// non-encrypted collection that is in the lexical Index.
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
    /// Hybrid search (BM25, ANN, RRF). `--query` is Protobuf JSON `SearchQuery`.
    /// Unset `--collections` → every non-isolated, non-encrypted collection
    /// that has at least one Index leg.
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
    /// Push `{vault}/.isolated/` to its own Remote. Does not touch the main tree.
    #[command(name = "push-isolated")]
    PushIsolated,
    /// Pull `{vault}/.isolated/` from its own Remote.
    #[command(name = "pull-isolated")]
    PullIsolated,
    /// Isolated `pull` then `push`.
    #[command(name = "sync-isolated")]
    SyncIsolated,
}

#[derive(Subcommand, Debug)]
pub enum CollectionCommand {
    /// Collection Schema as Protobuf JSON.
    #[command(name = "schema")]
    Schema,
    /// Create a Record. `--file` is UTF-8 bytes, or stdin when omitted.
    Add {
        #[arg(long)]
        id: String,
        #[arg(long)]
        file: Option<PathBuf>,
        /// Git commit subject. Unset → `scheda`. Empty → `invalid`.
        #[arg(long)]
        message: Option<String>,
    },
    /// Read one Record by id. Stdout is `Record.raw` bytes, not JSON.
    Get {
        #[arg(long)]
        id: String,
    },
    /// Git history of that Record file. Newest first.
    History {
        #[arg(long)]
        id: String,
        #[arg(long)]
        offset: u32,
        #[arg(long)]
        count: u32,
    },
    /// Replace a `document` or `singleton`. `--file` is UTF-8 bytes, or stdin when omitted.
    Replace {
        #[arg(long)]
        id: String,
        #[arg(long)]
        file: Option<PathBuf>,
        /// Git commit subject. Unset → `scheda`. Empty → `invalid`.
        #[arg(long)]
        message: Option<String>,
    },
    /// Delete a `document` or `singleton`. `journal` → `unsupported`.
    Remove {
        #[arg(long)]
        id: String,
        /// Git commit subject. Unset → `scheda`. Empty → `invalid`.
        #[arg(long)]
        message: Option<String>,
    },
    /// Walk Record files of this `document` or `singleton` in `id` order.
    /// `journal` → `unsupported`. Encrypted collection without a secret →
    /// `crypto`.
    Documents,
    /// Ids of this `document` or `singleton` in the same order as `documents`.
    /// `journal` → `unsupported`. Encrypted collections do not need a secret.
    Ids,
    /// Tantivy Query on this collection, then files.
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
    /// Hybrid search on this collection (BM25, ANN, RRF).
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
    /// `SearchQuery.rerank_limit`. Unset → score every fused hit at pool
    /// `max(k, 50)`. Set to N → both legs retrieve `max(k, 50, N)` and the
    /// reranker scores the top N. Ignored when no reranker is passed. Zero
    /// and a value below `k` are `invalid`.
    #[arg(long, value_name = "N")]
    pub rerank_limit: Option<u32>,
    /// `SearchQuery.include_raw`. Default false: omit `Hit.raw`.
    #[arg(long, default_value_t = false)]
    pub include_raw: bool,
}
