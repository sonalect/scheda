//! The generations of the Index on Lance: the folder of one build of the
//! Index, and the pointer to the one queries read.
//!
//! ```text
//! .scheda/index/current              the name of the live generation
//! .scheda/index/g{n}/commit          what the generation holds, written last
//! .scheda/index/g{n}/t{n}{kind}      the datasets of the tables of its collections
//! ```
//!
//! The names are short because the whole path must stay within a budget,
//! with Lance's own `_indices/{uuid}/…` below them. `n` of a generation is
//! one more than the largest number of the generations on disk. A table
//! has the number its collection was given (`t{n}`), and its datasets the
//! kinds `d` (the Documents), `f` (the files), and `x{k}` (the texts of
//! the `text list` vector column numbered `k` in that table). A number is
//! given once and not reused within a generation.
//!
//! A generation is built from nothing (the first build, `Index.Reindex`)
//! beside the live one, which keeps answering, and becomes the live one
//! when `current` is written to name it, under `index.lock`, in one atomic
//! write. The generation it replaced is removed an hour after it stopped
//! being the live one (see **Removal**). A generation that no `current`
//! names and that is not waiting to be removed is a build that died, and
//! goes at the next round.
//!
//! `commit` holds the state of the Vault the generation reflects, the ports
//! it was built with (the projection, the embedder's model and vector
//! length, the columns of each collection), and, for each collection, its
//! table and the Lance versions a reader opens. It is one file, written
//! whole and last by a round, so a reader that reads it once sees one state
//! of every collection.
//!
//! **Removal.** What a `commit` stops naming (a Lance version of a table, a
//! dropped table or texts dataset, the generation a rebuild retired) stays
//! on disk for [`VERSION_LIFETIME`] after it stopped: a reader that read the
//! `commit` naming it may read it until then, however long the round that
//! replaced it took. The moment it stopped is stamped by the next round,
//! when it starts: a round starts under `index.lock`, after the round
//! before it wrote its `commit`, so the stamp is never earlier than the
//! moment no new reader could open what was replaced. A reader is thus
//! safe for one hour from the moment before it read `commit`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use scheda_api::v1::VaultErrorCode;
use scheda_blocking::Blocking;
use serde::{Deserialize, Serialize};

use crate::error::{Context, Error, Fault, io};
use crate::fs::{atomic_write_private, create_dir_private, restrict_dir};
use crate::index::{State, generation_number};
use crate::store::{Column, Shape, VERSION_LIFETIME, Versions};

const CURRENT_FILE: &str = "current";
const COMMIT_FILE: &str = "commit";
const PREFIX: &str = "g";

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

/// What the Projector declares for one collection, before the collection's
/// `index_scope` is applied: the part of an Index's freshness that belongs
/// to the Projector, not to the declaration's scope. The vector length is not here: the Index's embedder
/// fixes it.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Declared {
    /// All the columns it gives.
    pub(crate) columns: Vec<Column>,
    /// The names of its `vector` columns.
    pub(crate) vectors: Vec<String>,
}

/// One collection's table in a generation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Table {
    /// The folder name of its datasets (`t{n}`).
    pub(crate) name: String,
    /// The number of the texts dataset of each `text list` vector column
    /// the table has: `t{n}x{k}`.
    #[serde(default)]
    pub(crate) texts: BTreeMap<String, u32>,
    /// The number the next texts dataset of the table gets.
    #[serde(default)]
    pub(crate) next_text: u32,
    /// Its columns as the table holds them: the declaration's `index_scope`
    /// applied.
    pub(crate) shape: Shape,
    /// What the Projector declared for the collection when the table was
    /// built, before any scope.
    pub(crate) declared: Declared,
    /// The versions a reader opens.
    pub(crate) versions: Versions,
    /// The versions earlier `commit`s named that a reader may still read,
    /// oldest first: what a round keeps when it prunes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) replaced: Vec<Replaced>,
}

impl Table {
    /// The oldest version of each dataset of the table a reader may still
    /// open at `now`: the one this `commit` names, or one an earlier
    /// `commit` named that it stopped naming less than [`VERSION_LIFETIME`]
    /// ago, or whose moment is not stamped yet. A round prunes the versions
    /// before these.
    pub(crate) fn keep_from(&self, now: u64) -> Versions {
        let mut keep = self.versions.clone();
        for replaced in self.replaced.iter().filter(|replaced| replaced.held(now)) {
            keep.rows = keep.rows.min(replaced.versions.rows);
            keep.files = keep.files.min(replaced.versions.files);
            for (column, version) in &replaced.versions.texts {
                if let Some(kept) = keep.texts.get_mut(column) {
                    *kept = (*kept).min(*version);
                }
            }
        }
        keep
    }
}

/// The versions an earlier `commit` named for a table, and when it stopped
/// naming them.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Replaced {
    pub(crate) versions: Versions,
    /// When they stopped being named, in milliseconds since the epoch, as
    /// the round after the one that replaced them stamped it; `None` until
    /// then.
    pub(crate) since: Option<u64>,
}

impl Replaced {
    /// Whether a reader may still read these versions at `now`.
    fn held(&self, now: u64) -> bool {
        self.since.is_none_or(|since| !expired(since, now))
    }
}

/// A generation, or a table, that was replaced, and when.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Retired {
    /// The name of a generation; of a table (`t{n}`); or of the texts
    /// dataset of one column of a table, which the table no longer has.
    pub(crate) name: String,
    /// When the live `commit` stopped naming it, in milliseconds since the
    /// epoch, as the round after the one that replaced it stamped it; until
    /// then the moment the round that replaced it did so, which an earlier
    /// Scheda, reading this `commit`, takes for the stamp.
    pub(crate) at: u64,
    /// Whether `at` is the stamp. Until it is, nothing is removed. An entry
    /// of an earlier Scheda has no such field: its `at` is its stamp.
    #[serde(default = "stamped_before")]
    pub(crate) stamped: bool,
}

/// What `Retired.stamped` is in a `commit` of an earlier Scheda.
fn stamped_before() -> bool {
    true
}

impl Retired {
    /// One replaced by the round that writes the next `commit`: its moment
    /// is stamped by the round after.
    pub(crate) fn now(name: String) -> Retired {
        Retired {
            name,
            at: now_millis(),
            stamped: false,
        }
    }

    /// Whether its hour is over at `now`.
    pub(crate) fn due(&self, now: u64) -> bool {
        self.stamped && expired(self.at, now)
    }

    /// Stamps it at `now`, unless it is.
    fn stamp(&mut self, now: u64) {
        if !self.stamped {
            self.at = now;
            self.stamped = true;
        }
    }
}

/// Whether what stopped being named at `since` may be removed at `now`.
fn expired(since: u64, now: u64) -> bool {
    let lifetime = u64::try_from(VERSION_LIFETIME.as_millis()).unwrap_or(u64::MAX);
    now.saturating_sub(since) >= lifetime
}

/// The stamps of [`Replaced`] in one bucket of this many milliseconds are
/// one: the older versions are kept until the newer stamp, a minute at
/// most longer, so a table keeps about sixty of them however often rounds
/// run.
const STAMP_BUCKET: u64 = 60_000;

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
    /// The tables whose collection went, and the texts datasets of
    /// columns a table lost, until their folders are removed, an hour
    /// later: a reader of an older `commit` may still read them.
    pub(crate) dropped: Vec<Retired>,
    /// Whether the tables record what they replaced
    /// ([`Table::replaced`]). A `commit` of an earlier Scheda does not: the
    /// next round keeps every version of it for one more hour.
    #[serde(default)]
    pub(crate) records_replaced: bool,
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
            records_replaced: true,
        }
    }

    /// Stamps, at `now`, the moment of everything this `commit` stopped
    /// naming that has no moment yet, and forgets what was replaced more
    /// than [`VERSION_LIFETIME`] ago. Called by a round on the live
    /// `commit` it read, before it writes anything: the round that wrote
    /// this `commit` had ended by then.
    pub(crate) fn stamp(&mut self, now: u64) {
        if let Some(retired) = &mut self.retired {
            retired.stamp(now);
        }
        for dropped in &mut self.dropped {
            dropped.stamp(now);
        }
        let earlier = !self.records_replaced;
        self.records_replaced = true;
        for table in self.tables.values_mut() {
            if earlier {
                // Unknown: every version is kept for one more hour.
                table.replaced = vec![Replaced {
                    versions: Versions {
                        rows: 0,
                        files: 0,
                        texts: table
                            .versions
                            .texts
                            .keys()
                            .map(|c| (c.clone(), 0))
                            .collect(),
                    },
                    since: Some(now),
                }];
                continue;
            }
            let mut kept: Vec<Replaced> = Vec::with_capacity(table.replaced.len());
            for mut replaced in table.replaced.drain(..) {
                replaced.since.get_or_insert(now);
                if !replaced.held(now) {
                    continue;
                }
                match kept.last_mut() {
                    // Of one bucket: the older versions, until the newer
                    // stamp.
                    Some(last)
                        if last
                            .since
                            .zip(replaced.since)
                            .is_some_and(|(a, b)| a / STAMP_BUCKET == b / STAMP_BUCKET) =>
                    {
                        last.since = replaced.since;
                    }
                    _ => kept.push(replaced),
                }
            }
            table.replaced = kept;
        }
    }

    /// The state the generation reflects.
    pub(crate) fn state(&self) -> Option<State> {
        State::parse(&self.state)
    }

    /// The folder name of a new table (`t{n}`), and the count moved on.
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
        create_dir_private(&self.root).context("create the Index folder")?;
        let mut next = 0u64;
        for entry in fs::read_dir(&self.root)
            .map_err(io)
            .context("list the generations of the Index")?
        {
            let entry = entry
                .map_err(io)
                .context("list the generations of the Index")?;
            if let Some(n) = entry.file_name().to_str().and_then(number_of) {
                next = next.max(n.saturating_add(1));
            }
        }
        let generation = self.generation(&format!("{PREFIX}{next}"));
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
        let now = now_millis();
        let keep_retired = retired
            .filter(|retired| !retired.due(now))
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
    number_of(name).is_some()
}

/// The number in a generation's name `g{n}`; `None` for any other name.
fn number_of(name: &str) -> Option<u64> {
    generation_number(name)
}

/// Whether the folder `folder` is a dataset of `dropped`: a table (`t3`,
/// all its datasets, which follow its number with a letter) or a texts
/// dataset (`t3x0`, itself).
fn dataset_of(folder: &str, dropped: &str) -> bool {
    match folder.strip_prefix(dropped) {
        Some("") => true,
        Some(rest) => {
            let table = dropped
                .strip_prefix('t')
                .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()));
            table && rest.starts_with(|c: char| c.is_ascii_alphabetic())
        }
        None => false,
    }
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

    /// Removes the folders of the tables and texts datasets `commit`
    /// dropped more than an hour ago, and forgets those it removed: the
    /// dataset folders of a dropped table `t3` (`t3d`, `t3f`, `t3x0`, …,
    /// never `t30d`), or the one folder of a dropped texts dataset `t3x0`.
    /// Under `index.lock`; `commit` is written after.
    pub(crate) fn sweep_tables(&self, _cx: &Blocking, commit: &mut Commit) {
        let now = now_millis();
        if !commit.dropped.iter().any(|table| table.due(now)) {
            return;
        }
        // A folder that cannot be listed now is listed at the next round.
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let names: Vec<String> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .collect();
        commit.dropped.retain(|table| {
            if !table.due(now) {
                return true;
            }
            // A table that cannot be removed now (a reader holds a file of
            // it open) stays listed, and the next round tries again.
            let removed = names
                .iter()
                .filter(|name| dataset_of(name, &table.name))
                .all(|name| match fs::remove_dir_all(self.dir.join(name)) {
                    Ok(()) => true,
                    Err(err) => err.kind() == std::io::ErrorKind::NotFound,
                });
            !removed
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
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{
        Commit, Layout, Ports, Replaced, Retired, Table, Versions, now_millis, valid_name,
    };

    #[test]
    fn only_names_of_generations_are_generations() {
        assert!(valid_name("g0"));
        assert!(valid_name("g18"));
        assert!(!valid_name("g"));
        assert!(!valid_name("g01"));
        assert!(!valid_name("g-1"));
        assert!(!valid_name("journal"));
        assert!(!valid_name("g1/../x"));
        assert!(!valid_name("g-18f3a-1c2-0"));
        assert!(!valid_name("current"));
    }

    #[tokio::test]
    async fn a_dropped_table_takes_every_folder_of_its_name_and_no_other() {
        let base = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let root = base.join(format!("scheda-generation-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        crate::testing::remove_when_the_test_ends(&root);
        let generation = Layout::new(&root).generation("g1");
        for name in ["t3d", "t3f", "t3x0", "t30d", "t4d", "t4x0", "t4x1"] {
            std::fs::create_dir_all(generation.dir().join(name).join("data")).unwrap();
        }
        let mut commit = Commit::empty(Ports::default());
        commit.dropped = vec![
            Retired {
                name: String::from("t3"),
                at: 0,
                stamped: true,
            },
            Retired {
                name: String::from("t4x0"),
                at: 0,
                stamped: true,
            },
            Retired {
                name: String::from("t30"),
                at: now_millis(),
                stamped: true,
            },
            // Dropped by the round that wrote this commit: not stamped yet.
            Retired::now(String::from("t31")),
        ];
        let commit = {
            let generation = generation.clone();
            scheda_blocking::blocking(move |cx| {
                generation.sweep_tables(cx, &mut commit);
                Ok::<_, crate::error::Error>(commit)
            })
            .await
            .unwrap()
        };
        let mut left: Vec<String> = std::fs::read_dir(generation.dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["t30d", "t4d", "t4x1"]);
        let kept: Vec<&str> = commit
            .dropped
            .iter()
            .map(|table| table.name.as_str())
            .collect();
        assert_eq!(
            kept,
            ["t30", "t31"],
            "the hour of t30 is not over, that of t31 has not begun"
        );
    }

    fn versions(rows: u64, files: u64, texts: &[(&str, u64)]) -> Versions {
        Versions {
            rows,
            files,
            texts: texts
                .iter()
                .map(|(column, version)| ((*column).to_owned(), *version))
                .collect(),
        }
    }

    fn table(live: Versions, replaced: Vec<Replaced>) -> Table {
        Table {
            name: String::from("t0"),
            texts: BTreeMap::new(),
            next_text: 0,
            shape: crate::store::Shape::default(),
            declared: super::Declared::default(),
            versions: live,
            replaced,
        }
    }

    const HOUR: u64 = 3_600_000;

    #[test]
    fn a_version_is_kept_for_an_hour_from_the_stamp_the_round_after_its_replacement_gives_it() {
        let now = 10 * HOUR;
        let mut commit = Commit::empty(Ports::default());
        commit.tables.insert(
            String::from("notes"),
            table(
                versions(9, 5, &[("tags", 7)]),
                vec![
                    // Replaced long ago: forgotten, and pruned.
                    Replaced {
                        versions: versions(2, 2, &[("tags", 2)]),
                        since: Some(now - 2 * HOUR),
                    },
                    // Replaced less than an hour ago: kept.
                    Replaced {
                        versions: versions(5, 3, &[("tags", 4)]),
                        since: Some(now - HOUR + 1),
                    },
                    // Replaced by the round that wrote this commit, however
                    // long that round took: kept, from now on.
                    Replaced {
                        versions: versions(8, 4, &[("tags", 6)]),
                        since: None,
                    },
                ],
            ),
        );
        let held = &commit.tables["notes"];
        assert_eq!(held.keep_from(now), versions(5, 3, &[("tags", 4)]));
        commit.stamp(now);
        let stamped = &commit.tables["notes"];
        assert_eq!(
            stamped
                .replaced
                .iter()
                .map(|replaced| (replaced.versions.rows, replaced.since))
                .collect::<Vec<_>>(),
            [(5, Some(now - HOUR + 1)), (8, Some(now))]
        );
        // An hour after the oldest stamp, the pending one alone is kept.
        assert_eq!(stamped.keep_from(now + 2), versions(8, 4, &[("tags", 6)]));
        assert_eq!(
            stamped.keep_from(now + HOUR),
            versions(9, 5, &[("tags", 7)])
        );
    }

    #[test]
    fn stamps_within_a_minute_are_one_that_keeps_the_older_versions() {
        let now = 10 * HOUR;
        let mut commit = Commit::empty(Ports::default());
        commit.tables.insert(
            String::from("notes"),
            table(
                versions(9, 9, &[]),
                vec![Replaced {
                    versions: versions(3, 3, &[]),
                    since: Some(now - now % super::STAMP_BUCKET),
                }],
            ),
        );
        commit
            .tables
            .get_mut("notes")
            .unwrap()
            .replaced
            .push(Replaced {
                versions: versions(6, 6, &[]),
                since: None,
            });
        commit.stamp(now - now % super::STAMP_BUCKET + 10);
        let replaced = &commit.tables["notes"].replaced;
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].versions, versions(3, 3, &[]));
        assert_eq!(
            replaced[0].since,
            Some(now - now % super::STAMP_BUCKET + 10)
        );
    }

    #[test]
    fn a_commit_of_an_earlier_scheda_keeps_every_version_for_an_hour_and_retired_ones_get_their_stamp()
     {
        let mut commit: Commit = serde_json::from_value(serde_json::json!({
            "state": "",
            "ports": {"projection": null, "embedder": null},
            "tables": {"notes": {
                "name": "t0",
                "shape": {"columns": [], "vectors": [], "dimensions": null},
                "declared": {"columns": [], "vectors": []},
                "versions": {"rows": 12, "files": 8, "texts": {"tags": 4}}
            }},
            "next_table": 1,
            "rounds": 3,
            "retired": {"name": "g-1", "at": 5},
            "dropped": [{"name": "t9", "at": 6}]
        }))
        .unwrap();
        assert!(!commit.records_replaced);
        assert!(
            commit.dropped[0].stamped && commit.dropped[0].at == 6,
            "an earlier stamp is read"
        );
        let now = 10 * HOUR;
        commit.stamp(now);
        assert!(commit.records_replaced);
        assert_eq!(
            commit.tables["notes"].keep_from(now + HOUR - 1),
            versions(0, 0, &[("tags", 0)])
        );
        assert_eq!(
            commit.tables["notes"].keep_from(now + HOUR),
            versions(12, 8, &[("tags", 4)])
        );

        // An earlier Scheda reads what this one writes: its `Retired` had a
        // number `at` and no `stamped`, its `Table` no `replaced`.
        #[derive(serde::Deserialize)]
        struct Earlier {
            name: String,
            at: u64,
        }
        let mut written = Commit::empty(Ports::default());
        written.dropped.push(Retired::now(String::from("t9")));
        let json = serde_json::to_value(&written).unwrap();
        let read: Vec<Earlier> = serde_json::from_value(json["dropped"].clone()).unwrap();
        assert_eq!(read[0].name, "t9");
        assert!(
            read[0].at > 0,
            "the moment it was replaced, for an earlier Scheda"
        );

        let mut switched = Commit::empty(Ports::default());
        switched.retired = Some(Retired::now(String::from("g-1")));
        assert!(!switched.retired.as_ref().unwrap().due(u64::MAX));
        switched.stamp(now);
        assert_eq!(switched.retired.as_ref().unwrap().at, now);
        assert!(!switched.retired.as_ref().unwrap().due(now + HOUR - 1));
        assert!(switched.retired.as_ref().unwrap().due(now + HOUR));
    }
}
