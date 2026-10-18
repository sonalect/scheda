//! Who indexes: whether a handle has every port the Index needs for a
//! round.
//!
//! Handles of one Vault may hold different ports (a service with a
//! Projector and an embedder, command-line tools with none). A handle takes
//! a round only when it has what the Index needs: a Projector, and, when a
//! collection has a `vector` column, an embedder whose model is the Index's
//! (any model while the Index has none). A handle without them never takes
//! `index.lock` for a round: it reads, its `role` is false, and its status
//! follows the Index. A Projector must also be of the projection the Index
//! was built with (any while the Index records none): a Vault with another
//! one finds the Index `INDEX_STALE` and does not index. A `Reindex` is the
//! exception to the model and to the projection: it rebuilds the Index with
//! the ports of the handle that asks, so it needs the ports only.
//!
//! A build without `find` has no Projector to ask for: this module is not
//! there, and every handle indexes the journal id maps.

use std::sync::Arc;
#[cfg(feature = "search")]
use std::sync::Mutex;

use scheda_api::v1::VaultErrorCode;

#[cfg(feature = "search")]
use crate::collection;
use crate::error::Context;
#[cfg(feature = "search")]
use crate::error::{self};
use crate::error::{Error, Fault};
use crate::index::Index;
#[cfg(feature = "search")]
use crate::index::State;
use crate::lexical;
use crate::ports::{Installed, ProjectorPort};
#[cfg(feature = "search")]
use crate::repo::Repo;
use crate::repo::Repos;

/// What a handle may do about the Index now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    /// It has what a round needs.
    Index,
    /// It lacks a port; the name says which.
    Lacks(&'static str),
    /// Its embedder is of another model than the vectors of the Index.
    #[cfg(feature = "search")]
    Mismatch,
    /// Its Projector is of another projection than the one the Index was
    /// built with.
    Stale,
}

/// `INDEX_STALE`: the Index was built by a Projector of another projection
/// than this Vault's.
pub(crate) fn stale_fault() -> Fault {
    Fault::new(
        VaultErrorCode::IndexStale,
        "the Index was built by a Projector of another projection than this Vault's; \
         Index.Reindex from this Vault rebuilds it with its own",
    )
}

impl Verdict {
    /// The failure of a call that needs the handle to index, or `None` when
    /// it may.
    pub(crate) fn precondition(self) -> Option<Fault> {
        match self {
            Verdict::Index => None,
            Verdict::Lacks(port) => Some(Fault::new(
                VaultErrorCode::FailedPrecondition,
                format!("this handle has no {port}, which the Index needs to index"),
            )),
            Verdict::Stale => Some(stale_fault()),
            #[cfg(feature = "search")]
            Verdict::Mismatch => Some(Fault::new(
                VaultErrorCode::FailedPrecondition,
                "the embedder of this handle is of another model than the vectors of the Index",
            )),
        }
    }
}

/// The ports and the Vault a handle decides from.
#[derive(Debug)]
pub(crate) struct Who {
    ports: Arc<Installed>,
    #[cfg(feature = "search")]
    repos: Arc<Repos>,
    index: Index,
    /// Whether `HEAD` has a `vector` column, with the state of `HEAD` it was
    /// found for: a handle that cannot index looks at every poll, and the
    /// answer changes only with `HEAD`.
    #[cfg(feature = "search")]
    vector_column: Mutex<Option<(State, bool)>>,
}

impl Who {
    /// The decision of the handle that holds `ports` for the Vault `repos`
    /// reads, whose Index is `index`.
    pub(crate) fn new(ports: Arc<Installed>, repos: Arc<Repos>, index: Index) -> Who {
        #[cfg(not(feature = "search"))]
        let _ = repos;
        Who {
            ports,
            #[cfg(feature = "search")]
            repos,
            index,
            #[cfg(feature = "search")]
            vector_column: Mutex::new(None),
        }
    }

    /// Whether the handle may take a round now. `asked`: the round is a
    /// rebuild some handle asked for, which takes the model of this one.
    ///
    /// # Errors
    ///
    /// The failure of the Projector when it is asked for its columns and
    /// fails; `STORAGE` and the like when the Vault cannot be read.
    pub(crate) fn check(&self, asked: bool) -> Result<Verdict, Error> {
        self.check_with(self.ports.projector().as_deref(), asked)
    }

    /// [`Who::check`] for the Projector a round would project with.
    ///
    /// # Errors
    ///
    /// As [`Who::check`]; `INDEX_BROKEN` when the columns file of the Index
    /// is damaged.
    pub(crate) fn check_with(
        &self,
        projector: Option<&ProjectorPort>,
        asked: bool,
    ) -> Result<Verdict, Error> {
        let Some(projector) = projector else {
            return Ok(Verdict::Lacks("Projector"));
        };
        if !asked && self.other_projection(projector)? {
            return Ok(Verdict::Stale);
        }
        #[cfg(feature = "search")]
        {
            match self.ports.embedder() {
                Some(embedder) => {
                    if !asked
                        && crate::vector::recorded(&self.index)
                            .context("read the model of the Index")?
                            .is_some_and(|recorded| !recorded.is(&embedder))
                    {
                        return Ok(Verdict::Mismatch);
                    }
                }
                None => {
                    if self.has_vector_column(projector)? {
                        return Ok(Verdict::Lacks("embedder"));
                    }
                }
            }
        }
        #[cfg(not(feature = "search"))]
        let _ = (projector, asked);
        Ok(Verdict::Index)
    }

    /// Whether the Index was built by a Projector of another projection than
    /// `projector`. False while the Index records none.
    fn other_projection(&self, projector: &ProjectorPort) -> Result<bool, Error> {
        let dump = lexical::read_dump(&self.index).context("read the projection of the Index")?;
        Ok(dump
            .as_ref()
            .and_then(lexical::Dump::projection)
            .is_some_and(|recorded| recorded != projector.projection_id()))
    }

    /// Whether the Projector of this Vault is of another projection than
    /// the Index. False without a Projector, and when the Index cannot be
    /// read (the round says so).
    pub(crate) fn stale(&self) -> bool {
        self.ports
            .projector()
            .is_some_and(|projector| matches!(self.other_projection(&projector), Ok(true)))
    }

    /// Whether the embedder of this handle is of another model than the
    /// vectors of the Index. False when it has no embedder, and when the
    /// vectors cannot be read (the reader says so).
    pub(crate) fn mismatch(&self) -> bool {
        #[cfg(feature = "search")]
        {
            let Some(embedder) = self.ports.embedder() else {
                return false;
            };
            matches!(
                crate::vector::recorded(&self.index),
                Ok(Some(recorded)) if !recorded.is(&embedder)
            )
        }
        #[cfg(not(feature = "search"))]
        {
            false
        }
    }

    /// Whether a collection of `HEAD` that the vector side takes has a
    /// column its Projector marks `vector`. Asked of the Projector once per
    /// state of `HEAD`.
    #[cfg(feature = "search")]
    fn has_vector_column(&self, projector: &crate::ports::ProjectorPort) -> Result<bool, Error> {
        let views = self.repos.views().context("read HEAD")?;
        let head = State::of(&views);
        let mut known = error::mutex(&self.vector_column, "look at the columns")?;
        if let Some((state, found)) = *known
            && state == head
        {
            return Ok(found);
        }
        let declarations =
            collection::load(views.of(Repo::Main)).context("read the collections")?;
        let mut found = false;
        for decl in declarations.iter().filter(|decl| decl.vector_indexed()) {
            let schema = projector
                .columns(decl.name().to_owned())
                .context("ask for the columns")?;
            if schema.is_some_and(|schema| {
                schema
                    .columns
                    .iter()
                    .any(|column| column.vector.unwrap_or(false))
            }) {
                found = true;
                break;
            }
        }
        *known = Some((head, found));
        Ok(found)
    }
}
