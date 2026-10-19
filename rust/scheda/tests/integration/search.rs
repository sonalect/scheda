//! The searches of the contract: `Collection.Search` and
//! `Vault.SearchDocuments`. This version of Scheda has no Index to rank
//! from, so both answer `UNIMPLEMENTED`.

use scheda::Error;
use scheda::v1::{CollectionAsync, DocumentSearch, VaultAsync, VaultErrorCode, VaultSearch};

use crate::common::{Scratch, both, collection, open, snapshot, vault as built_vault};

fn search() -> DocumentSearch {
    serde_json::from_value(serde_json::json!({"text": "words"})).unwrap()
}

fn is_unimplemented(error: &Error) -> bool {
    error.is(VaultErrorCode::Unimplemented)
}

#[test]
fn the_two_calls_are_unimplemented() {
    both(|| async {
        let scratch = Scratch::new("search-unimplemented");
        let path = scratch.join("vault");
        built_vault(&path);
        let vault = open(&path, Default::default()).await.unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        let before = snapshot(&path);

        assert!(is_unimplemented(
            &notes.search(search()).await.err().unwrap()
        ));
        let request = VaultSearch {
            search: search().into(),
            ..VaultSearch::default()
        };
        assert!(is_unimplemented(
            &vault.search_documents(request).await.err().unwrap()
        ));
        assert_eq!(snapshot(&path), before, "a refused search writes nothing");
    });
}
