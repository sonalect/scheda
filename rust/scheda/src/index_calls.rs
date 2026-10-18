//! Index calls of the `Vault` handle: ask for a rebuild, and read where the
//! Index stands.

use buffa_types::google::protobuf::Empty;
#[cfg(feature = "find")]
use protocontract::BoxIter;
use scheda_api::v1::IndexState;
#[cfg(not(feature = "find"))]
use scheda_api::v1::VaultErrorCode;

#[cfg(not(feature = "find"))]
use crate::error::Fault;
use crate::error::{Context, Error};

/// The failure of a status call in a build without the Index.
#[cfg(not(feature = "find"))]
fn no_status() -> Error {
    Fault::new(
        VaultErrorCode::Unimplemented,
        "the status of the Index needs a build with the `find` feature",
    )
    .into()
}

impl crate::Vault {
    /// Starts a rebuild of the Index from the stored Documents and returns
    /// at once; `watch_index_status` shows its progress and its result. A
    /// rebuild already asked for or running is `ABORTED`. In a build without
    /// `find`, it rebuilds the journal id maps only.
    ///
    /// The rebuild is a round of this handle's indexer from an empty Index.
    /// It waits for the Index lock like any round: while another indexer
    /// holds it the rebuild is asked for and has not begun, and a second
    /// `reindex` is `ABORTED`. Until the rebuild ends the Index has no
    /// `version`, and a journal line is found by a scan.
    ///
    /// A handle rebuilds the Index only when it has every port the Index
    /// needs: a Projector, and an embedder when a collection has a `vector`
    /// column. A rebuild from an embedder of another model than the vectors
    /// of the Index makes the Index again with that model.
    ///
    /// # Errors
    ///
    /// `FAILED_PRECONDITION` naming the port this handle lacks; `ABORTED` as
    /// above.
    pub fn reindex(&self, _empty: Empty) -> Result<Empty, Error> {
        self.indexer().reindex().context("reindex")?;
        Ok(Empty::default())
    }

    /// Where the Index stands now. A build without `find`, `UNIMPLEMENTED`.
    ///
    /// # Errors
    ///
    /// `UNIMPLEMENTED` in a build without the `find` feature.
    pub fn get_index_status(&self, _empty: Empty) -> Result<IndexState, Error> {
        #[cfg(feature = "find")]
        {
            Ok(self.indexer().status())
        }
        #[cfg(not(feature = "find"))]
        {
            Err(no_status())
        }
    }

    /// Where the Index stands, now and at every change: the first item at
    /// once, then one item for each indexer round, for the Index catching up
    /// with the last write, and for a rebuild starting, moving on, and
    /// ending. The stream does not end by itself; the caller drops it when
    /// it has what it waited for. A build without `find`, `UNIMPLEMENTED`.
    ///
    /// The stream holds no lock while it waits for the next item, and it
    /// ends when the handle is closed. A reader that falls more than 256
    /// items behind skips to the oldest it can still give. Progress items
    /// of a rebuild come at most ten times a second.
    ///
    /// # Errors
    ///
    /// `UNIMPLEMENTED` in a build without the `find` feature.
    #[cfg(feature = "find")]
    pub fn watch_index_status(
        &self,
        _empty: Empty,
    ) -> Result<BoxIter<'static, Result<IndexState, Error>>, Error> {
        Ok(Box::new(self.indexer().watch()))
    }

    /// Where the Index stands, now and at every change. A build without
    /// `find`, `UNIMPLEMENTED`.
    ///
    /// # Errors
    ///
    /// `UNIMPLEMENTED` in a build without the `find` feature.
    #[cfg(not(feature = "find"))]
    pub fn watch_index_status(
        &self,
        _empty: Empty,
    ) -> Result<protocontract::BoxIter<'static, Result<IndexState, Error>>, Error> {
        Err(no_status())
    }
}
