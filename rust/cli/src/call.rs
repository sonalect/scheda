//! A subcommand turned into the request of its rpc. Everything the caller
//! supplies (flags, a file, stdin) is read and checked here, before the
//! Vault is opened, so a bad input writes nothing.

use scheda::parse_command;
use scheda::v1::document_selection::Selection;
use scheda::v1::{
    CollectionDeclaration, CollectionDeletion, CollectionOperation, DocumentCreation,
    DocumentDeletion, DocumentId, DocumentOperation, DocumentQuery, DocumentSearch,
    DocumentSelection, DocumentUpdate, DocumentUpsert, Ids, Listing, MirrorSetting,
    VaultDocumentOperation, VaultErrorCode, VaultQuery, VaultSearch,
};
use scheda::{Error, Fault};

use crate::args::{CollectionCommand, Command, IndexCommand, MirrorCommand};
use crate::stdio::{json_arg, json_lines, raw_bytes};

/// One rpc, with its request when it takes one. A batch holds the
/// operations it read from stdin, one per line.
pub(crate) enum Call {
    /// `Scheda.Info`: the one call that opens no Vault.
    SchedaInfo,
    /// `Vault.Info`.
    Info,
    /// `Vault.Check`.
    Check,
    /// `Vault.ListCollections`.
    ListCollections,
    /// `Vault.CreateCollection`.
    CreateCollection(CollectionDeclaration),
    /// `Vault.UpdateCollection`.
    UpdateCollection(CollectionDeclaration),
    /// `Vault.DeleteCollection`.
    DeleteCollection(CollectionDeletion),
    /// `Vault.BatchCollections`.
    BatchCollections(Vec<Result<CollectionOperation, Error>>),
    /// `Vault.BatchDocuments`.
    BatchDocuments(Vec<Result<VaultDocumentOperation, Error>>),
    /// `Vault.QueryDocuments`.
    QueryDocuments(VaultQuery),
    /// `Vault.CountDocuments`.
    CountDocuments(VaultQuery),
    /// `Vault.SearchDocuments`.
    SearchDocuments(VaultSearch),
    /// A call on the collection with this name.
    Collection(String, CollectionCall),
    /// A call on the mirror.
    Mirror(MirrorCall),
    /// A call on the Index.
    Index(IndexCall),
}

/// One rpc of `Collection`.
pub(crate) enum CollectionCall {
    Describe,
    Get(DocumentId),
    GetMany(DocumentSelection),
    History(DocumentId),
    Create(DocumentCreation),
    Update(DocumentUpdate),
    Upsert(DocumentUpsert),
    Delete(DocumentDeletion),
    Batch(Vec<Result<DocumentOperation, Error>>),
    Query(DocumentQuery),
    Count(DocumentQuery),
    Search(DocumentSearch),
}

/// One rpc of `Mirror`.
pub(crate) enum MirrorCall {
    Info,
    Set(MirrorSetting),
    Remove,
    Push,
    Pull,
    Sync,
}

/// One rpc of `Index`.
pub(crate) enum IndexCall {
    Status,
    Watch { until_settled: bool },
    Reindex,
}

impl Call {
    /// Whether the call answers from the Index, and so waits for it to
    /// reach the state of the Vault at open first.
    pub(crate) fn reads_index(&self) -> bool {
        matches!(
            self,
            Call::QueryDocuments(_)
                | Call::CountDocuments(_)
                | Call::SearchDocuments(_)
                | Call::Collection(
                    _,
                    CollectionCall::Query(_) | CollectionCall::Count(_) | CollectionCall::Search(_)
                )
        )
    }

    /// Makes a call that reads the Index answer once the Index reflects
    /// the state the token `token` names: the request's `notBefore`, unless
    /// it names its own.
    pub(crate) fn wait_for(&mut self, token: String) {
        let not_before = match self {
            Call::QueryDocuments(query) | Call::CountDocuments(query) => &mut query.not_before,
            Call::SearchDocuments(search) => match search.search.as_option_mut() {
                Some(search) => &mut search.not_before,
                None => return,
            },
            Call::Collection(_, CollectionCall::Query(query) | CollectionCall::Count(query)) => {
                &mut query.not_before
            }
            Call::Collection(_, CollectionCall::Search(search)) => &mut search.not_before,
            _ => return,
        };
        if not_before.is_none() {
            *not_before = Some(token);
        }
    }

    /// The call a subcommand makes, with its input read and parsed.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for an input that is missing, unreadable, or not
    /// the JSON of the request.
    pub(crate) fn prepare(command: Command) -> Result<Call, Error> {
        Ok(match command {
            Command::SchedaInfo => Call::SchedaInfo,
            Command::Info => Call::Info,
            Command::Check => Call::Check,
            Command::ListCollections => Call::ListCollections,
            Command::CreateCollection { name, schema } => {
                Call::CreateCollection(declaration(name, schema.as_deref())?)
            }
            Command::UpdateCollection { name, schema } => {
                Call::UpdateCollection(declaration(name, schema.as_deref())?)
            }
            Command::DeleteCollection {
                name,
                ignore_missing,
            } => Call::DeleteCollection(
                CollectionDeletion::default()
                    .with_name(name)
                    .with_ignore_missing(ignore_missing),
            ),
            Command::BatchCollections => Call::BatchCollections(json_lines(parse_command)),
            Command::BatchDocuments => Call::BatchDocuments(json_lines(parse_command)),
            Command::QueryDocuments { query } => Call::QueryDocuments(json_arg(
                query.as_deref(),
                "--query",
                parse_command::<VaultQuery>,
            )?),
            Command::CountDocuments { query } => Call::CountDocuments(json_arg(
                query.as_deref(),
                "--query",
                parse_command::<VaultQuery>,
            )?),
            Command::SearchDocuments { search } => Call::SearchDocuments(json_arg(
                search.as_deref(),
                "--search",
                parse_command::<VaultSearch>,
            )?),
            Command::Collection { name, command } => {
                Call::Collection(name, collection_call(command)?)
            }
            Command::Mirror { command } => Call::Mirror(match command {
                MirrorCommand::Info => MirrorCall::Info,
                MirrorCommand::Set(set) => MirrorCall::Set(
                    MirrorSetting::default()
                        .with_url(set.url)
                        .with_replace_existing(set.replace_existing),
                ),
                MirrorCommand::Remove => MirrorCall::Remove,
                MirrorCommand::Push => MirrorCall::Push,
                MirrorCommand::Pull => MirrorCall::Pull,
                MirrorCommand::Sync => MirrorCall::Sync,
            }),
            Command::Index { command } => Call::Index(match command {
                IndexCommand::Status => IndexCall::Status,
                IndexCommand::Watch { until_settled } => IndexCall::Watch { until_settled },
                IndexCommand::Reindex => IndexCall::Reindex,
            }),
        })
    }
}

/// The declaration of the collection `name` with the Schema of the JSON.
///
/// The Schema is read as plain JSON only to put it under `schema` next to
/// the name; the declaration is then parsed strictly as a whole, so an
/// unknown key is named by its full path, as the library would name it.
fn declaration(name: String, schema: Option<&str>) -> Result<CollectionDeclaration, Error> {
    let schema: serde_json::Value = json_arg(schema, "--schema", json_value)?;
    let text = serde_json::json!({"name": name, "schema": schema}).to_string();
    parse_command(text.as_bytes())
}

/// The JSON value of `json`: its syntax checked and nothing else.
fn json_value(json: &[u8]) -> Result<serde_json::Value, Error> {
    serde_json::from_slice(json).map_err(|err| {
        Error::from(
            Fault::new(
                VaultErrorCode::InvalidArgument,
                format!("the Schema is not JSON: {err}"),
            )
            .with_source(err),
        )
    })
}

fn collection_call(command: CollectionCommand) -> Result<CollectionCall, Error> {
    Ok(match command {
        CollectionCommand::Describe => CollectionCall::Describe,
        CollectionCommand::Get { id } => CollectionCall::Get(DocumentId::default().with_id(id)),
        CollectionCommand::GetMany {
            ids,
            limit,
            start_after,
            include_raw,
        } => {
            let selection = match ids {
                Some(ids) => Selection::Ids(Box::new(Ids {
                    ids,
                    ..Ids::default()
                })),
                None => Selection::Listing(Box::new(Listing {
                    limit,
                    start_after,
                    ..Listing::default()
                })),
            };
            CollectionCall::GetMany(DocumentSelection {
                include_raw: Some(include_raw),
                selection: Some(selection),
                ..DocumentSelection::default()
            })
        }
        CollectionCommand::History { id } => {
            CollectionCall::History(DocumentId::default().with_id(id))
        }
        CollectionCommand::Create {
            id,
            file,
            note,
            keep_existing,
        } => CollectionCall::Create(
            DocumentCreation::default()
                .with_id(id)
                .with_raw(raw_bytes(file.as_deref())?)
                .with_note(note)
                .with_keep_existing(keep_existing),
        ),
        CollectionCommand::Update {
            id,
            file,
            note,
            revision,
        } => {
            let update = DocumentUpdate::default()
                .with_id(id)
                .with_raw(raw_bytes(file.as_deref())?)
                .with_note(note);
            CollectionCall::Update(match revision {
                Some(revision) => update.with_revision(revision),
                None => update,
            })
        }
        CollectionCommand::Upsert { id, file, note } => CollectionCall::Upsert(
            DocumentUpsert::default()
                .with_id(id)
                .with_raw(raw_bytes(file.as_deref())?)
                .with_note(note),
        ),
        CollectionCommand::Delete {
            id,
            note,
            revision,
            ignore_missing,
        } => {
            let deletion = DocumentDeletion::default()
                .with_id(id)
                .with_note(note)
                .with_ignore_missing(ignore_missing);
            CollectionCall::Delete(match revision {
                Some(revision) => deletion.with_revision(revision),
                None => deletion,
            })
        }
        CollectionCommand::Batch => CollectionCall::Batch(json_lines(parse_command)),
        CollectionCommand::Query { query } => CollectionCall::Query(json_arg(
            query.as_deref(),
            "--query",
            parse_command::<DocumentQuery>,
        )?),
        CollectionCommand::Count { query } => CollectionCall::Count(json_arg(
            query.as_deref(),
            "--query",
            parse_command::<DocumentQuery>,
        )?),
        CollectionCommand::Search { search } => CollectionCall::Search(json_arg(
            search.as_deref(),
            "--search",
            parse_command::<DocumentSearch>,
        )?),
    })
}
