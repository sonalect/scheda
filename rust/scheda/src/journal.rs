//! On-disk `journal` JSONL segments.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::iter::FusedIterator;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::error::{Code, ContextAs, ContextIo, bail};
use crate::index::{self, JournalIdMaps};
use crate::schema::{CollectionDecl, Kind, Schema, SegmentRotate};
use crate::store;
use crate::{Error, Record};

pub(crate) struct JournalRollback {
    pub(crate) path: PathBuf,
    pub(crate) previous_len: u64,
}

/// Append-order walk of one `journal` collection.
///
/// Yields one Record at a time from segment files. Does not open the Index.
/// `raw` is the line bytes including the trailing newline.
pub struct JournalLines {
    vault: PathBuf,
    decl: CollectionDecl,
    segments: Vec<PathBuf>,
    next_segment: usize,
    reader: Option<BufReader<File>>,
    current_path: PathBuf,
    buf: String,
    done: bool,
}

pub(crate) fn journal_lines(vault: &Path, decl: &CollectionDecl) -> Result<JournalLines, Error> {
    if decl.kind != Kind::Journal {
        bail!(Error::unsupported("lines is offered for journal only"));
    }
    let segments = list_segments(&journal_dir(vault, decl)?)?;
    Ok(JournalLines {
        vault: vault.to_path_buf(),
        decl: decl.clone(),
        segments,
        next_segment: 0,
        reader: None,
        current_path: PathBuf::new(),
        buf: String::new(),
        done: false,
    })
}

impl JournalLines {
    fn next_record(&mut self) -> Result<Option<Record>, Error> {
        loop {
            if self.reader.is_none() {
                let Some(path) = self.segments.get(self.next_segment) else {
                    return Ok(None);
                };
                self.next_segment += 1;
                self.current_path.clone_from(path);
                let rel = store::rel_posix(&self.vault, &self.current_path)?;
                let file = File::open(&self.current_path)
                    .context("[Journal::lines] open segment")
                    .map_err(|err| err.with_path(&rel))?;
                self.reader = Some(BufReader::new(file));
            }
            let rel = store::rel_posix(&self.vault, &self.current_path)?;
            let mut reader = match self.reader.take() {
                Some(reader) => reader,
                None => unreachable!("reader was just opened"),
            };
            self.buf.clear();
            let n = reader
                .read_line(&mut self.buf)
                .context("[Journal::lines] read line")
                .map_err(|err| err.with_path(&rel))?;
            if n == 0 {
                continue;
            }
            self.reader = Some(reader);
            let Some(line) = self.buf.strip_suffix('\n') else {
                bail!(Error::invalid(format!(
                    "{rel} has a truncated line without a trailing newline"
                )));
            };
            if store::has_conflict_markers(line) {
                bail!(Error::conflict_markers(format!(
                    "{rel} has git conflict markers"
                )));
            }
            let parsed = parse_line(line).map_err(|err| {
                Error::invalid(format!("{rel} has a line that is not a JSON object: {err}"))
            })?;
            return Ok(Some(record_from_parsed(
                &self.vault,
                &self.decl,
                &self.current_path,
                parsed,
            )?));
        }
    }
}

impl Iterator for JournalLines {
    type Item = Result<Record, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_record() {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(err) => {
                self.done = true;
                Some(Err(err))
            }
        }
    }
}

impl FusedIterator for JournalLines {}

pub(crate) fn rollback_insert(write: &JournalRollback) -> Result<(), Error> {
    if write.previous_len == 0 {
        match fs::remove_file(&write.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).context("[Journal::rollback] remove new segment"),
        }
    } else {
        let file = OpenOptions::new()
            .write(true)
            .open(&write.path)
            .context("[Journal::rollback] open segment")?;
        file.set_len(write.previous_len)
            .context("[Journal::rollback] truncate segment")?;
        file.sync_all().context("[Journal::rollback] fsync segment")
    }
}

pub(crate) fn insert_journal(
    vault: &Path,
    decl: &CollectionDecl,
    record: &Record,
    index_dir: &Path,
    maps: &mut JournalIdMaps,
    writer: &str,
) -> Result<(String, String, JournalRollback), Error> {
    let (id, line) = prepare_line(decl, record)?;
    match maps.contains(&decl.name, &id) {
        Some(true) => {
            bail!(Error::duplicate_id(format!(
                "record {id} already exists in {}",
                decl.name
            )));
        }
        Some(false) => {}
        None => {
            if id_exists(vault, decl, &id)? {
                bail!(Error::duplicate_id(format!(
                    "record {id} already exists in {}",
                    decl.name
                )));
            }
        }
    }
    let dir = journal_dir(vault, decl)?;
    fs::create_dir_all(&dir).context("[Journal::insert] create dir")?;
    crate::isolated::restrict_record(vault, &dir)?;
    let path = open_segment_path(&dir, decl, line.len() as u64, maps, writer)?;
    let previous_len = if path.is_file() {
        fs::metadata(&path)
            .context("[Journal::insert] stat segment")?
            .len()
    } else {
        0
    };
    append_line(&path, &line)?;
    crate::isolated::restrict_record(vault, &path)?;
    maps.add_segment_line(&path);
    let rel = store::rel_posix(vault, &path)?;
    if maps.contains(&decl.name, &id).is_some() {
        index::remember_id(index_dir, maps, &decl.name, &id, &rel)?;
    } else {
        let ids = map_from_files(vault, decl)?;
        index::rewrite_collection(index_dir, maps, &decl.name, ids)?;
    }
    Ok((id, rel, JournalRollback { path, previous_len }))
}

/// Scan segment files for `id`. Catch-up uses this when the live map is
/// missing, so a map that does not yet know the line still finds it on disk.
pub(crate) fn find_journal_scan(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
) -> Result<Record, Error> {
    let mut found = None;
    for_each_parsed(vault, decl, |path, parsed| {
        if parsed.id == id {
            found = Some(record_from_parsed(vault, decl, path, parsed)?);
            Ok(false)
        } else {
            Ok(true)
        }
    })?;
    found.ok_or_else(|| Error::not_found(format!("record {id} is not in {}", decl.name)))
}

pub(crate) fn find_journal(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
    maps: &JournalIdMaps,
) -> Result<Record, Error> {
    if id.is_empty() {
        bail!(Error::invalid("id is empty"));
    }
    match maps.lookup(&decl.name, id) {
        index::MapLookup::UnknownId => Err(Error::not_found(format!(
            "record {id} is not in {}",
            decl.name
        ))),
        index::MapLookup::Path(rel) => find_in_segment(vault, decl, rel, id),
        index::MapLookup::MissingMap => find_journal_scan(vault, decl, id),
    }
}

fn resolve_segment(vault: &Path, decl: &CollectionDecl, rel: &str) -> Result<PathBuf, Error> {
    if !index::id_map_rel_ok(decl, rel) {
        bail!(
            Error::index_corrupt("journal id map path is not a collection segment")
                .context("[Journal::find] resolve segment")
        );
    }
    let path = crate::schema::join_inside(vault, rel).map_err(|_| {
        Error::index_corrupt("journal id map path is not a collection segment")
            .context("[Journal::find] resolve segment")
    })?;
    let dir = crate::isolated::records_dir(vault, decl)?;
    if !path.starts_with(&dir) {
        bail!(
            Error::index_corrupt("journal id map path is not a collection segment")
                .context("[Journal::find] resolve segment")
        );
    }
    Ok(path)
}

pub(crate) fn find_in_segment(
    vault: &Path,
    decl: &CollectionDecl,
    rel: &str,
    id: &str,
) -> Result<Record, Error> {
    let path = resolve_segment(vault, decl, rel)?;
    let raw = fs::read(&path)
        .context("[Journal::find] read segment")
        .map_err(|err| err.with_path(rel))?;
    for parsed in parse_segment_bytes(&raw) {
        if parsed.id == id {
            return record_from_parsed(vault, decl, &path, parsed);
        }
    }
    Err(Error::not_found(format!(
        "record {id} is not in {}",
        decl.name
    )))
}

/// Number of segment files in Schema sequence. Missing directory → 0.
#[cfg(feature = "find")]
pub(crate) fn segment_count(vault: &Path, decl: &CollectionDecl) -> Result<u64, Error> {
    Ok(list_segments(&journal_dir(vault, decl)?)?.len() as u64)
}

/// Walk every parsed line as a Record, one at a time. `visit` returns
/// `false` to stop. Does not collect the collection in memory.
#[cfg(feature = "find")]
pub(crate) fn for_each_record(
    vault: &Path,
    decl: &CollectionDecl,
    mut visit: impl FnMut(Record) -> Result<bool, Error>,
) -> Result<(), Error> {
    for_each_parsed(vault, decl, |path, parsed| {
        visit(record_from_parsed(vault, decl, path, parsed)?)
    })
}

pub(crate) fn check_journals(vault: &Path, schema: &Schema) -> Result<(), Error> {
    for decl in schema.collections() {
        if decl.kind != Kind::Journal {
            continue;
        }
        let mut seen = BTreeSet::new();
        for path in list_segments(&journal_dir(vault, decl)?)? {
            let rel = store::rel_posix(vault, &path)?;
            let raw = fs::read(&path)
                .context("[Journal::check] read segment")
                .map_err(|err| err.with_path(&rel))?;
            let text = str::from_utf8(&raw)
                .context(Code::NotUtf8, "[Journal::check] decode UTF-8")
                .map_err(|err| err.with_path(&rel))?;
            if store::has_conflict_markers(text) {
                bail!(Error::conflict_markers(format!(
                    "{rel} has git conflict markers"
                )));
            }
            let (complete, tail) = split_complete(text);
            if tail.is_some() {
                bail!(Error::invalid(format!(
                    "{rel} has a truncated line without a trailing newline"
                )));
            }
            for line in complete {
                let parsed = parse_line(line).map_err(|err| {
                    Error::invalid(format!("{rel} has a line that is not a JSON object: {err}"))
                })?;
                if !seen.insert(parsed.id.clone()) {
                    bail!(Error::duplicate_id(format!(
                        "duplicate id {} in {}",
                        parsed.id, decl.name
                    )));
                }
                if let Err(err) = store::validate_fields(&parsed.fields, &decl.fields, true, false)
                {
                    bail!(Error::invalid(format!("{rel}: {err}")));
                }
            }
        }
    }
    Ok(())
}

struct ParsedLine {
    id: String,
    fields: Map<String, Value>,
    raw: Vec<u8>,
}

fn prepare_line(decl: &CollectionDecl, record: &Record) -> Result<(String, Vec<u8>), Error> {
    let Some(id) = record.id.as_deref() else {
        bail!(Error::invalid("record id is required"));
    };
    crate::id::require_slug("id", id)?;
    if record.body.as_deref().is_some_and(|body| !body.is_empty()) {
        bail!(Error::invalid(format!(
            "journal {} does not allow a Markdown body",
            decl.name
        )));
    }
    let mut fields = store::fields_map(record)?;
    store::apply_defaults(&mut fields, &decl.fields);
    match fields.get(crate::id::FIELD) {
        Some(Value::String(existing)) if existing == id => {}
        Some(Value::Null) | None => {
            fields.insert(String::from(crate::id::FIELD), Value::String(id.to_owned()));
        }
        Some(_) => {
            bail!(Error::invalid(format!(
                "object {} must equal record id {id}",
                crate::id::FIELD
            )));
        }
    }
    store::validate_fields(&fields, &decl.fields, true, false)?;
    let mut line = canonical_json(&Value::Object(fields))?;
    line.push(b'\n');
    Ok((id.to_owned(), line))
}

fn canonical_json(value: &Value) -> Result<Vec<u8>, Error> {
    let mut out = String::new();
    write_canonical(value, &mut out)?;
    Ok(out.into_bytes())
}

fn write_canonical(value: &Value, out: &mut String) -> Result<(), Error> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            let Some(number) = n.as_f64() else {
                bail!(Error::invalid("JSON number must be finite"));
            };
            write_es6_number(number, out)?;
        }
        Value::String(s) => write_json_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(key, out);
                out.push(':');
                let Some(value) = map.get(*key) else {
                    unreachable!("key came from the same map");
                };
                write_canonical(value, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// ECMAScript `NumberToString` as required by RFC 8785. `-0` is `0`.
fn write_es6_number(n: f64, out: &mut String) -> Result<(), Error> {
    if !n.is_finite() {
        bail!(Error::invalid("JSON number must be finite"));
    }
    if n == 0.0 {
        out.push('0');
        return Ok(());
    }
    if n < 0.0 {
        out.push('-');
    }
    let abs = n.abs();
    if (1e-6..1e21).contains(&abs) {
        write!(out, "{abs}").map_err(|_| Error::invalid("cannot format JSON number"))?;
    } else {
        write_es6_scientific(abs, out);
    }
    Ok(())
}

fn write_es6_scientific(abs: f64, out: &mut String) {
    let sci = format!("{abs:e}");
    let Some((mant, exp)) = sci.split_once('e') else {
        out.push_str(&sci);
        return;
    };
    out.push_str(mant);
    out.push('e');
    if let Some(rest) = exp.strip_prefix('-') {
        out.push('-');
        out.push_str(strip_leading_zeros(rest));
    } else {
        out.push('+');
        let rest = exp.strip_prefix('+').unwrap_or(exp);
        out.push_str(strip_leading_zeros(rest));
    }
}

fn strip_leading_zeros(s: &str) -> &str {
    let trimmed = s.trim_start_matches('0');
    if trimmed.is_empty() { "0" } else { trimmed }
}

/// RFC 8785 string: only `"`, `\`, and U+0000–U+001F are escaped. UTF-8
/// `str` cannot hold a lone surrogate, which the RFC would reject.
fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) <= 0x1F => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let n = c as u32;
                out.push_str("\\u00");
                out.push(HEX[(n >> 4) as usize] as char);
                out.push(HEX[(n & 0xF) as usize] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn journal_dir(vault: &Path, decl: &CollectionDecl) -> Result<PathBuf, Error> {
    crate::isolated::records_dir(vault, decl)
}

fn list_segments(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    if !dir.is_dir() {
        bail!(Error::invalid(format!(
            "journal layout is not a directory: {}",
            dir.display()
        )));
    }
    let mut out = Vec::new();
    let entries = fs::read_dir(dir).context("[Journal::list_segments] read dir")?;
    for entry in entries {
        let entry = entry.context("[Journal::list_segments] read entry")?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') || !name.ends_with(".jsonl") || !path.is_file() {
            continue;
        }
        out.push(path);
    }
    out.sort_by_cached_key(|path| segment_order(path));
    Ok(out)
}

/// Sequence files (`00000001…`) before daily (`YYYY-MM-DD…`), then sequence
/// number (daily overflow after the first file of that date), then writer
/// (writerless names first so older vaults keep append order), then the
/// file name as a last tie-break. Lexicographic file names put
/// `{seq}-{writer}` before a writerless `{seq}` because `-` sorts before `.`.
fn segment_order(path: &Path) -> (u8, String, u32, String, String) {
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

fn open_segment_path(
    dir: &Path,
    decl: &CollectionDecl,
    line_len: u64,
    maps: &mut JournalIdMaps,
    writer: &str,
) -> Result<PathBuf, Error> {
    let segments = list_segments(dir)?;
    let mine: Vec<PathBuf> = segments
        .iter()
        .filter(|path| segment_belongs_to(path, writer))
        .cloned()
        .collect();
    if let Some(last) = mine.last()
        && !segment_is_full(last, decl, line_len, maps)?
    {
        return Ok(last.clone());
    }
    Ok(dir.join(next_segment_name(&mine, decl, writer)?))
}

fn segment_is_full(
    path: &Path,
    decl: &CollectionDecl,
    line_len: u64,
    maps: &mut JournalIdMaps,
) -> Result<bool, Error> {
    if !path.exists() {
        return Ok(false);
    }
    let meta = fs::metadata(path).context("[Journal::segment_is_full] stat segment")?;
    let size = meta.len();
    if size == 0 {
        return Ok(false);
    }
    if decl.segment_rotate == Some(SegmentRotate::Daily) {
        let today = utc_ymd(SystemTime::now())?;
        if segment_date(path).is_none_or(|date| date != today) {
            return Ok(true);
        }
    }
    if let Some(cap) = decl.segment_max_bytes
        && size.saturating_add(line_len) > cap
    {
        return Ok(true);
    }
    if let Some(max_lines) = decl.segment_max_lines {
        let lines = maps.ensure_segment_lines(path, count_newlines)?;
        if lines >= max_lines {
            return Ok(true);
        }
    }
    Ok(false)
}

fn parse_hex_seq(stem: &str) -> Option<u32> {
    if stem.len() != 8 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(stem, 16).ok()
}

fn next_segment_name(
    existing: &[PathBuf],
    decl: &CollectionDecl,
    writer: &str,
) -> Result<String, Error> {
    if decl.segment_rotate == Some(SegmentRotate::Daily) {
        let today = utc_ymd(SystemTime::now())?;
        let mut seq = 0u32;
        let mut has_plain = false;
        let prefix = format!("{today}-");
        let writer_suffix = format!("-{writer}");
        let plain = format!("{today}-{writer}");
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
        let next = seq
            .checked_add(1)
            .ok_or_else(|| Error::invalid("journal segment sequence exhausted"))?;
        return Ok(format!("{today}-{next:08x}-{writer}.jsonl"));
    }
    let mut max = 0u32;
    let writer_suffix = format!("-{writer}");
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
    let next = max
        .checked_add(1)
        .ok_or_else(|| Error::invalid("journal segment sequence exhausted"))?;
    Ok(format!("{next:08x}-{writer}.jsonl"))
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
    if stem.len() >= 10 && stem.as_bytes()[4] == b'-' && stem.as_bytes()[7] == b'-' {
        Some(&stem[..10])
    } else {
        None
    }
}

fn utc_ymd(now: SystemTime) -> Result<String, Error> {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .context(Code::IoOther, "[Journal::utc_ymd] clock")?
        .as_secs();
    let (year, month, day) = civil_from_unix_days((secs / 86_400) as i32);
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

/// Civil date from days since Unix epoch (Howard Hinnant).
fn civil_from_unix_days(days: i32) -> (i32, u32, u32) {
    let z = i64::from(days) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + i64::from(m <= 2);
    (y as i32, m, d)
}

fn append_line(path: &Path, line: &[u8]) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .context("[Journal::append_line] open segment")?;
    file.write_all(line)
        .context("[Journal::append_line] write line")?;
    file.sync_all()
        .context("[Journal::append_line] fsync segment")?;
    Ok(())
}

fn count_newlines(path: &Path) -> Result<u64, Error> {
    use std::io::Read;
    let mut file = fs::File::open(path).context("[Journal::count_newlines] open segment")?;
    let mut buf = [0u8; 8192];
    let mut lines = 0u64;
    loop {
        let n = file
            .read(&mut buf)
            .context("[Journal::count_newlines] read segment")?;
        if n == 0 {
            break;
        }
        lines += buf[..n].iter().filter(|byte| **byte == b'\n').count() as u64;
    }
    Ok(lines)
}

fn id_exists(vault: &Path, decl: &CollectionDecl, id: &str) -> Result<bool, Error> {
    let mut found = false;
    for_each_parsed(vault, decl, |_, parsed| {
        if parsed.id == id {
            found = true;
            Ok(false)
        } else {
            Ok(true)
        }
    })?;
    Ok(found)
}

fn for_each_parsed(
    vault: &Path,
    decl: &CollectionDecl,
    mut visit: impl FnMut(&Path, ParsedLine) -> Result<bool, Error>,
) -> Result<(), Error> {
    for path in list_segments(&journal_dir(vault, decl)?)? {
        for parsed in parse_segment(&path)? {
            if !visit(&path, parsed)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

pub(crate) fn reindex_map(
    vault: &Path,
    decl: &CollectionDecl,
) -> Result<(u64, u64, u64, BTreeMap<String, String>), Error> {
    let segments = list_segments(&journal_dir(vault, decl)?)?.len() as u64;
    let mut records = 0u64;
    let mut invalid = 0u64;
    let mut ids = std::collections::BTreeMap::new();
    for_each_parsed(vault, decl, |path, parsed| {
        records += 1;
        let record = record_from_parsed(vault, decl, path, parsed)?;
        if record.valid != Some(true) {
            invalid += 1;
        }
        if let (Some(id), Some(rel)) = (record.id, record.path) {
            ids.insert(id, rel);
        }
        Ok(true)
    })?;
    Ok((records, invalid, segments, ids))
}

fn map_from_files(vault: &Path, decl: &CollectionDecl) -> Result<BTreeMap<String, String>, Error> {
    let mut ids = std::collections::BTreeMap::new();
    for_each_parsed(vault, decl, |path, parsed| {
        let rel = store::rel_posix(vault, path)?;
        ids.insert(parsed.id, rel);
        Ok(true)
    })?;
    Ok(ids)
}

/// Record `id`s in one journal segment file. Missing file → empty.
pub(crate) fn ids_in_segment(path: &Path) -> Result<Vec<String>, Error> {
    Ok(parse_segment(path)?
        .into_iter()
        .map(|parsed| parsed.id)
        .collect())
}

fn parse_segment(path: &Path) -> Result<Vec<ParsedLine>, Error> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).context("[Journal::parse_segment] read segment"),
    };
    Ok(parse_segment_bytes(&raw))
}

fn parse_segment_bytes(raw: &[u8]) -> Vec<ParsedLine> {
    let Ok(text) = str::from_utf8(raw) else {
        return Vec::new();
    };
    if store::has_conflict_markers(text) {
        return Vec::new();
    }
    let (complete, _) = split_complete(text);
    let mut out = Vec::new();
    for line in complete {
        match parse_line(line) {
            Ok(parsed) => out.push(parsed),
            Err(_) => continue,
        }
    }
    out
}

fn split_complete(text: &str) -> (Vec<&str>, Option<&str>) {
    if text.is_empty() {
        return (Vec::new(), None);
    }
    if let Some(stripped) = text.strip_suffix('\n') {
        let lines: Vec<&str> = if stripped.is_empty() {
            Vec::new()
        } else {
            stripped.split('\n').collect()
        };
        return (lines, None);
    }
    match text.rsplit_once('\n') {
        Some((head, tail)) => {
            let lines = if head.is_empty() {
                Vec::new()
            } else {
                head.split('\n').collect()
            };
            (lines, Some(tail))
        }
        None => (Vec::new(), Some(text)),
    }
}

fn parse_line(line: &str) -> Result<ParsedLine, Error> {
    if line.is_empty() {
        bail!(Error::invalid("empty journal line"));
    }
    let value: Value =
        serde_json::from_str(line).context(Code::Invalid, "[Journal::parse_line] decode JSON")?;
    let Value::Object(fields) = value else {
        bail!(Error::invalid("journal line must be a JSON object"));
    };
    let id = match fields.get(crate::id::FIELD) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            bail!(Error::invalid("journal object id is missing"));
        }
    };
    let mut raw = line.as_bytes().to_vec();
    raw.push(b'\n');
    Ok(ParsedLine { id, fields, raw })
}

fn record_from_parsed(
    vault: &Path,
    decl: &CollectionDecl,
    path: &Path,
    parsed: ParsedLine,
) -> Result<Record, Error> {
    let rel = store::rel_posix(vault, path)?;
    let valid = store::validate_fields(&parsed.fields, &decl.fields, true, false).is_ok();
    let mut record = Record::default()
        .with_id(parsed.id)
        .with_collection(decl.name.as_str())
        .with_path(rel)
        .with_valid(valid)
        .with_raw(parsed.raw);
    store::put_fields(&mut record, parsed.fields)?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::{canonical_json, civil_from_unix_days, write_canonical};
    use serde_json::{Number, Value, json};
    use std::path::PathBuf;

    #[test]
    fn canonical_json_sorts_object_keys() {
        let bytes = canonical_json(&json!({"b": 1, "a": 2})).unwrap();
        assert_eq!(bytes, br#"{"a":2,"b":1}"#);
    }

    #[test]
    fn unix_epoch_is_1970_01_01() {
        assert_eq!(civil_from_unix_days(0), (1970, 1, 1));
        assert_eq!(civil_from_unix_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn write_canonical_compact_array() {
        let mut out = String::new();
        write_canonical(&json!([true, null, "x"]), &mut out).unwrap();
        assert_eq!(out, r#"[true,null,"x"]"#);
    }

    #[test]
    fn rfc8785_float_one_prints_as_integer() {
        let n = Number::from_f64(1.0).unwrap();
        assert_eq!(canonical_json(&Value::Number(n)).unwrap(), b"1");
    }

    #[test]
    fn rfc8785_minus_zero_prints_as_zero() {
        let n = Number::from_f64(-0.0).unwrap();
        assert_eq!(canonical_json(&Value::Number(n)).unwrap(), b"0");
    }

    #[test]
    fn rfc8785_sample_object() {
        let mut string = String::from("€$");
        string.push('\u{000F}');
        string.push('\n');
        string.push_str("A'B");
        string.push('"');
        string.push('\\');
        string.push('\\');
        string.push('"');
        string.push('/');
        let value = json!({
            "numbers": [333_333_333.333_333_3, 1e30, 4.50, 2e-3, 1e-27],
            "string": string,
            "literals": [null, true, false],
        });
        let bytes = canonical_json(&value).unwrap();
        let expected = "{\"literals\":[null,true,false],\"numbers\":[333333333.3333333,1e+30,4.5,0.002,1e-27],\"string\":\"€$\\u000f\\nA'B\\\"\\\\\\\\\\\"/\"}";
        assert_eq!(String::from_utf8(bytes).unwrap(), expected);
    }

    #[test]
    fn rfc8785_keys_sort_by_utf16_code_unit() {
        let value = json!({
            "\u{FB33}": 1,
            "\u{1F600}": 2,
        });
        let bytes = canonical_json(&value).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let grin = text.find('\u{1F600}').unwrap();
        let hebrew = text.find('\u{FB33}').unwrap();
        assert!(grin < hebrew, "{text}");
    }

    #[test]
    fn segment_seq_is_eight_lowercase_hex() {
        assert_eq!(super::parse_hex_seq("00000001"), Some(1));
        assert_eq!(super::parse_hex_seq("0000000a"), Some(10));
        assert_eq!(super::parse_hex_seq("ffffffff"), Some(u32::MAX));
        assert_eq!(super::parse_hex_seq("0001"), None);
        assert_eq!(super::parse_hex_seq("+0000001"), None);
        assert_eq!(format!("{:08x}", 10u32), "0000000a");
    }

    #[test]
    fn segment_writer_parses_owned_and_writerless_names() {
        assert_eq!(super::segment_writer("00000001"), None);
        assert_eq!(super::segment_writer("2026-09-15"), None);
        assert_eq!(super::segment_writer("2026-09-15-00000002"), None);
        assert_eq!(super::segment_writer("00000001-alice"), Some("alice"));
        assert_eq!(super::segment_writer("2026-09-15-alice"), Some("alice"));
        assert_eq!(
            super::segment_writer("2026-09-15-00000001-alice"),
            Some("alice")
        );
        assert_eq!(super::segment_writer("00000001-host-1"), Some("host-1"));
    }

    #[test]
    fn hyphenated_writer_round_trips_in_all_segment_forms() {
        for writer in ["host-1", "node-abcdef01", "a-b-c-1"] {
            crate::writer::validate(writer).unwrap();
            assert_writer_name_round_trip(writer);
        }
    }

    fn journal_decl(daily: bool) -> crate::schema::CollectionDecl {
        crate::schema::CollectionDecl {
            name: String::from("log"),
            kind: crate::schema::Kind::Journal,
            version: 1,
            filter_keys: Default::default(),
            segment_max_bytes: None,
            segment_max_lines: None,
            segment_rotate: daily.then_some(crate::schema::SegmentRotate::Daily),
            singleton_id: None,
            body: None,
            fields: Vec::new(),
            isolated: false,
            encrypted: false,
            exclude_lexical_index: false,
            exclude_vector_index: false,
        }
    }

    fn stem_of(name: &str) -> &str {
        name.strip_suffix(".jsonl").unwrap_or(name)
    }

    fn assert_writer_name_round_trip(writer: &str) {
        let seq = super::next_segment_name(&[], &journal_decl(false), writer).unwrap();
        let seq_path = PathBuf::from(&seq);
        assert_eq!(super::segment_writer(stem_of(&seq)), Some(writer), "{seq}");
        assert!(
            super::segment_belongs_to(&seq_path, writer),
            "{seq} belongs to {writer}"
        );

        let daily = super::next_segment_name(&[], &journal_decl(true), writer).unwrap();
        let daily_path = PathBuf::from(&daily);
        assert_eq!(
            super::segment_writer(stem_of(&daily)),
            Some(writer),
            "{daily}"
        );
        assert!(
            super::segment_belongs_to(&daily_path, writer),
            "{daily} belongs to {writer}"
        );

        let overflow = super::next_segment_name(
            std::slice::from_ref(&daily_path),
            &journal_decl(true),
            writer,
        )
        .unwrap();
        let overflow_path = PathBuf::from(&overflow);
        assert_ne!(overflow, daily);
        assert_eq!(
            super::segment_writer(stem_of(&overflow)),
            Some(writer),
            "{overflow}"
        );
        assert!(
            super::segment_belongs_to(&overflow_path, writer),
            "{overflow} belongs to {writer}"
        );
    }

    #[test]
    fn segment_order_is_seq_then_writer_then_daily_overflow() {
        let names = [
            "00000001-alice.jsonl",
            "00000001.jsonl",
            "00000002-alice.jsonl",
            "00000001-bob.jsonl",
            "2026-09-15-00000001-alice.jsonl",
            "2026-09-15-alice.jsonl",
            "2026-09-15.jsonl",
        ];
        let mut paths: Vec<_> = names.iter().map(PathBuf::from).collect();
        paths.sort_by_key(|a| super::segment_order(a));
        let ordered: Vec<_> = paths
            .iter()
            .filter_map(|p| p.file_name()?.to_str())
            .collect();
        assert_eq!(
            ordered,
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
}
