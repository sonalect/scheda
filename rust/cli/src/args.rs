//! The clap surface of the command line: the global flags that open the
//! Vault and the subcommands, one per rpc of the contract.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// The `scheda` command line: each subcommand is one rpc of the contract.
///
/// The reply is the Protobuf JSON of the contract, one message per line on
/// stdout; a failure is one line of JSON on stderr and exit status 1.
#[derive(Parser, Debug)]
#[command(
    name = "scheda",
    version = env!("CARGO_PKG_VERSION"),
    about = "One rpc of the Scheda contract per subcommand.",
    subcommand_required = true,
    disable_help_subcommand = true
)]
pub(crate) struct Cli {
    /// The Vault folder. Required by every subcommand but `scheda-info`.
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) vault: Option<PathBuf>,

    /// Whether to open an existing Vault, make one, or both. Unset: the
    /// library default (open, or make when there is none).
    #[arg(long, global = true, value_enum, value_name = "MODE")]
    pub(crate) mode: Option<ModeArg>,

    /// Whether a Vault that open makes has a folder of files. On an existing
    /// Vault it must be the Vault's own. Unset: the library default (files).
    #[arg(long, global = true, value_enum, value_name = "WORK_TREE")]
    pub(crate) work_tree: Option<WorkTreeArg>,

    /// The branch a Vault that open makes starts on. Unset: the library
    /// default (`init.defaultBranch`, or `main`).
    #[arg(long, global = true, value_name = "NAME")]
    pub(crate) initial_branch: Option<String>,

    /// The mirror of the Vault (a git remote URL), set at open.
    #[arg(long, global = true, value_name = "URL")]
    pub(crate) mirror: Option<String>,

    /// Pull on open and push after a local commit when a mirror exists.
    #[arg(long, global = true, default_value_t = false)]
    pub(crate) auto_sync: bool,

    /// The author of the commits. With `--author-email`, or neither.
    #[arg(long, global = true, value_name = "NAME")]
    pub(crate) author_name: Option<String>,

    /// The author's e-mail. With `--author-name`, or neither.
    #[arg(long, global = true, value_name = "EMAIL")]
    pub(crate) author_email: Option<String>,

    /// The writer id of this process.
    #[arg(long, global = true, value_name = "ID")]
    pub(crate) writer: Option<String>,

    /// How long a write waits for the write lock, in milliseconds.
    #[arg(long, global = true, value_name = "N")]
    pub(crate) write_timeout_ms: Option<u32>,

    /// How long a query, a count, or a search waits for the Index to reach
    /// the state of the Vault at open, in milliseconds (60 000 unset).
    #[arg(long, global = true, value_name = "N")]
    pub(crate) index_wait_timeout_ms: Option<u32>,

    /// How deep a filter may nest (32 unset, at most 100).
    #[arg(long, global = true, value_name = "N")]
    pub(crate) filter_max_depth: Option<u32>,

    /// Env var that holds the passphrase of encrypted collections.
    #[arg(
        long,
        global = true,
        value_name = "NAME",
        conflicts_with = "encryption_identity_file"
    )]
    pub(crate) encryption_passphrase_env_var: Option<String>,

    /// File with an age identity (`AGE-SECRET-KEY-…`) for encrypted
    /// collections.
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        conflicts_with = "encryption_passphrase_env_var"
    )]
    pub(crate) encryption_identity_file: Option<PathBuf>,

    /// OpenAI-compatible API root, joined with `embeddings`. With
    /// `--embedder-model` and `--embedder-dimensions`, or none.
    #[arg(long, global = true, value_name = "URL")]
    pub(crate) embedder_openai_url: Option<String>,

    /// The `model` field of the embeddings request, and the model id of the
    /// embedder.
    #[arg(long, global = true, value_name = "NAME")]
    pub(crate) embedder_model: Option<String>,

    /// The length of the vectors the HTTP embedder gives.
    #[arg(long, global = true, value_name = "N")]
    pub(crate) embedder_dimensions: Option<u32>,

    /// Env var whose value is the `Authorization: Bearer` token of the HTTP
    /// embedder. Only with the HTTP embedder flags.
    #[arg(long, global = true, value_name = "NAME")]
    pub(crate) embedder_api_key_env_var: Option<String>,

    /// Cohere or Jina rerank API root, joined with `rerank`. With
    /// `--reranker-model`, or neither. Used by a search with `rerankLimit`.
    #[arg(long, global = true, value_name = "URL")]
    pub(crate) reranker_cohere_url: Option<String>,

    /// The `model` field of the rerank request.
    #[arg(long, global = true, value_name = "NAME")]
    pub(crate) reranker_model: Option<String>,

    /// Env var whose value is the `Authorization: Bearer` token of the HTTP
    /// reranker. Only with the HTTP reranker flags.
    #[arg(long, global = true, value_name = "NAME")]
    pub(crate) reranker_api_key_env_var: Option<String>,

    /// Answer a query, a count, or a search from the Index as it is: send
    /// no `notBefore` (by default, the state of the Vault at open, unless
    /// the request sets its own). And do not wait for the rebuild after
    /// `index reindex`.
    #[arg(long, global = true, default_value_t = false)]
    pub(crate) no_wait: bool,

    /// What to call.
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// `--mode`: what `Scheda.Open` does when the folder is, or is not, a Vault.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum ModeArg {
    /// Open the Vault, or make one when the folder holds none.
    OpenOrCreate,
    /// Open a Vault that exists; fail when there is none.
    OpenExisting,
    /// Make a Vault; fail when the folder holds one.
    CreateNew,
}

/// `--work-tree`: whether a Vault that `Scheda.Open` makes has a folder of
/// files.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum WorkTreeArg {
    /// The Vault has a folder of files.
    Files,
    /// The Vault has no folder of files.
    None,
}

/// A subcommand: one rpc, reached from the Vault.
#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// This build of Scheda: its version and features. Opens no Vault.
    SchedaInfo,
    /// The path of the Vault, of its Index, and its writer id.
    Info,
    /// Problems found in the stored files of the Vault.
    Check,
    /// The declarations of every collection.
    ListCollections,
    /// Make a collection. The Schema comes from `--schema` or stdin.
    CreateCollection {
        /// The collection name.
        name: String,
        /// The Schema, as Protobuf JSON.
        #[arg(long, value_name = "JSON")]
        schema: Option<String>,
    },
    /// Replace the Schema of a collection.
    UpdateCollection {
        /// The collection name.
        name: String,
        /// The Schema, as Protobuf JSON.
        #[arg(long, value_name = "JSON")]
        schema: Option<String>,
    },
    /// Delete a collection that holds no Documents (one that does is refused).
    DeleteCollection {
        /// The collection name.
        name: String,
        /// Succeed when there is no such collection.
        #[arg(long, default_value_t = false)]
        ignore_missing: bool,
    },
    /// Several collection operations as one; JSON Lines of
    /// `CollectionOperation` on stdin.
    BatchCollections,
    /// Several Document writes across collections as one; JSON Lines of
    /// `VaultDocumentOperation` on stdin.
    BatchDocuments,
    /// The Documents that match a filter, across collections.
    QueryDocuments {
        /// The `VaultQuery`, as Protobuf JSON.
        #[arg(long, value_name = "JSON")]
        query: Option<String>,
    },
    /// How many Documents `query-documents` would give.
    CountDocuments {
        /// The `VaultQuery`, as Protobuf JSON.
        #[arg(long, value_name = "JSON")]
        query: Option<String>,
    },
    /// The Documents that best fit a text, across collections.
    SearchDocuments {
        /// The `VaultSearch`, as Protobuf JSON.
        #[arg(long, value_name = "JSON")]
        search: Option<String>,
    },
    /// One collection of the Vault.
    Collection {
        /// The collection name.
        name: String,
        /// What to call on the collection.
        #[command(subcommand)]
        command: CollectionCommand,
    },
    /// The mirror of the Vault.
    Mirror {
        /// What to call on the mirror.
        #[command(subcommand)]
        command: MirrorCommand,
    },
    /// The Index of the Vault.
    Index {
        /// What to call on the Index.
        #[command(subcommand)]
        command: IndexCommand,
    },
}

/// A subcommand of `collection NAME`.
#[derive(Subcommand, Debug)]
pub(crate) enum CollectionCommand {
    /// The declaration of the collection.
    Describe,
    /// One Document; `raw` is Base64 in the JSON.
    Get {
        /// The Document id.
        #[arg(long)]
        id: String,
    },
    /// Many Documents: the given ids, or the collection page by page.
    GetMany {
        /// Comma-separated ids. Not with `--limit` or `--start-after`.
        #[arg(
            long,
            value_delimiter = ',',
            value_name = "ID,…",
            conflicts_with_all = ["limit", "start_after"]
        )]
        ids: Option<Vec<String>>,
        /// At most this many Documents of the listing.
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
        /// The listing continues after this id.
        #[arg(long, value_name = "ID")]
        start_after: Option<String>,
        /// Include `raw` in each Document.
        #[arg(long, default_value_t = false)]
        include_raw: bool,
    },
    /// The past versions of a Document, newest first.
    History {
        /// The Document id.
        #[arg(long)]
        id: String,
    },
    /// Create a Document. The bytes come from `--file` or stdin.
    Create {
        /// The Document id.
        #[arg(long)]
        id: String,
        /// Read the bytes from this file instead of stdin.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// The commit note.
        #[arg(long)]
        note: String,
        /// When the Document exists, return it and write nothing.
        #[arg(long, default_value_t = false)]
        keep_existing: bool,
    },
    /// Replace a Document. The bytes come from `--file` or stdin.
    Update {
        /// The Document id.
        #[arg(long)]
        id: String,
        /// Read the bytes from this file instead of stdin.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// The commit note.
        #[arg(long)]
        note: String,
        /// The revision the stored Document must have.
        #[arg(long, value_name = "REV")]
        revision: Option<String>,
    },
    /// Create a Document, or replace it when present.
    Upsert {
        /// The Document id.
        #[arg(long)]
        id: String,
        /// Read the bytes from this file instead of stdin.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// The commit note.
        #[arg(long)]
        note: String,
    },
    /// Delete a Document.
    Delete {
        /// The Document id.
        #[arg(long)]
        id: String,
        /// The commit note.
        #[arg(long)]
        note: String,
        /// The revision the stored Document must have.
        #[arg(long, value_name = "REV")]
        revision: Option<String>,
        /// Succeed when there is no such Document.
        #[arg(long, default_value_t = false)]
        ignore_missing: bool,
    },
    /// Several Document writes as one; JSON Lines of `DocumentOperation`
    /// on stdin.
    Batch,
    /// The Documents that match a filter.
    Query {
        /// The `DocumentQuery`, as Protobuf JSON.
        #[arg(long, value_name = "JSON")]
        query: Option<String>,
    },
    /// How many Documents `query` would give.
    Count {
        /// The `DocumentQuery`, as Protobuf JSON.
        #[arg(long, value_name = "JSON")]
        query: Option<String>,
    },
    /// The Documents that best fit a text, a Document, or a vector.
    Search {
        /// The `DocumentSearch`, as Protobuf JSON.
        #[arg(long, value_name = "JSON")]
        search: Option<String>,
    },
    /// The vectors the Index holds in one `vector` column for some
    /// Documents.
    GetVectors {
        /// The `VectorSelection`, as Protobuf JSON.
        #[arg(long, value_name = "JSON")]
        selection: Option<String>,
    },
}

/// A subcommand of `mirror`.
#[derive(Subcommand, Debug)]
pub(crate) enum MirrorCommand {
    /// The URL of the mirror, its auto-sync flag, and its last failure.
    Info,
    /// Set the mirror.
    Set(MirrorSet),
    /// Remove the mirror.
    Remove,
    /// Push the Vault to the mirror.
    Push,
    /// Pull the mirror into the Vault.
    Pull,
    /// Pull, then push.
    Sync,
}

/// The arguments of `mirror set`.
#[derive(Args, Debug)]
pub(crate) struct MirrorSet {
    /// The mirror URL.
    pub(crate) url: String,
    /// Replace a mirror that is already set.
    #[arg(long, default_value_t = false)]
    pub(crate) replace_existing: bool,
}

/// A subcommand of `index`.
#[derive(Subcommand, Debug)]
pub(crate) enum IndexCommand {
    /// The state of the Index now.
    Status,
    /// The state of the Index, then each change of it.
    Watch {
        /// Stop after the first settled state.
        #[arg(long, default_value_t = false)]
        until_settled: bool,
    },
    /// Rebuild the Index and wait until it is rebuilt.
    Reindex,
}
