//! The searches of the contract: `Collection.Search` and
//! `Vault.SearchDocuments`. This version of Scheda has no Index to rank
//! from, so both answer `UNIMPLEMENTED`.

mod common;

use scheda::Error;
use scheda::v1::{CollectionKey, DocumentSearch, VaultErrorCode, VaultSearch};

use common::{Scratch, open, snapshot, vault as built_vault};

fn search() -> DocumentSearch {
    serde_json::from_value(serde_json::json!({"text": "words"})).unwrap()
}

fn is_unimplemented(error: &Error) -> bool {
    error.is(VaultErrorCode::Unimplemented)
}

#[test]
fn the_two_calls_are_unimplemented() {
    let scratch = Scratch::new("search-unimplemented");
    let path = scratch.join("vault");
    built_vault(&path);
    let vault = open(&path, Default::default()).unwrap();
    let notes = vault
        .get_collection(CollectionKey::default().with_name("notes"))
        .unwrap()
        .to_sync()
        .unwrap();
    let before = snapshot(&path);

    assert!(is_unimplemented(&notes.search(search()).err().unwrap()));
    let request = VaultSearch {
        search: search().into(),
        ..VaultSearch::default()
    };
    assert!(is_unimplemented(
        &vault.search_documents(request).err().unwrap()
    ));
    assert_eq!(snapshot(&path), before, "a refused search writes nothing");
}
