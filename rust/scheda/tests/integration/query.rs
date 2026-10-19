//! The queries of the contract: `Collection.Query` and `Collection.Count`,
//! `Vault.QueryDocuments` and `Vault.CountDocuments`. This version of Scheda
//! has no Index to answer them from, so all four answer `UNIMPLEMENTED`,
//! and so does `Vault.Index`.

use buffa_types::google::protobuf::Empty;
use scheda::Error;
use scheda::v1::{CollectionDeclaration, DocumentQuery, VaultErrorCode, VaultQuery};

use scheda::v1::{CollectionAsync, VaultAsync};

use crate::common::{Scratch, both, collection, open, snapshot, vault as built_vault};

fn query() -> DocumentQuery {
    serde_json::from_value(serde_json::json!({"filter": {"all": {}}})).unwrap()
}

fn vault_query() -> VaultQuery {
    serde_json::from_value(serde_json::json!({"filter": {"all": {}}})).unwrap()
}

fn is_unimplemented(error: &Error) -> bool {
    error.is(VaultErrorCode::Unimplemented)
}

#[test]
fn the_four_calls_and_the_index_object_are_unimplemented() {
    both(|| async {
        let scratch = Scratch::new("query-unimplemented");
        let path = scratch.join("vault");
        built_vault(&path);
        let vault = open(&path, Default::default()).await.unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        let before = snapshot(&path);

        assert!(is_unimplemented(&notes.query(query()).await.err().unwrap()));
        assert!(is_unimplemented(&notes.count(query()).await.unwrap_err()));
        assert!(is_unimplemented(
            &vault.query_documents(vault_query()).await.err().unwrap()
        ));
        assert!(is_unimplemented(
            &vault.count_documents(vault_query()).await.unwrap_err()
        ));
        assert!(is_unimplemented(
            &vault.index(Empty::default()).await.err().unwrap()
        ));
        assert_eq!(snapshot(&path), before, "a refused query writes nothing");

        // A collection declared meanwhile is refused the same way.
        let declaration: CollectionDeclaration = serde_json::from_value(
            serde_json::json!({"name": "later", "schema": {"standard": {}}}),
        )
        .unwrap();
        vault.create_collection(declaration).await.unwrap();
        let later = collection(&vault, "later").await.unwrap();
        assert!(is_unimplemented(&later.count(query()).await.unwrap_err()));
    });
}
