//! The path budget of a Vault without a work tree: with the longest names
//! the rules allow, no file or folder under the Vault is more than 120
//! characters from its root, so the Vault fits where an absolute path may
//! hold 260 characters in all.

#[cfg(feature = "index")]
mod on_lance {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use buffa_types::google::protobuf::Empty;
    use protocontract::BoxIter;
    use scheda::v1::document_operation::Operation;
    use scheda::v1::embedder_ref::EmbedderRef;
    use scheda::v1::projector_ref::ProjectorRef;
    use scheda::v1::{
        CollectionAsync, CollectionDeclaration, CollectionKey, ColumnKind, DocumentCreation,
        DocumentOperation, DocumentUpsert, DynVaultAsync, EmbedderDescriptor, EmbedderSync,
        IndexAsync, IndexColumn, IndexDoc, IndexSchema, IndexValue, MirrorAsync, MirrorSetting,
        ProjectorDescriptor, ProjectorSync, Query, RawDocument, SchedaAsync, Text, VaultAsync,
        Vector, WorkTree, index_value,
    };
    use scheda::{Error, Scheda};
    use serde_json::json;

    use crate::common::{Scratch, file_url, git, opening, stream_of};

    /// The most characters a path below the root of the Vault may have.
    const BUDGET: usize = 120;

    /// Documents in the table of the standard collection: enough for the
    /// vector index of Lance, which starts at 10 000 rows.
    const DOCUMENTS: usize = 10_050;

    const DIMENSIONS: usize = 16;

    /// A name of 255 bytes that starts with `first`.
    fn long(first: &str) -> String {
        format!("{first}{}", "x".repeat(255 - first.len()))
    }

    /// A vector that differs from the vector of every other number.
    fn vector_of(n: u64) -> Vec<f32> {
        let mut state = n.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..DIMENSIONS)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % 1000) as f32 / 1000.0
            })
            .collect()
    }

    /// The columns of 255 characters: `fts`, a scalar index, and a vector.
    struct Columns {
        fts: String,
        scalar: String,
        vector: String,
    }

    impl Columns {
        fn new() -> Columns {
            Columns {
                fts: long("title"),
                scalar: long("kind"),
                vector: long("topic"),
            }
        }
    }

    /// A Projector of Documents `title|kind|topic`.
    struct Wide(Columns);

    fn column(name: &str) -> IndexColumn {
        IndexColumn::default()
            .with_name(name)
            .with_kind(ColumnKind::COLUMN_KIND_TEXT)
    }

    impl ProjectorSync for Wide {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("wide@1"))
        }

        fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema {
                columns: vec![
                    column(&self.0.fts).with_fts(true),
                    column(&self.0.scalar).with_scalar_index(true),
                    column(&self.0.vector).with_vector(true),
                ],
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let text = String::from_utf8(document.raw.unwrap_or_default()).unwrap_or_default();
            let mut parts = text.splitn(3, '|');
            let mut doc = IndexDoc::default();
            for name in [&self.0.fts, &self.0.scalar, &self.0.vector] {
                if let Some(part) = parts.next() {
                    doc.values.insert(
                        name.clone(),
                        IndexValue {
                            value: Some(index_value::Value::Text(part.to_owned())),
                            ..IndexValue::default()
                        },
                    );
                }
            }
            Ok(doc)
        }
    }

    /// An embedder that gives each text a vector of its own number.
    struct Numbered;

    fn number_of(text: &str) -> u64 {
        text.rsplit(' ')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }

    impl EmbedderSync for Numbered {
        fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
            Ok(EmbedderDescriptor::default()
                .with_model_id("numbered@1")
                .with_dimensions(DIMENSIONS as u32))
        }

        fn embed(
            &self,
            texts: BoxIter<'static, Result<Text, Error>>,
        ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
            Ok(Box::new(texts.map(|text| {
                Ok(Vector {
                    values: vector_of(number_of(&text?.text.unwrap_or_default())),
                    ..Vector::default()
                })
            })))
        }

        fn embed_query(&self, query: Query) -> Result<Vector, Error> {
            Ok(Vector {
                values: vector_of(number_of(&query.text.unwrap_or_default())),
                ..Vector::default()
            })
        }
    }

    fn declaration(name: &str, schema: serde_json::Value) -> CollectionDeclaration {
        serde_json::from_value(json!({"name": name, "schema": schema})).unwrap()
    }

    /// The longest path, relative to `root`, of everything under it, and
    /// whether a vector index (`auxiliary.idx`) is among them.
    fn longest(root: &Path) -> (usize, String, bool) {
        let mut best = (0, String::new());
        let mut vector_index = false;
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                vector_index |= rel.ends_with("/auxiliary.idx");
                if rel.chars().count() > best.0 {
                    best = (rel.chars().count(), rel);
                }
                if path.is_dir() && !path.is_symlink() {
                    pending.push(path);
                }
            }
        }
        (best.0, best.1, vector_index)
    }

    /// Waits until the Index of `vault` reflects the stored state now.
    async fn wait_indexed(vault: &DynVaultAsync) {
        let index = vault
            .index(Empty::default())
            .await
            .unwrap()
            .to_async_inline()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let head = vault.info(Empty::default()).await.unwrap().version;
            let state = index.status(Empty::default()).await.unwrap();
            if state.version.is_some() && state.version == head {
                return;
            }
            assert!(Instant::now() < deadline, "not indexed: {state:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn no_path_under_a_vault_without_a_work_tree_is_longer_than_120_characters() {
        // The paths do not depend on the runtime: one run is enough.
        crate::common::once_on_multi_thread(|| async {
            let scratch = Scratch::new("budget");
            let mirror = scratch.join("mirror.git");
            std::fs::create_dir_all(&mirror).unwrap();
            git(
                &mirror,
                &["init", "--quiet", "--bare", "--initial-branch=main"],
            );
            let path = scratch.join("vault");
            let mut request = opening(
                &path,
                scheda::v1::OpenOptions::default()
                    .with_work_tree(WorkTree::WORK_TREE_NONE)
                    .with_initial_branch("main")
                    .with_index_poll_ms(10)
                    .with_write_timeout_ms(120_000),
            );
            request.projector = ProjectorRef::from_sync(Wide(Columns::new())).into();
            request.embedder = EmbedderRef::from_sync(Numbered).into();
            let vault = Scheda
                .open(request)
                .await
                .unwrap()
                .to_async_inline()
                .unwrap();

            let (notes, log, diary) = (long("n"), long("l"), long("d"));
            vault
                .create_collection(declaration(&notes, json!({"standard": {}})))
                .await
                .unwrap();
            vault
                .create_collection(declaration(&log, json!({"journal": {}})))
                .await
                .unwrap();
            vault
                .create_collection(declaration(
                    &diary,
                    json!({"standard": {}, "isolated": true}),
                ))
                .await
                .unwrap();
            let collection = |name: &str| {
                let vault = vault.clone();
                let name = name.to_owned();
                async move {
                    vault
                        .get_collection(CollectionKey::default().with_name(name))
                        .await
                        .unwrap()
                        .to_async_inline()
                        .unwrap()
                }
            };
            let upsert = |id: String, raw: String| DocumentOperation {
                operation: Some(Operation::Upsert(Box::new(
                    DocumentUpsert::default()
                        .with_id(id)
                        .with_raw(raw.into_bytes())
                        .with_note("budget"),
                ))),
                ..DocumentOperation::default()
            };
            let body = |n: usize| format!("word{} common|k{}|topic {n}", n % 100, n % 7);
            collection(&notes)
                .await
                .batch(stream_of(
                    (0..DOCUMENTS)
                        .map(|n| upsert(format!("{n:06}"), body(n)))
                        .collect(),
                ))
                .await
                .unwrap();
            collection(&diary)
                .await
                .batch(stream_of(
                    (0..20)
                        .map(|n| upsert(format!("{n:06}"), body(n)))
                        .collect(),
                ))
                .await
                .unwrap();
            let journal = collection(&log).await;
            for n in 0..20 {
                journal
                    .create(
                        DocumentCreation::default()
                            .with_id(format!("l{n:03}"))
                            .with_raw(body(n).into_bytes())
                            .with_note("budget"),
                    )
                    .await
                    .unwrap();
            }
            wait_indexed(&vault).await;

            let mirror_handle = vault
                .mirror(Empty::default())
                .await
                .unwrap()
                .to_async_inline()
                .unwrap();
            mirror_handle
                .set(MirrorSetting::default().with_url(file_url(&mirror)))
                .await
                .unwrap();
            mirror_handle.sync(Empty::default()).await.unwrap();
            wait_indexed(&vault).await;
            vault.close(Empty::default()).await.unwrap();

            let (length, longest, vector_index) = longest(&path);
            assert!(vector_index, "the table has its vector index");
            assert!(
                length <= BUDGET,
                "the longest path is {length} characters, over {BUDGET}: {longest}"
            );
            eprintln!("the longest path is {length} characters: {longest}");
        });
    }
}
