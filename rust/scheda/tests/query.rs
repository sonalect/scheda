//! The queries of the contract: `Collection.Query` and `Collection.Count`,
//! `Vault.QueryDocuments` and `Vault.CountDocuments`. This version of Scheda
//! has no Index to answer them from, so all four answer `UNIMPLEMENTED`,
//! and so does `Vault.Index`.

mod common;

use buffa_types::google::protobuf::Empty;
use scheda::Error;
use scheda::v1::{CollectionDeclaration, CollectionKey, DocumentQuery, VaultErrorCode, VaultQuery};

use common::{Scratch, open, snapshot, vault as built_vault};

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
    let scratch = Scratch::new("query-unimplemented");
    let path = scratch.join("vault");
    built_vault(&path);
    let vault = open(&path, Default::default()).unwrap();
    let notes = vault
        .get_collection(CollectionKey::default().with_name("notes"))
        .unwrap()
        .to_sync()
        .unwrap();
    let before = snapshot(&path);

    assert!(is_unimplemented(&notes.query(query()).err().unwrap()));
    assert!(is_unimplemented(&notes.count(query()).unwrap_err()));
    assert!(is_unimplemented(
        &vault.query_documents(vault_query()).err().unwrap()
    ));
    assert!(is_unimplemented(
        &vault.count_documents(vault_query()).unwrap_err()
    ));
    assert!(is_unimplemented(
        &vault.index(Empty::default()).err().unwrap()
    ));
    assert_eq!(snapshot(&path), before, "a refused query writes nothing");

    // A collection declared meanwhile is refused the same way.
    let declaration: CollectionDeclaration =
        serde_json::from_value(serde_json::json!({"name": "later", "schema": {"standard": {}}}))
            .unwrap();
    vault.create_collection(declaration).unwrap();
    let later = vault
        .get_collection(CollectionKey::default().with_name("later"))
        .unwrap()
        .to_sync()
        .unwrap();
    assert!(is_unimplemented(&later.count(query()).unwrap_err()));
}
