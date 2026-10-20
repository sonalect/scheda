//! The generations of the Index on Lance: the folder of one build of the
//! Index, and the pointer to the one queries read.
//!
//! ```text
//! .scheda/index/current              the name of the live generation
//! .scheda/index/g-{…}/commit         what the generation holds, written last
//! .scheda/index/g-{…}/t{n}.lance     the tables of its collections
//! ```
//!
//! A generation is built from nothing (the first build, `Index.Reindex`)
//! beside the live one, which keeps answering, and becomes the live one
//! when `current` is written to name it, under `index.lock`, in one atomic
//! write. The generation it replaced is removed an hour later: no read of
//! the Index lasts that long. A generation that no `current` names and that
//! is not waiting to be removed is a build that died, and goes at the next
//! round.
//!
//! `commit` holds the state of the Vault the generation reflects, the ports
//! it was built with (the projection, the embedder's model and vector
//! length, the columns of each collection), and, for each collection, its
//! table and the Lance versions a reader opens. It is one file, written
//! whole and last by a round, so a reader that reads it once sees one state
//! of every collection.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use scheda_api::v1::VaultErrorCode;
use scheda_blocking::Blocking;
use serde::{Deserialize, Serialize};

use crate::error::{Context, Error, Fault, io};
use crate::fs::{atomic_write_private, create_dir_private, restrict_dir};
use crate::index::State;
use crate::store::{Shape, VERSION_LIFETIME, Versions};

const CURRENT_FILE: &str = "current";
const COMMIT_FILE: &str = "commit";
const PREFIX: &str = "g-";

/// Numbers the generations this process makes.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// The ports an Index is built with, as far as they shape it.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Ports {
    /// The projection of the Projector; `None` without one.
    pub(crate) projection: Option<String>,
    /// The embedder's model and vector length, when a collection has a
    /// `vector` column.
    pub(crate) embedder: Option<Embedder>,
}

/// An embedder as an Index records it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Embedder {
    pub(crate) model: String,
    pub(crate) dimensions: u32,
}

/// One collection's table in a generation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Table {
    /// The folder name of its datasets (`t{n}`).
    pub(crate) name: String,
    /// Its columns.
    pub(crate) shape: Shape,
    /// The versions a reader opens.
    pub(crate) versions: Versions,
}

/// A generation that was replaced, and when.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Retired {
    pub(crate) name: String,
    /// When it was replaced, in milliseconds since the epoch.
    pub(crate) at: u64,
}

/// What a generation holds: its `commit`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Commit {
    /// The state of the Vault, as `State::line`.
    pub(crate) state: String,
    /// The ports it was built with.
    pub(crate) ports: Ports,
    /// The table of each collection it holds.
    pub(crate) tables: BTreeMap<String, Table>,
    /// The number of the next table made.
    pub(crate) next_table: u64,
    /// The rounds that wrote this generation.
    pub(crate) rounds: u64,
    /// The generation this one replaced, until it is removed.
    pub(crate) retired: Option<Retired>,
    /// The tables whose collection went, until their folders are removed,
    /// an hour later: a reader of an older `commit` may still read them.
    pub(crate) dropped: Vec<Retired>,
}

impl Commit {
    /// The commit of a generation not built yet, for `ports`.
    pub(crate) fn empty(ports: Ports) -> Commit {
        Commit {
            state: State::default().line(),
            ports,
            tables: BTreeMap::new(),
            next_table: 0,
            rounds: 0,
            retired: None,
            dropped: Vec::new(),
        }
    }

    /// The state the generation reflects.
    pub(crate) fn state(&self) -> Option<State> {
        State::parse(&self.state)
    }

    /// The folder name of a new table, and the count moved on.
    pub(crate) fn new_table(&mut self) -> String {
        let name = format!("t{}", self.next_table);
        self.next_table += 1;
        name
    }
}

/// The milliseconds since the epoch, now.
pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The generations of the Index of one Vault.
#[derive(Clone, Debug)]
pub(crate) struct Layout {
    /// `{vault}/.scheda/index`.
    root: PathBuf,
}

impl Layout {
    /// The generations under the Index folder `root`.
    pub(crate) fn new(root: &Path) -> Layout {
        Layout {
            root: root.to_path_buf(),
        }
    }

    /// The live generation, if one was ever switched to.
    ///
    /// # Errors
    ///
    /// `STORAGE` when `current` cannot be read.
    pub(crate) fn current(&self, _cx: &Blocking) -> Result<Option<Generation>, Error> {
        match fs::read_to_string(self.root.join(CURRENT_FILE)) {
            Ok(text) => {
                let name = text.trim();
                Ok(valid_name(name).then(|| self.generation(name)))
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidData
                ) =>
            {
                Ok(None)
            }
            Err(err) => Err(io(err)).context("read the live generation of the Index"),
        }
    }

    /// Makes `generation` the live one, in one atomic write. Under
    /// `index.lock`.
    ///
    /// # Errors
    ///
    /// `STORAGE` when `current` cannot be written.
    pub(crate) fn switch(&self, _cx: &Blocking, generation: &Generation) -> Result<(), Error> {
        atomic_write_private(
            &self.root.join(CURRENT_FILE),
            format!("{}\n", generation.name).as_bytes(),
        )
        .context("switch the Index to a new generation")
    }

    /// A new, empty generation folder with a name no other has. Under
    /// `index.lock`.
    ///
    /// # Errors
    ///
    /// `STORAGE` when the folder cannot be made.
    pub(crate) fn create(&self, _cx: &Blocking) -> Result<Generation, Error> {
        let name = format!(
            "{PREFIX}{:x}-{:x}-{:x}",
            now_millis(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let generation = self.generation(&name);
        create_dir_private(&self.root).context("create the Index folder")?;
        create_dir_private(&generation.dir).context("create a generation of the Index")?;
        restrict_dir(&generation.dir)?;
        Ok(generation)
    }

    /// The generation called `name`.
    pub(crate) fn generation(&self, name: &str) -> Generation {
        Generation {
            dir: self.root.join(name),
            name: name.to_owned(),
        }
    }

    /// Removes the generations that neither `live` nor the generation it
    /// retired less than an hour ago name: builds that died, and retired
    /// generations whose hour is over. Under `index.lock`. A folder that
    /// cannot be removed now (a reader on Windows holds a file open) is
    /// tried again at the next round.
    ///
    /// # Errors
    ///
    /// `STORAGE` when the Index folder cannot be listed.
    pub(crate) fn sweep(
        &self,
        _cx: &Blocking,
        live: Option<&str>,
        retired: Option<&Retired>,
    ) -> Result<(), Error> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(io(err)).context("list the generations of the Index"),
        };
        let lifetime = u64::try_from(VERSION_LIFETIME.as_millis()).unwrap_or(u64::MAX);
        let keep_retired = retired
            .filter(|retired| now_millis().saturating_sub(retired.at) < lifetime)
            .map(|retired| retired.name.as_str());
        for entry in entries {
            let entry = entry
                .map_err(io)
                .context("list the generations of the Index")?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !valid_name(name) || Some(name) == live || Some(name) == keep_retired {
                continue;
            }
            let _ = fs::remove_dir_all(entry.path());
        }
        Ok(())
    }
}

/// Whether `name` is the name of a generation.
fn valid_name(name: &str) -> bool {
    name.strip_prefix(PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    })
}

/// One generation of the Index.
#[derive(Clone, Debug)]
pub(crate) struct Generation {
    dir: PathBuf,
    name: String,
}

impl Generation {
    /// Its folder.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Its name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// What it holds, or `None` before its first round ended.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` for a `commit` that is not one; `STORAGE` when it
    /// cannot be read.
    pub(crate) fn read_commit(&self, _cx: &Blocking) -> Result<Option<Commit>, Error> {
        match fs::read(self.dir.join(COMMIT_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|err| {
                Error::from(
                    Fault::new(
                        VaultErrorCode::IndexBroken,
                        "the commit of the Index is not valid",
                    )
                    .with_source(err),
                )
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(io(err)).context("read the commit of the Index"),
        }
    }

    /// Removes the folders of the tables `commit` dropped more than an hour
    /// ago, and forgets them. Under `index.lock`; `commit` is written after.
    pub(crate) fn sweep_tables(&self, _cx: &Blocking, commit: &mut Commit) {
        let lifetime = u64::try_from(VERSION_LIFETIME.as_millis()).unwrap_or(u64::MAX);
        let now = now_millis();
        commit.dropped.retain(|table| {
            if now.saturating_sub(table.at) < lifetime {
                return true;
            }
            for suffix in [".lance", ".files.lance"] {
                let _ = fs::remove_dir_all(self.dir.join(format!("{}{suffix}", table.name)));
            }
            false
        });
    }

    /// Records `commit`, the last write of a round.
    ///
    /// # Errors
    ///
    /// `STORAGE` when it cannot be written.
    pub(crate) fn write_commit(&self, _cx: &Blocking, commit: &Commit) -> Result<(), Error> {
        let bytes = serde_json::to_vec(commit).map_err(|err| {
            Error::from(
                Fault::new(
                    VaultErrorCode::Storage,
                    "cannot encode the commit of the Index",
                )
                .with_source(err),
            )
        })?;
        atomic_write_private(&self.dir.join(COMMIT_FILE), &bytes)
            .context("write the commit of the Index")
    }
}

#[cfg(test)]
mod tests {
    use super::valid_name;

    #[test]
    fn only_names_of_generations_are_generations() {
        assert!(valid_name("g-18f3a-1c2-0"));
        assert!(!valid_name("g-"));
        assert!(!valid_name("journal"));
        assert!(!valid_name("g-../x"));
        assert!(!valid_name("current"));
    }
}
