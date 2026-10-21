//! Journal segments: the files of a `journal` collection, each line one
//! Document.
//!
//! A line is `{id}: {payload}\n` on bytes. The id is a slug; the payload is
//! any bytes except `\n` and `\r`, because those are what end a line. That
//! is the one rule the form sets, and none is made about the content.
//! A segment is named `{seq}-{writer}.jsonl`, or, in a collection
//! that rotates daily, `{date}-{writer}.jsonl` and
//! `{date}-{seq}-{writer}.jsonl`; the writer in the name keeps two clones of
//! one Vault from appending to the same file.
//!
//! This module reads segments, names them, and decides when one is full. It
//! reads the segments of a snapshot of the commit `HEAD` names, never the
//! work tree. Appending a line, and the id maps of the Index, belong to the
//! writes. A segment is named here by its Vault-relative path.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::iter::FusedIterator;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use scheda_api::v1::{Problem, VaultErrorCode};

use crate::check::problem;
use crate::collection::{DEFAULT_SEGMENT_MAX_BYTES, Declaration, Declarations, Kind};
use crate::error::{Context, Error, Fault};
use crate::id::require_slug;
use crate::isolated;
use crate::repo::{NodeKind, Snap, Views};

fn invalid(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::InvalidArgument, message)
}

/// The delimiter between the id and the payload of a line.
const DELIMITER: &[u8] = b": ";

/// One line of a journal: a Document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Line {
    /// The id, the text before the first `: `.
    pub(crate) id: String,
    /// The payload, the bytes after it up to the end of the line.
    pub(crate) payload: Vec<u8>,
}

/// Encodes one line: `{id}: {payload}\n`.
///
/// The id must be a slug and the payload must hold no `\n` and no `\r`;
/// either way it is `INVALID_ARGUMENT`. Any other bytes, those that are not
/// UTF-8 included, are stored as they are.
pub(crate) fn encode_line(id: &str, payload: &[u8]) -> Result<Vec<u8>, Fault> {
    require_slug("id", id)?;
    if payload.iter().any(|&byte| byte == b'\n' || byte == b'\r') {
        return Err(invalid("a journal payload must not contain CR or LF"));
    }
    let mut line = Vec::with_capacity(id.len() + DELIMITER.len() + payload.len() + 1);
    line.extend_from_slice(id.as_bytes());
    line.extend_from_slice(DELIMITER);
    line.extend_from_slice(payload);
    line.push(b'\n');
    Ok(line)
}

/// Why a line does not parse.
enum Reject {
    Empty,
    Cr,
    NoDelimiter,
    EmptyId,
    NotUtf8(std::str::Utf8Error),
}

/// The id and the payload of one line without its `\n`, borrowed from it.
/// The rules of a line live here.
fn split_line(line: &[u8]) -> Result<(&str, &[u8]), Reject> {
    if line.is_empty() {
        return Err(Reject::Empty);
    }
    if line.contains(&b'\r') {
        return Err(Reject::Cr);
    }
    let Some(at) = line
        .windows(DELIMITER.len())
        .position(|window| window == DELIMITER)
    else {
        return Err(Reject::NoDelimiter);
    };
    if at == 0 {
        return Err(Reject::EmptyId);
    }
    let id = std::str::from_utf8(&line[..at]).map_err(Reject::NotUtf8)?;
    Ok((id, &line[at + DELIMITER.len()..]))
}

/// Parses one line without its `\n`. A line with no `: `, with an empty id
/// or an id that is not UTF-8, or one that holds a `\r`, is
/// `INVALID_ARGUMENT`.
pub(crate) fn parse_line(line: &[u8]) -> Result<Line, Fault> {
    match split_line(line) {
        Ok((id, payload)) => Ok(Line {
            id: id.to_owned(),
            payload: payload.to_vec(),
        }),
        Err(Reject::Empty) => Err(invalid("the journal line is empty")),
        Err(Reject::Cr) => Err(invalid("the journal line holds a CR")),
        Err(Reject::NoDelimiter) => Err(invalid("the journal line has no ': ' after its id")),
        Err(Reject::EmptyId) => Err(invalid("the journal line has an empty id")),
        Err(Reject::NotUtf8(err)) => {
            Err(invalid("the journal line has an id that is not UTF-8").with_source(err))
        }
    }
}

/// Splits the bytes of a segment into its complete lines (each without its
/// `\n`) and a tail that no `\n` ended, the trace of an interrupted write.
pub(crate) fn split_lines(bytes: &[u8]) -> (Vec<&[u8]>, Option<&[u8]>) {
    let (complete, tail) = match bytes.iter().rposition(|&byte| byte == b'\n') {
        Some(last) => (&bytes[..last], &bytes[last + 1..]),
        None => (&bytes[..0], bytes),
    };
    let lines = if bytes.len() == tail.len() {
        Vec::new()
    } else {
        complete.split(|&byte| byte == b'\n').collect()
    };
    (lines, (!tail.is_empty()).then_some(tail))
}

/// The lines of a segment that parse, in order, as `(id, payload)` borrowed
/// from `bytes`: nothing is copied. A line that does not parse and a tail
/// without `\n` are left out; reading a journal strictly is [`LinesState`],
/// and [`check_journals`] reports what is left out. These are the rules of
/// a segment; [`find_line`] is built on them.
pub(crate) fn lines(bytes: &[u8]) -> impl Iterator<Item = (&str, &[u8])> {
    split_lines(bytes)
        .0
        .into_iter()
        .filter_map(|line| split_line(line).ok())
}

/// The first line of a segment with the id `id`, copied; every other line
/// is only looked at.
pub(crate) fn find_line(bytes: &[u8], id: &str) -> Option<Line> {
    lines(bytes)
        .find(|(line_id, _)| *line_id == id)
        .map(|(line_id, payload)| Line {
            id: line_id.to_owned(),
            payload: payload.to_vec(),
        })
}

/// The lines of a segment that parse, in order, copied (see [`lines`]); the
/// tests compare the borrowed walk with it. Nothing in the library copies a
/// whole segment.
#[cfg(test)]
pub(crate) fn parse_segment(bytes: &[u8]) -> Vec<Line> {
    lines(bytes)
        .map(|(id, payload)| Line {
            id: id.to_owned(),
            payload: payload.to_vec(),
        })
        .collect()
}

/// The bytes of the last segment a reader took through it, with the id of
/// its blob. A reader that asks for many lines of one segment (a page of a
/// query) reads the blob once: the next ask finds the node of the path in
/// the tree, which reads no blob, and the same blob id is the same bytes,
/// whichever snapshot asks. Only one segment is kept, so the memory is
/// bounded by it.
#[derive(Debug, Default)]
pub(crate) struct SegmentMemo {
    last: Option<(git2::Oid, Vec<u8>)>,
}

impl SegmentMemo {
    /// The bytes of the segment `path` in the commit of `snap`; `None` when
    /// the commit holds no such file.
    pub(crate) fn read(&mut self, snap: &Snap, path: &Path) -> Result<Option<&[u8]>, Fault> {
        let rel = rel_str(path);
        let Some(node) = snap.lookup(&rel)? else {
            return Ok(None);
        };
        if node.kind != NodeKind::File {
            return Ok(None);
        }
        if self.last.as_ref().map(|(id, _)| *id) != Some(node.id) {
            let bytes = snap.blob(node.id).map_err(|fault| fault.with_path(&rel))?;
            self.last = Some((node.id, bytes));
        }
        Ok(self.last.as_ref().map(|(_, bytes)| bytes.as_slice()))
    }
}

/// When a segment is full: the limits of a declared journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Limits {
    /// Bytes per segment.
    pub(crate) max_bytes: u64,
    /// Lines per segment, if capped.
    pub(crate) max_lines: Option<u64>,
    /// Whether a new day (UTC) starts a new segment.
    pub(crate) daily: bool,
}

impl Limits {
    /// The limits of `decl`, whose byte limit `open` has filled in.
    /// `INVALID_ARGUMENT` unless `decl` is a journal.
    pub(crate) fn of(decl: &Declaration) -> Result<Limits, Fault> {
        let journal = decl
            .journal()
            .ok_or_else(|| invalid(format!("the collection {} is not a journal", decl.name())))?;
        Ok(Limits {
            max_bytes: journal
                .segment_max_bytes
                .unwrap_or(DEFAULT_SEGMENT_MAX_BYTES),
            max_lines: journal.segment_max_lines,
            daily: journal.daily.is_set(),
        })
    }
}

/// The segment files of a journal, in append order: sequence files before
/// daily ones, then by date, sequence, writer, and name. Only plain
/// `*.jsonl` files count; a hidden name and a symlink do not. A journal with
/// no directory in the commit has none; a symlink on the way to it is
/// `INVALID_ARGUMENT`. The paths are Vault-relative.
pub(crate) fn list_segments(snap: &Snap, decl: &Declaration) -> Result<Vec<PathBuf>, Fault> {
    require_journal(decl)?;
    let dir = isolated::vault_rel(decl, "");
    let dir = dir.trim_end_matches('/');
    match snap.lookup(dir)? {
        None => return Ok(Vec::new()),
        Some(node) if node.kind != NodeKind::Dir => {
            return Err(invalid("the journal is not a directory").with_path(dir));
        }
        Some(_) => {}
    }
    let mut out = Vec::new();
    collect_segments(snap, dir, &mut out)?;
    out.sort_by_cached_key(|path| segment_order(path));
    Ok(out)
}

/// Adds the segment files of the folder `dir` to `out`, and those of the
/// layout folders (`_<hex>`) below it, however a person or a mirror split
/// them.
fn collect_segments(snap: &Snap, dir: &str, out: &mut Vec<PathBuf>) -> Result<(), Fault> {
    for child in snap.list(dir)?.unwrap_or_default() {
        match child.node.kind {
            NodeKind::File if !child.name.starts_with('.') && child.name.ends_with(".jsonl") => {
                out.push(PathBuf::from(format!("{dir}/{}", child.name)));
            }
            NodeKind::Dir if crate::layout::decode(&child.name).is_some() => {
                collect_segments(snap, &format!("{dir}/{}", child.name), out)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// The Vault-relative form of a segment path, with `/` on every platform.
pub(crate) fn rel_str(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The bytes of the segment `path` in the commit of `snap`; `None` when the
/// commit holds no such file.
pub(crate) fn read_segment(snap: &Snap, path: &Path) -> Result<Option<Vec<u8>>, Fault> {
    let rel = rel_str(path);
    Ok(snap.read(&rel)?.map(|blob| blob.bytes))
}

fn require_journal(decl: &Declaration) -> Result<(), Fault> {
    if decl.kind() == Kind::Journal {
        Ok(())
    } else {
        Err(invalid(format!(
            "the collection {} is not a journal",
            decl.name()
        )))
    }
}

/// A lazy walk of the lines of a journal in append order: segment by
/// segment, line by line, over the commit of the snapshot it was given. A
/// segment is read when the walk reaches it; no line is held after it is
/// returned.
///
/// A tail without `\n` and a line that does not parse are
/// `INVALID_ARGUMENT` with the path of the segment, and end the walk.
///
/// A walk borrows its snapshot. A stream that must own the snapshot keeps a
/// [`LinesState`] next to it and passes it at each step.
#[cfg(test)]
pub(crate) struct JournalLines<'a> {
    snap: &'a Snap,
    state: LinesState,
}

#[cfg(test)]
impl<'a> JournalLines<'a> {
    /// Starts a walk of `decl`, a journal.
    pub(crate) fn new(snap: &'a Snap, decl: &Declaration) -> Result<JournalLines<'a>, Fault> {
        Ok(JournalLines {
            snap,
            state: LinesState::new(snap, decl)?,
        })
    }

    /// The next line, or `None` at the end.
    pub(crate) fn next_line(&mut self) -> Result<Option<Line>, Fault> {
        self.state.next_line(self.snap)
    }
}

/// Where a walk of the lines of a journal is, apart from the snapshot it
/// reads: the segments, and the one it is inside. It owns all of it; every
/// step is given the snapshot the walk was started with.
pub(crate) struct LinesState {
    segments: Vec<PathBuf>,
    next_segment: usize,
    open: Option<(Vec<u8>, usize, String)>,
    done: bool,
}

impl LinesState {
    /// Starts a walk of `decl`, a journal, over `snap`.
    pub(crate) fn new(snap: &Snap, decl: &Declaration) -> Result<LinesState, Fault> {
        Ok(LinesState {
            segments: list_segments(snap, decl)?,
            next_segment: 0,
            open: None,
            done: false,
        })
    }

    /// Begins the walk at `segment`, passing over the segments before it in
    /// append order. A segment the journal does not have leaves the walk
    /// where it is.
    pub(crate) fn start_at(&mut self, segment: &Path) {
        if let Some(at) = self.segments.iter().position(|path| path == segment) {
            self.next_segment = at;
        }
    }

    /// The next line, or `None` at the end. After a failure the walk is at
    /// its end.
    pub(crate) fn next_line(&mut self, snap: &Snap) -> Result<Option<Line>, Fault> {
        if self.done {
            return Ok(None);
        }
        let step = self.advance(snap);
        if !matches!(step, Ok(Some(_))) {
            self.done = true;
        }
        step
    }

    fn advance(&mut self, snap: &Snap) -> Result<Option<Line>, Fault> {
        loop {
            let (bytes, at, rel) = match self.open.take() {
                Some(open) => open,
                None => {
                    let Some(path) = self.segments.get(self.next_segment) else {
                        return Ok(None);
                    };
                    self.next_segment += 1;
                    let rel = rel_str(path);
                    let Some(bytes) = read_segment(snap, path)? else {
                        continue;
                    };
                    (bytes, 0, rel)
                }
            };
            if at >= bytes.len() {
                continue;
            }
            let Some(len) = bytes[at..].iter().position(|byte| *byte == b'\n') else {
                return Err(invalid("the segment ends in a line without a newline").with_path(rel));
            };
            let line = parse_line(&bytes[at..at + len]).map_err(|fault| {
                invalid(format!(
                    "the segment has a broken line: {}",
                    fault.message()
                ))
                .with_path(rel.as_str())
            })?;
            self.open = Some((bytes, at + len + 1, rel));
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
impl Iterator for JournalLines<'_> {
    type Item = Result<Line, Fault>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_line().transpose()
    }
}

#[cfg(test)]
impl FusedIterator for JournalLines<'_> {}

/// Sort key that puts the segments of one journal in append order.
pub(crate) type SegmentOrder = (u8, String, u32, String, String);

/// Sequence files (`00000001…`) before daily (`YYYY-MM-DD…`), then sequence
/// number (daily overflow after the first file of that date), then writer
/// (writerless names first, so a Vault from before the writer was named
/// keeps its append order), then the file name as a last tie-break.
pub(crate) fn segment_order(path: &Path) -> SegmentOrder {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_owned();
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return (2, String::new(), 0, String::new(), name);
    };
    if let Some((seq, writer)) = sequence_order(stem) {
        return (0, String::new(), seq, writer, name);
    }
    if let Some((date, seq, writer)) = daily_order(stem) {
        return (1, date, seq, writer, name);
    }
    (2, String::new(), 0, String::new(), name)
}

fn sequence_order(stem: &str) -> Option<(u32, String)> {
    if let Some(seq) = parse_hex_seq(stem) {
        return Some((seq, String::new()));
    }
    let (hex, writer) = stem.split_once('-')?;
    let seq = parse_hex_seq(hex)?;
    if writer.is_empty() {
        return None;
    }
    Some((seq, writer.to_owned()))
}

fn daily_order(stem: &str) -> Option<(String, u32, String)> {
    let date = segment_date_stem(stem)?;
    if stem == date {
        return Some((date.to_owned(), 0, String::new()));
    }
    if stem.as_bytes().get(date.len()) != Some(&b'-') {
        return None;
    }
    let rest = &stem[date.len() + 1..];
    if let Some(seq) = parse_hex_seq(rest) {
        return Some((date.to_owned(), seq, String::new()));
    }
    if let Some((hex, writer)) = rest.split_once('-')
        && let Some(seq) = parse_hex_seq(hex)
        && !writer.is_empty()
    {
        return Some((date.to_owned(), seq, writer.to_owned()));
    }
    if rest.is_empty() {
        return None;
    }
    Some((date.to_owned(), 0, rest.to_owned()))
}

fn parse_hex_seq(stem: &str) -> Option<u32> {
    if stem.len() != 8 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(stem, 16).ok()
}

/// Writer named in a segment stem, if this file is writer-owned.
fn segment_writer(stem: &str) -> Option<&str> {
    if parse_hex_seq(stem).is_some() {
        return None;
    }
    if segment_date_stem(stem).is_some_and(|date| date == stem) {
        return None;
    }
    if let Some(date) = segment_date_stem(stem)
        && stem.len() > date.len() + 1
        && stem.as_bytes().get(date.len()) == Some(&b'-')
    {
        let rest = &stem[date.len() + 1..];
        if parse_hex_seq(rest).is_some() {
            return None;
        }
        if let Some((hex, writer)) = rest.split_once('-')
            && parse_hex_seq(hex).is_some()
            && !writer.is_empty()
        {
            return Some(writer);
        }
        return Some(rest);
    }
    if let Some((hex, writer)) = stem.split_once('-')
        && parse_hex_seq(hex).is_some()
        && !writer.is_empty()
    {
        return Some(writer);
    }
    None
}

fn segment_belongs_to(path: &Path, writer: &str) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| segment_writer(stem) == Some(writer))
}

fn segment_date(path: &Path) -> Option<&str> {
    segment_date_stem(path.file_stem()?.to_str()?)
}

fn segment_date_stem(stem: &str) -> Option<&str> {
    let bytes = stem.as_bytes();
    if bytes.len() >= 10 && bytes[4] == b'-' && bytes[7] == b'-' {
        stem.get(..10)
    } else {
        None
    }
}

/// Whether the segment at `path` (Vault-relative), as the commit of `snap`
/// holds it, cannot take a line of `line_len` bytes more. A segment that
/// does not exist or is empty is never full. Otherwise it is full when:
///
/// - the collection rotates daily and the segment is not from `today`
///   (`YYYY-MM-DD`, UTC);
/// - the line would take it over the byte limit;
/// - it already holds the most lines the limit allows.
pub(crate) fn segment_is_full(
    snap: &Snap,
    path: &Path,
    limits: &Limits,
    line_len: u64,
    today: &str,
) -> Result<bool, Fault> {
    let Some(size) = snap.size(&rel_str(path))? else {
        return Ok(false);
    };
    segment_is_full_by(
        path,
        size,
        || count_newlines(snap, path),
        limits,
        line_len,
        today,
    )
}

/// [`segment_is_full`] for a segment whose size is known and whose lines
/// `count` counts only when the limit asks for it.
fn segment_is_full_by(
    path: &Path,
    size: u64,
    count: impl FnOnce() -> Result<u64, Fault>,
    limits: &Limits,
    line_len: u64,
    today: &str,
) -> Result<bool, Fault> {
    if size == 0 {
        return Ok(false);
    }
    if limits.daily && segment_date(path).is_none_or(|date| date != today) {
        return Ok(true);
    }
    if size.saturating_add(line_len) > limits.max_bytes {
        return Ok(true);
    }
    if let Some(max_lines) = limits.max_lines
        && count()? >= max_lines
    {
        return Ok(true);
    }
    Ok(false)
}

fn count_newlines(snap: &Snap, path: &Path) -> Result<u64, Fault> {
    let bytes = read_segment(snap, path)?.unwrap_or_default();
    Ok(bytes.iter().filter(|byte| **byte == b'\n').count() as u64)
}

/// The name of the segment `writer` starts next, given the segments it
/// already has (`existing`, any order; those of other writers are ignored).
///
/// Without daily rotation the name is `{seq}-{writer}.jsonl`, `seq` eight
/// lowercase hexadecimal digits, one past the writer's highest. With it, the
/// first segment of `today` is `{today}-{writer}.jsonl` and each overflow
/// `{today}-{seq}-{writer}.jsonl`.
pub(crate) fn next_segment_name(
    existing: &[PathBuf],
    daily: bool,
    writer: &str,
    today: &str,
) -> Result<String, Fault> {
    let exhausted = || {
        Fault::new(
            VaultErrorCode::FailedPrecondition,
            "the journal segment sequence is exhausted",
        )
    };
    let writer_suffix = format!("-{writer}");
    if daily {
        let prefix = format!("{today}-");
        let plain = format!("{today}-{writer}");
        let mut seq = 0u32;
        let mut has_plain = false;
        for path in existing {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem == plain {
                has_plain = true;
                continue;
            }
            let Some(rest) = stem.strip_prefix(&prefix) else {
                continue;
            };
            let Some(hex) = rest.strip_suffix(&writer_suffix) else {
                continue;
            };
            if let Some(n) = parse_hex_seq(hex) {
                seq = seq.max(n);
            }
        }
        if !has_plain && seq == 0 {
            return Ok(format!("{today}-{writer}.jsonl"));
        }
        let next = seq.checked_add(1).ok_or_else(exhausted)?;
        return Ok(format!("{today}-{next:08x}-{writer}.jsonl"));
    }
    let mut max = 0u32;
    for path in existing {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(hex) = stem.strip_suffix(&writer_suffix) else {
            continue;
        };
        if let Some(n) = parse_hex_seq(hex) {
            max = max.max(n);
        }
    }
    let next = max.checked_add(1).ok_or_else(exhausted)?;
    Ok(format!("{next:08x}-{writer}.jsonl"))
}

/// The segment `writer` appends a line of `line_len` bytes to: its last
/// segment, unless that is full, otherwise a new one named by
/// [`next_segment_name`]. A segment of another writer is never chosen.
#[cfg(test)]
pub(crate) fn segment_for_append(
    snap: &Snap,
    decl: &Declaration,
    line_len: u64,
    writer: &str,
    today: &str,
) -> Result<PathBuf, Fault> {
    segment_for_append_in(snap, decl, line_len, writer, today, &BTreeMap::new())
}

/// The segment `writer` appends a line to, for a call that appends several lines before any
/// is stored: `pending` holds the whole new bytes of the segments the call
/// has appended to so far, by path. A segment in it is measured by those
/// bytes, and one that is not on disk yet counts as a segment of the writer.
pub(crate) fn segment_for_append_in(
    snap: &Snap,
    decl: &Declaration,
    line_len: u64,
    writer: &str,
    today: &str,
    pending: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<PathBuf, Fault> {
    let limits = Limits::of(decl)?;
    let dir = PathBuf::from(isolated::vault_rel(decl, "").trim_end_matches('/'));
    let mut mine: Vec<PathBuf> = list_segments(snap, decl)?
        .into_iter()
        .filter(|path| segment_belongs_to(path, writer))
        .collect();
    for path in pending.keys() {
        if path.parent() == Some(dir.as_path())
            && segment_belongs_to(path, writer)
            && !mine.contains(path)
        {
            mine.push(path.clone());
        }
    }
    mine.sort_by_cached_key(|path| segment_order(path));
    if let Some(last) = mine.last() {
        let full = match pending.get(last) {
            Some(bytes) => segment_is_full_by(
                last,
                bytes.len() as u64,
                || Ok(bytes.iter().filter(|byte| **byte == b'\n').count() as u64),
                &limits,
                line_len,
                today,
            )?,
            None => segment_is_full(snap, last, &limits, line_len, today)?,
        };
        if !full {
            return Ok(last.clone());
        }
    }
    let name = next_segment_name(&mine, limits.daily, writer, today)?;
    Ok(dir.join(name))
}

/// Today's date in UTC, `YYYY-MM-DD`, as segment names carry it.
pub(crate) fn today_utc() -> Result<String, Fault> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| {
            Fault::new(
                VaultErrorCode::FailedPrecondition,
                "the system clock is before 1970",
            )
            .with_source(err)
        })?
        .as_secs();
    let (year, month, day) = civil_from_unix_days(secs / 86_400);
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

/// Civil date from days since the Unix epoch (Howard Hinnant).
fn civil_from_unix_days(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(m <= 2), m, d)
}

/// Checks the segments of every `journal` collection and collects what is
/// wrong, in collection and segment order:
///
/// - a symlink under the collection directory (`INVALID_ARGUMENT`);
/// - a segment that ends in a line without `\n` (`INVALID_ARGUMENT`);
/// - a line that is not `{id}: {payload}` (`INVALID_ARGUMENT`);
/// - an id that two lines of one journal share (`ALREADY_EXISTS`).
///
/// The payload is never checked: it is bytes. The call fails only
/// when it cannot run (an I/O failure).
pub(crate) fn check_journals(
    views: &Views,
    declarations: &Declarations,
) -> Result<Vec<Problem>, Error> {
    let mut problems = Vec::new();
    for decl in declarations.iter() {
        if decl.kind() != Kind::Journal {
            continue;
        }
        let snap = views.of(decl.repo());
        let dir_rel = isolated::vault_rel(decl, "");
        let dir_rel = dir_rel.trim_end_matches('/');
        let links = snap
            .symlinks_below(dir_rel)
            .context("check_journals: look for symlinks")?;
        let blocked = links
            .iter()
            .any(|link| dir_rel == link || dir_rel.starts_with(&format!("{link}/")));
        for link in links {
            let message = format!("{link} is a symlink");
            problems.push(problem(
                VaultErrorCode::InvalidArgument,
                Some(&link),
                message,
            ));
        }
        if blocked {
            continue;
        }
        check_segments(snap, decl, &mut problems).context("check_journals: check the segments")?;
    }
    Ok(problems)
}

fn check_segments(
    snap: &Snap,
    decl: &Declaration,
    problems: &mut Vec<Problem>,
) -> Result<(), Fault> {
    let mut seen = BTreeSet::new();
    for path in list_segments(snap, decl)? {
        let rel = rel_str(&path);
        let bytes = read_segment(snap, &path)?.unwrap_or_default();
        let (lines, tail) = split_lines(&bytes);
        for (n, line) in lines.into_iter().enumerate() {
            match parse_line(line) {
                Ok(parsed) => {
                    if !seen.insert(parsed.id.clone()) {
                        let message = format!(
                            "line {} repeats the id {} of the journal {}",
                            n + 1,
                            parsed.id,
                            decl.name()
                        );
                        problems.push(problem(VaultErrorCode::AlreadyExists, Some(&rel), message));
                    }
                }
                Err(fault) => {
                    let message = format!("line {} is broken: {}", n + 1, fault.message());
                    problems.push(problem(
                        VaultErrorCode::InvalidArgument,
                        Some(&rel),
                        message,
                    ));
                }
            }
        }
        if tail.is_some() {
            problems.push(problem(
                VaultErrorCode::InvalidArgument,
                Some(&rel),
                "the segment ends in a line without a newline",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use scheda_api::v1::VaultErrorCode;

    use super::{
        JournalLines, Limits, check_journals, civil_from_unix_days, encode_line, find_line, lines,
        list_segments, next_segment_name, parse_hex_seq, parse_line, parse_segment, read_segment,
        segment_belongs_to, segment_for_append, segment_is_full, segment_order, segment_writer,
        split_lines, today_utc,
    };
    use crate::collection::{self, Declaration, Declarations};
    use crate::error::{Error, Fault};
    use crate::repo::{Repos, Snap, Views, fixture};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-journal-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn declare(&self, name: &str, schema: &str) -> Declaration {
            let dir = self.0.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(".collection.yaml"),
                format!("name: {name}\nschema:\n{schema}"),
            )
            .unwrap();
            self.declarations().get(name).unwrap().clone()
        }

        /// Commits what the files hold, in the Vault and in the nested
        /// repository when there is one: reads see commits only.
        fn commit(&self) {
            // The nested repository is never tracked by the Vault.
            self.write(".gitignore", b".scheda/\n.isolated/\n");
            fixture::commit_all(&self.0);
            if self.0.join(".isolated").is_dir() {
                fixture::commit_all(&self.0.join(".isolated"));
            }
        }

        fn declarations(&self) -> Declarations {
            self.commit();
            collection::load(&fixture::snap(&self.0)).unwrap()
        }

        /// A snapshot of the commit that holds the files now, in the
        /// repository `decl` lives in.
        fn snap(&self, decl: &Declaration) -> Snap {
            self.commit();
            Repos::new(&self.0).snapshot(decl.repo()).unwrap()
        }

        fn views(&self) -> Views {
            self.commit();
            Repos::new(&self.0).views().unwrap()
        }

        fn write(&self, rel: &str, bytes: &[u8]) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
    }

    fn invalid(fault: Fault) -> bool {
        Error::from(fault).is(VaultErrorCode::InvalidArgument)
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn a_line_is_id_colon_space_payload_newline() {
        assert_eq!(
            encode_line("alpha", b"{\"m\":1}").unwrap(),
            b"alpha: {\"m\":1}\n"
        );
        assert_eq!(encode_line("p", b"").unwrap(), b"p: \n");
        let parsed = parse_line(br#"alpha: {"msg":"hello"}"#).unwrap();
        assert_eq!(parsed.id, "alpha");
        assert_eq!(parsed.payload, br#"{"msg":"hello"}"#);
        let empty = parse_line(b"p: ").unwrap();
        assert_eq!(empty.id, "p");
        assert!(empty.payload.is_empty());
        // The first ': ' ends the id; later ones belong to the payload.
        let two = parse_line(b"a: b: c").unwrap();
        assert_eq!(
            (two.id.as_str(), two.payload.as_slice()),
            ("a", b"b: c".as_slice())
        );
        for bad in [
            &b""[..],
            b"no-delimiter",
            b": payload",
            b"a:b",
            b"a: x\r",
            b"\xff: x",
        ] {
            assert!(invalid(parse_line(bad).unwrap_err()), "{bad:?}");
        }
    }

    #[test]
    fn a_payload_with_lf_or_cr_is_invalid_argument() {
        for payload in [&b"a\nb"[..], b"a\rb", b"\n", b"\r\n"] {
            assert!(
                invalid(encode_line("id", payload).unwrap_err()),
                "{payload:?}"
            );
        }
        for id in ["", "a/b", "CON", " a"] {
            assert!(invalid(encode_line(id, b"x").unwrap_err()), "{id:?}");
        }
    }

    #[test]
    fn a_payload_that_is_not_utf8_is_encoded_parsed_and_found_by_id() {
        let scratch = Scratch::new("binary");
        let log = scratch.declare("log", "  journal: {}\n");
        let payload: Vec<u8> = [0x89, b'P', b'N', b'G', 0x00, 0xFF, 0xFE, b'\t', b':', b' ']
            .into_iter()
            .chain((0..=255u8).filter(|b| *b != b'\n' && *b != b'\r'))
            .collect();
        assert!(std::str::from_utf8(&payload).is_err());
        let mut bytes = encode_line("first", b"plain").unwrap();
        bytes.extend(encode_line("second", &payload).unwrap());
        assert_eq!(parse_segment(&bytes)[1].payload, payload, "parsed back");
        scratch.write("log/00000001-w.jsonl", &bytes);
        let snap = scratch.snap(&log);
        let find = |id: &str| {
            list_segments(&snap, &log)
                .unwrap()
                .into_iter()
                .filter_map(|path| read_segment(&snap, &path).unwrap())
                .find_map(|bytes| parse_segment(&bytes).into_iter().find(|line| line.id == id))
        };
        let found = find("second").unwrap();
        assert_eq!(found.id, "second");
        assert_eq!(found.payload, payload);
        assert_eq!(find("third"), None);
    }

    #[test]
    fn lines_split_into_complete_lines_and_a_torn_tail() {
        assert_eq!(split_lines(b""), (vec![], None));
        assert_eq!(split_lines(b"\n"), (vec![&b""[..]], None));
        assert_eq!(
            split_lines(b"a: 1\nb: 2\n"),
            (vec![&b"a: 1"[..], b"b: 2"], None)
        );
        assert_eq!(
            split_lines(b"a: 1\nb: "),
            (vec![&b"a: 1"[..]], Some(&b"b: "[..]))
        );
        assert_eq!(split_lines(b"a: 1"), (vec![], Some(&b"a: 1"[..])));
        // A line that does not parse, and the torn tail, are left out.
        let parsed = parse_segment(b"a: 1\nbroken\nb: 2\nc: ");
        assert_eq!(
            parsed.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn the_borrowed_walk_and_find_line_follow_the_rules_of_parse_segment() {
        // A line that does not parse, an empty id, a CR, a repeated id, and
        // a torn tail.
        let bytes = b"a: 1\nbroken\n: x\nb: 2\r\na: 3\nb: \xff\nc: 4\nd: ";
        let walked: Vec<(String, Vec<u8>)> = lines(bytes)
            .map(|(id, payload)| (id.to_owned(), payload.to_vec()))
            .collect();
        let parsed: Vec<(String, Vec<u8>)> = parse_segment(bytes)
            .into_iter()
            .map(|line| (line.id, line.payload))
            .collect();
        assert_eq!(walked, parsed);
        assert_eq!(
            walked.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["a", "a", "b", "c"]
        );
        // The first line with the id wins, as in `parse_segment`.
        assert_eq!(find_line(bytes, "a").unwrap().payload, b"1");
        assert_eq!(find_line(bytes, "b").unwrap().payload, b"\xff");
        assert_eq!(find_line(bytes, "c").unwrap().payload, b"4");
        // The torn tail and the lines that do not parse are not found.
        assert!(find_line(bytes, "d").is_none());
        assert!(find_line(bytes, "broken").is_none());
        assert!(find_line(b"", "a").is_none());
    }

    #[test]
    fn lines_come_back_in_append_order_across_two_segments() {
        let scratch = Scratch::new("order");
        let log = scratch.declare("log", "  journal: {}\n");
        // Written out of order on purpose: the order is the one of the names.
        scratch.write("log/00000002-w.jsonl", b"c: 3\nd: \xff\n");
        scratch.write("log/00000001-w.jsonl", b"a: 1\nb: 2\n");
        scratch.write("log/notes.txt", b"ignored: x\n");
        scratch.write("log/.hidden.jsonl", b"ignored: x\n");
        let snap = scratch.snap(&log);
        let lines: Vec<_> = JournalLines::new(&snap, &log)
            .unwrap()
            .map(|line| line.unwrap())
            .map(|line| (line.id, line.payload))
            .collect();
        assert_eq!(
            lines,
            [
                ("a".to_owned(), b"1".to_vec()),
                ("b".to_owned(), b"2".to_vec()),
                ("c".to_owned(), b"3".to_vec()),
                ("d".to_owned(), vec![0xFF]),
            ]
        );
    }

    #[test]
    fn the_line_walk_reads_the_commit_it_started_on() {
        let scratch = Scratch::new("walk-commit");
        let log = scratch.declare("log", "  journal: {}\n");
        scratch.write("log/00000001-w.jsonl", b"a: 1\nb: 2\n");
        let snap = scratch.snap(&log);
        let mut walk = JournalLines::new(&snap, &log).unwrap();
        assert_eq!(walk.next().unwrap().unwrap().id, "a");
        // A line appended, a segment added, after the walk started.
        scratch.write("log/00000001-w.jsonl", b"a: 1\nb: 2\nc: 3\n");
        scratch.write("log/00000002-w.jsonl", b"d: 4\n");
        scratch.commit();
        let rest: Vec<_> = walk.map(|line| line.unwrap().id).collect();
        assert_eq!(rest, ["b"]);
        let again = scratch.snap(&log);
        let all: Vec<_> = JournalLines::new(&again, &log)
            .unwrap()
            .map(|line| line.unwrap().id)
            .collect();
        assert_eq!(all, ["a", "b", "c", "d"]);
    }

    #[test]
    fn a_torn_tail_or_a_broken_line_ends_the_walk_with_the_segment_path() {
        let scratch = Scratch::new("torn");
        let log = scratch.declare("log", "  journal: {}\n");
        scratch.write("log/00000001-w.jsonl", b"a: 1\nb: 2");
        let snap = scratch.snap(&log);
        let mut walk = JournalLines::new(&snap, &log).unwrap();
        assert_eq!(walk.next().unwrap().unwrap().id, "a");
        let fault = walk.next().unwrap().unwrap_err();
        assert_eq!(fault.path(), Some("log/00000001-w.jsonl"));
        assert!(invalid(fault));
        assert!(walk.next().is_none());
        scratch.write("log/00000001-w.jsonl", b"a: 1\nnonsense\nc: 3\n");
        let snap = scratch.snap(&log);
        let mut walk = JournalLines::new(&snap, &log).unwrap();
        assert_eq!(walk.next().unwrap().unwrap().id, "a");
        assert!(invalid(walk.next().unwrap().unwrap_err()));
        assert!(walk.next().is_none());
    }

    #[test]
    fn an_empty_or_missing_journal_yields_no_line() {
        let scratch = Scratch::new("empty");
        let log = scratch.declare("log", "  journal: {}\n");
        let snap = scratch.snap(&log);
        assert!(JournalLines::new(&snap, &log).unwrap().next().is_none());
        std::fs::remove_dir_all(scratch.0.join("log")).unwrap();
        let snap = scratch.snap(&log);
        assert!(JournalLines::new(&snap, &log).unwrap().next().is_none());
        assert!(list_segments(&snap, &log).unwrap().is_empty());
    }

    #[test]
    fn a_collection_that_is_not_a_journal_is_invalid_argument() {
        let scratch = Scratch::new("kind");
        let notes = scratch.declare("notes", "  standard: {}\n");
        let snap = scratch.snap(&notes);
        assert!(invalid(list_segments(&snap, &notes).unwrap_err()));
        assert!(invalid(Limits::of(&notes).unwrap_err()));
        assert!(JournalLines::new(&snap, &notes).is_err());
    }

    #[test]
    fn unix_epoch_is_1970_01_01() {
        assert_eq!(civil_from_unix_days(0), (1970, 1, 1));
        assert_eq!(civil_from_unix_days(19_782), (2024, 2, 29));
        let today = today_utc().unwrap();
        assert_eq!(today.len(), 10);
        assert_eq!(&today[4..5], "-");
    }

    #[test]
    fn segment_seq_is_eight_lowercase_hex() {
        assert_eq!(parse_hex_seq("00000001"), Some(1));
        assert_eq!(parse_hex_seq("0000000a"), Some(10));
        assert_eq!(parse_hex_seq("ffffffff"), Some(u32::MAX));
        assert_eq!(parse_hex_seq("0001"), None);
        assert_eq!(parse_hex_seq("+0000001"), None);
        assert_eq!(format!("{:08x}", 10u32), "0000000a");
    }

    #[test]
    fn segment_writer_parses_owned_and_writerless_names() {
        assert_eq!(segment_writer("00000001"), None);
        assert_eq!(segment_writer("2026-09-15"), None);
        assert_eq!(segment_writer("2026-09-15-00000002"), None);
        assert_eq!(segment_writer("00000001-alice"), Some("alice"));
        assert_eq!(segment_writer("2026-09-15-alice"), Some("alice"));
        assert_eq!(segment_writer("2026-09-15-00000001-alice"), Some("alice"));
        assert_eq!(segment_writer("00000001-host-1"), Some("host-1"));
    }

    #[test]
    fn a_hyphenated_writer_round_trips_in_all_segment_forms() {
        let today = "2026-09-15";
        for writer in ["host-1", "node-abcdef01", "a-b-c-1", "alice"] {
            let seq = next_segment_name(&[], false, writer, today).unwrap();
            assert_eq!(seq, format!("00000001-{writer}.jsonl"));
            assert_eq!(
                segment_writer(seq.strip_suffix(".jsonl").unwrap()),
                Some(writer)
            );
            assert!(segment_belongs_to(&PathBuf::from(&seq), writer));

            let daily = next_segment_name(&[], true, writer, today).unwrap();
            assert_eq!(daily, format!("{today}-{writer}.jsonl"));
            assert!(segment_belongs_to(&PathBuf::from(&daily), writer));

            let overflow =
                next_segment_name(&[PathBuf::from(&daily)], true, writer, today).unwrap();
            assert_eq!(overflow, format!("{today}-00000001-{writer}.jsonl"));
            assert!(segment_belongs_to(&PathBuf::from(&overflow), writer));
            let after = next_segment_name(
                &[PathBuf::from(&daily), PathBuf::from(&overflow)],
                true,
                writer,
                today,
            )
            .unwrap();
            assert_eq!(after, format!("{today}-00000002-{writer}.jsonl"));
        }
    }

    #[test]
    fn the_next_segment_counts_only_the_writers_own() {
        let existing = [
            PathBuf::from("00000003-bob.jsonl"),
            PathBuf::from("00000001-alice.jsonl"),
            PathBuf::from("00000002.jsonl"),
        ];
        assert_eq!(
            next_segment_name(&existing, false, "alice", "2026-09-15").unwrap(),
            "00000002-alice.jsonl"
        );
        assert_eq!(
            next_segment_name(&existing, false, "carol", "2026-09-15").unwrap(),
            "00000001-carol.jsonl"
        );
        let full = [PathBuf::from("ffffffff-alice.jsonl")];
        let fault = next_segment_name(&full, false, "alice", "2026-09-15").unwrap_err();
        assert!(Error::from(fault).is(VaultErrorCode::FailedPrecondition));
    }

    #[test]
    fn segment_order_is_seq_then_writer_then_daily_overflow() {
        let names_in = [
            "00000001-alice.jsonl",
            "00000001.jsonl",
            "00000002-alice.jsonl",
            "00000001-bob.jsonl",
            "2026-09-15-00000001-alice.jsonl",
            "2026-09-15-alice.jsonl",
            "2026-09-15.jsonl",
        ];
        let mut paths: Vec<_> = names_in.iter().map(PathBuf::from).collect();
        paths.sort_by_key(|a| segment_order(a));
        assert_eq!(
            names(&paths),
            [
                "00000001.jsonl",
                "00000001-alice.jsonl",
                "00000001-bob.jsonl",
                "00000002-alice.jsonl",
                "2026-09-15.jsonl",
                "2026-09-15-alice.jsonl",
                "2026-09-15-00000001-alice.jsonl",
            ]
        );
    }

    #[test]
    fn rotation_by_size_closes_a_segment_without_changing_it() {
        let scratch = Scratch::new("size");
        let log = scratch.declare("log", "  journal:\n    segmentMaxBytes: 40\n");
        let limits = Limits::of(&log).unwrap();
        let today = "2026-09-15";
        let first = PathBuf::from("log/00000001-w.jsonl");
        // No segment yet: the first one is named.
        let snap = scratch.snap(&log);
        assert_eq!(
            segment_for_append(&snap, &log, 10, "w", today).unwrap(),
            first
        );
        let line = encode_line("a", &[b'x'; 17]).unwrap();
        assert_eq!(line.len(), 21);
        scratch.write("log/00000001-w.jsonl", &line);
        let snap = scratch.snap(&log);
        assert!(!segment_is_full(&snap, &first, &limits, 19, today).unwrap());
        assert!(segment_is_full(&snap, &first, &limits, 20, today).unwrap());
        assert_eq!(
            segment_for_append(&snap, &log, 19, "w", today).unwrap(),
            first
        );
        let second = PathBuf::from("log/00000002-w.jsonl");
        assert_eq!(
            segment_for_append(&snap, &log, 20, "w", today).unwrap(),
            second
        );
        assert_eq!(std::fs::read(scratch.0.join(&first)).unwrap(), line);
        // Another writer starts its own sequence and never takes mine.
        let other = PathBuf::from("log/00000001-v.jsonl");
        assert_eq!(
            segment_for_append(&snap, &log, 1, "v", today).unwrap(),
            other
        );
        // An empty segment is never full, however long the line.
        scratch.write("log/00000001-w.jsonl", b"");
        let snap = scratch.snap(&log);
        assert!(!segment_is_full(&snap, &first, &limits, 1000, today).unwrap());
        assert!(
            !segment_is_full(
                &snap,
                &PathBuf::from("log/missing.jsonl"),
                &limits,
                1,
                today
            )
            .unwrap()
        );
    }

    #[test]
    fn rotation_by_line_count() {
        let scratch = Scratch::new("lines");
        let log = scratch.declare("log", "  journal:\n    segmentMaxLines: 2\n");
        let limits = Limits::of(&log).unwrap();
        assert_eq!(limits.max_lines, Some(2));
        let today = "2026-09-15";
        let first = PathBuf::from("log/00000001-w.jsonl");
        scratch.write("log/00000001-w.jsonl", b"a: 1\n");
        let snap = scratch.snap(&log);
        assert!(!segment_is_full(&snap, &first, &limits, 5, today).unwrap());
        scratch.write("log/00000001-w.jsonl", b"a: 1\nb: 2\n");
        let snap = scratch.snap(&log);
        assert!(segment_is_full(&snap, &first, &limits, 5, today).unwrap());
        assert_eq!(
            segment_for_append(&snap, &log, 5, "w", today).unwrap(),
            PathBuf::from("log/00000002-w.jsonl")
        );
    }

    #[test]
    fn daily_rotation_names_by_date_and_starts_a_new_segment_on_a_new_day() {
        let scratch = Scratch::new("daily");
        let log = scratch.declare("log", "  journal:\n    daily: {}\n");
        let limits = Limits::of(&log).unwrap();
        assert!(limits.daily);
        assert_eq!(
            limits.max_bytes,
            crate::collection::DEFAULT_SEGMENT_MAX_BYTES
        );
        let monday = PathBuf::from("log/2026-09-14-w.jsonl");
        let snap = scratch.snap(&log);
        assert_eq!(
            segment_for_append(&snap, &log, 5, "w", "2026-09-14").unwrap(),
            monday
        );
        scratch.write("log/2026-09-14-w.jsonl", b"a: 1\n");
        let snap = scratch.snap(&log);
        assert!(!segment_is_full(&snap, &monday, &limits, 5, "2026-09-14").unwrap());
        assert!(segment_is_full(&snap, &monday, &limits, 5, "2026-09-15").unwrap());
        assert_eq!(
            segment_for_append(&snap, &log, 5, "w", "2026-09-14").unwrap(),
            monday
        );
        assert_eq!(
            segment_for_append(&snap, &log, 5, "w", "2026-09-15").unwrap(),
            PathBuf::from("log/2026-09-15-w.jsonl")
        );
        // A sequence segment is not from today in a daily journal.
        let seq = PathBuf::from("log/00000001-w.jsonl");
        scratch.write("log/00000001-w.jsonl", b"a: 1\n");
        let snap = scratch.snap(&log);
        assert!(segment_is_full(&snap, &seq, &limits, 5, "2026-09-14").unwrap());
    }

    #[test]
    fn an_isolated_journal_lives_under_the_isolated_tree() {
        let scratch = Scratch::new("isolated");
        let log = scratch.declare("log", "  journal: {}\n  isolated: true\n");
        scratch.write(".isolated/log/00000001-w.jsonl", b"a: 1\n");
        scratch.write("log/00000001-w.jsonl", b"stray: 1\n");
        let snap = scratch.snap(&log);
        let lines: Vec<_> = JournalLines::new(&snap, &log)
            .unwrap()
            .map(|line| line.unwrap().id)
            .collect();
        assert_eq!(lines, ["a"]);
        assert_eq!(
            segment_for_append(&snap, &log, 5, "w", "2026-09-15").unwrap(),
            PathBuf::from(".isolated/log/00000001-w.jsonl")
        );
    }

    fn found(problems: &[scheda_api::v1::Problem]) -> Vec<(VaultErrorCode, String)> {
        problems
            .iter()
            .map(|p| {
                (
                    p.code.and_then(|code| code.as_known()).unwrap(),
                    p.path.clone().unwrap_or_default(),
                )
            })
            .collect()
    }

    #[test]
    fn the_check_passes_marker_lines_and_binary_payloads() {
        let scratch = Scratch::new("check-ok");
        scratch.declare("log", "  journal: {}\n");
        let mut bytes = b"a: <<<<<<< HEAD\nb: >>>>>>> other\n".to_vec();
        bytes.extend(encode_line("c", &[0xFF, 0xFE, 0x00]).unwrap());
        bytes.extend_from_slice(b"d: =======\n");
        scratch.write("log/00000001-w.jsonl", &bytes);
        let declarations = scratch.declarations();
        assert!(
            check_journals(&scratch.views(), &declarations)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn the_check_collects_every_problem_of_every_journal() {
        let scratch = Scratch::new("check-bad");
        scratch.declare("log", "  journal: {}\n");
        scratch.declare("hidden", "  journal: {}\n  isolated: true\n");
        scratch.write("log/00000001-w.jsonl", b"a: 1\nnonsense\nb: 2\n");
        scratch.write("log/00000002-w.jsonl", b"a: again\nc: 3\ntorn");
        scratch.write(".isolated/hidden/00000001-w.jsonl", b"x: 1\r\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            scratch.0.join("log/00000001-w.jsonl"),
            scratch.0.join("log/00000003-w.jsonl"),
        )
        .unwrap();
        let declarations = scratch.declarations();
        let problems = check_journals(&scratch.views(), &declarations).unwrap();
        let mut expected = vec![
            (
                VaultErrorCode::InvalidArgument,
                ".isolated/hidden/00000001-w.jsonl".to_owned(),
            ),
            (
                VaultErrorCode::InvalidArgument,
                "log/00000001-w.jsonl".to_owned(),
            ),
            (
                VaultErrorCode::AlreadyExists,
                "log/00000002-w.jsonl".to_owned(),
            ),
            (
                VaultErrorCode::InvalidArgument,
                "log/00000002-w.jsonl".to_owned(),
            ),
        ];
        if cfg!(unix) {
            expected.push((
                VaultErrorCode::InvalidArgument,
                "log/00000003-w.jsonl".to_owned(),
            ));
        }
        let mut got = found(&problems);
        got.sort_by(|a, b| (&a.1, a.0 as i32).cmp(&(&b.1, b.0 as i32)));
        expected.sort_by(|a, b| (&a.1, a.0 as i32).cmp(&(&b.1, b.0 as i32)));
        assert_eq!(got, expected);
    }
}
