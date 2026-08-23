//! On-disk `journal` JSONL segments (C4).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::schema::{self, CollectionDecl, Kind, Schema, SegmentRotate};
use crate::store;
use crate::{Error, Record};

pub(crate) fn insert_journal(
    vault: &Path,
    decl: &CollectionDecl,
    record: Record,
) -> Result<String, Error> {
    let (id, line) = prepare_line(decl, record)?;
    if id_exists(vault, decl, &id)? {
        return Err(Error::conflict(format!(
            "record {id} already exists in {}",
            decl.name
        )));
    }
    let dir = journal_dir(vault, decl)?;
    fs::create_dir_all(&dir)
        .map_err(|err| Error::io(format!("create {}: {err}", dir.display())))?;
    let path = open_segment_path(&dir, decl, line.len() as u64)?;
    append_line(&path, &line)?;
    Ok(id)
}

pub(crate) fn find_journal(vault: &Path, decl: &CollectionDecl, id: &str) -> Result<Record, Error> {
    if id.is_empty() {
        return Err(Error::invalid("id is empty"));
    }
    for (path, parsed) in scan_parsed(vault, decl)? {
        if parsed.id == id {
            return record_from_parsed(vault, decl, &path, parsed);
        }
    }
    Err(Error::not_found(format!(
        "record {id} is not in {}",
        decl.name
    )))
}

pub(crate) fn scan_journal(vault: &Path, decl: &CollectionDecl) -> Result<Vec<Record>, Error> {
    let mut out = Vec::new();
    for (path, parsed) in scan_parsed(vault, decl)? {
        out.push(record_from_parsed(vault, decl, &path, parsed)?);
    }
    Ok(out)
}

pub(crate) fn check_journals(vault: &Path, schema: &Schema) -> Result<(), Error> {
    for decl in schema.collections() {
        if decl.kind != Kind::Journal {
            continue;
        }
        let mut seen = std::collections::BTreeSet::new();
        for path in list_segments(&journal_dir(vault, decl)?)? {
            let rel = store::rel_posix(vault, &path)?;
            let raw = fs::read(&path)
                .map_err(|err| Error::io(format!("read {}: {err}", path.display())))?;
            let text =
                str::from_utf8(&raw).map_err(|_| Error::invalid(format!("{rel} is not UTF-8")))?;
            if store::has_conflict_markers(text) {
                return Err(Error::conflict(format!("{rel} has git conflict markers")));
            }
            let (complete, tail) = split_complete(text);
            if tail.is_some() {
                return Err(Error::invalid(format!(
                    "{rel} has a truncated line without a trailing newline"
                )));
            }
            for line in complete {
                let parsed = parse_line(decl, line).map_err(|err| {
                    Error::invalid(format!("{rel} has a line that is not a JSON object: {err}"))
                })?;
                if !seen.insert(parsed.id.clone()) {
                    return Err(Error::conflict(format!(
                        "duplicate id {} in {}",
                        parsed.id, decl.name
                    )));
                }
                if let Err(err) = store::validate_fields(
                    &parsed.fields,
                    &decl.fields,
                    Some(decl.id_field.as_str()),
                    false,
                ) {
                    return Err(Error::invalid(format!("{rel}: {err}")));
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

fn prepare_line(decl: &CollectionDecl, record: Record) -> Result<(String, Vec<u8>), Error> {
    let id = record
        .id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::invalid("id is empty"))?
        .to_owned();
    let collection = record
        .collection
        .as_deref()
        .ok_or_else(|| Error::invalid("collection is empty"))?;
    if collection != decl.name {
        return Err(Error::invalid(format!(
            "record collection {collection} does not match {}",
            decl.name
        )));
    }
    if record.body.as_deref().is_some_and(|body| !body.is_empty()) {
        return Err(Error::invalid(format!(
            "journal {} does not allow a Markdown body",
            decl.name
        )));
    }
    if let Some(pattern) = &decl.id_pattern
        && !pattern.is_match(&id)
    {
        return Err(Error::invalid(format!(
            "id {id} does not match collection {} pattern",
            decl.name
        )));
    }
    if id.contains('/') || id.contains('\\') {
        return Err(Error::invalid("id must not contain a path separator"));
    }
    let mut fields = store::fields_map(&record)?;
    store::apply_defaults(&mut fields, &decl.fields);
    match fields.get(&decl.id_field) {
        Some(Value::String(existing)) if existing == &id => {}
        Some(Value::Null) | None => {
            fields.insert(decl.id_field.clone(), Value::String(id.clone()));
        }
        Some(_) => {
            return Err(Error::invalid(format!(
                "object {} must equal record id {id}",
                decl.id_field
            )));
        }
    }
    store::validate_fields(&fields, &decl.fields, Some(decl.id_field.as_str()), false)?;
    let mut line = canonical_json(&Value::Object(fields.clone()))?;
    line.push(b'\n');
    Ok((id, line))
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
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => {
            let encoded = serde_json::to_string(s)
                .map_err(|err| Error::invalid(format!("encode JSON string: {err}")))?;
            out.push_str(&encoded);
        }
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
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let encoded = serde_json::to_string(key)
                    .map_err(|err| Error::invalid(format!("encode JSON key: {err}")))?;
                out.push_str(&encoded);
                out.push(':');
                write_canonical(&map[key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn journal_dir(vault: &Path, decl: &CollectionDecl) -> Result<PathBuf, Error> {
    let rel = decl
        .layout
        .first()
        .map(String::as_str)
        .ok_or_else(|| Error::invalid(format!("collection {} is missing layout", decl.name)))?;
    let rel = rel.trim_end_matches('/');
    if rel.is_empty() {
        return Err(Error::invalid(format!(
            "collection {} layout is empty",
            decl.name
        )));
    }
    schema::join_in_collection(vault, &decl.name, rel)
}

fn list_segments(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    if !dir.is_dir() {
        return Err(Error::invalid(format!(
            "journal layout is not a directory: {}",
            dir.display()
        )));
    }
    let mut out = Vec::new();
    let entries =
        fs::read_dir(dir).map_err(|err| Error::io(format!("read {}: {err}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|err| Error::io(format!("read {}: {err}", dir.display())))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') || !name.ends_with(".jsonl") || !path.is_file() {
            continue;
        }
        out.push(path);
    }
    out.sort();
    Ok(out)
}

fn open_segment_path(dir: &Path, decl: &CollectionDecl, line_len: u64) -> Result<PathBuf, Error> {
    let segments = list_segments(dir)?;
    if let Some(last) = segments.last()
        && !segment_is_full(last, decl, line_len)?
    {
        return Ok(last.clone());
    }
    Ok(dir.join(next_segment_name(&segments, decl)?))
}

fn segment_is_full(path: &Path, decl: &CollectionDecl, line_len: u64) -> Result<bool, Error> {
    if !path.exists() {
        return Ok(false);
    }
    let meta =
        fs::metadata(path).map_err(|err| Error::io(format!("stat {}: {err}", path.display())))?;
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
        let text = fs::read_to_string(path)
            .map_err(|err| Error::io(format!("read {}: {err}", path.display())))?;
        let lines = text.bytes().filter(|b| *b == b'\n').count() as u64;
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

fn next_segment_name(existing: &[PathBuf], decl: &CollectionDecl) -> Result<String, Error> {
    if decl.segment_rotate == Some(SegmentRotate::Daily) {
        let today = utc_ymd(SystemTime::now())?;
        let mut seq = 0u32;
        for path in existing {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem == today {
                seq = seq.max(1);
            } else if let Some(rest) = stem.strip_prefix(&format!("{today}-"))
                && let Some(n) = parse_hex_seq(rest)
            {
                seq = seq.max(n);
            }
        }
        let next = seq
            .checked_add(1)
            .ok_or_else(|| Error::invalid("journal segment sequence exhausted"))?;
        return Ok(if seq == 0 {
            format!("{today}.jsonl")
        } else {
            format!("{today}-{next:08x}.jsonl")
        });
    }
    let mut max = 0u32;
    for path in existing {
        if let Some(n) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(parse_hex_seq)
        {
            max = max.max(n);
        }
    }
    let next = max
        .checked_add(1)
        .ok_or_else(|| Error::invalid("journal segment sequence exhausted"))?;
    Ok(format!("{next:08x}.jsonl"))
}

fn segment_date(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() >= 10 && stem.as_bytes()[4] == b'-' && stem.as_bytes()[7] == b'-' {
        Some(&stem[..10])
    } else {
        None
    }
}

fn utc_ymd(now: SystemTime) -> Result<String, Error> {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map_err(|err| Error::io(format!("clock: {err}")))?
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
        .map_err(|err| Error::io(format!("open {}: {err}", path.display())))?;
    file.write_all(line)
        .map_err(|err| Error::io(format!("write {}: {err}", path.display())))?;
    file.sync_all()
        .map_err(|err| Error::io(format!("fsync {}: {err}", path.display())))?;
    Ok(())
}

fn id_exists(vault: &Path, decl: &CollectionDecl, id: &str) -> Result<bool, Error> {
    for (_, parsed) in scan_parsed(vault, decl)? {
        if parsed.id == id {
            return Ok(true);
        }
    }
    Ok(false)
}

fn scan_parsed(vault: &Path, decl: &CollectionDecl) -> Result<Vec<(PathBuf, ParsedLine)>, Error> {
    let mut out = Vec::new();
    for path in list_segments(&journal_dir(vault, decl)?)? {
        let raw = match fs::read(&path) {
            Ok(raw) => raw,
            Err(err) => {
                return Err(Error::io(format!("read {}: {err}", path.display())));
            }
        };
        let Ok(text) = str::from_utf8(&raw) else {
            continue;
        };
        if store::has_conflict_markers(text) {
            continue;
        }
        let (complete, _) = split_complete(text);
        for line in complete {
            match parse_line(decl, line) {
                Ok(parsed) => out.push((path.clone(), parsed)),
                Err(_) => break,
            }
        }
    }
    Ok(out)
}

pub(crate) fn reindex_stats(vault: &Path, decl: &CollectionDecl) -> Result<(u64, u64, u64), Error> {
    let segments = list_segments(&journal_dir(vault, decl)?)?.len() as u64;
    let mut records = 0u64;
    let mut invalid = 0u64;
    for record in scan_journal(vault, decl)? {
        records += 1;
        if record.valid != Some(true) {
            invalid += 1;
        }
    }
    Ok((records, invalid, segments))
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

fn parse_line(decl: &CollectionDecl, line: &str) -> Result<ParsedLine, Error> {
    if line.is_empty() {
        return Err(Error::invalid("empty journal line"));
    }
    let value: Value = serde_json::from_str(line)
        .map_err(|err| Error::invalid(format!("journal line is not JSON: {err}")))?;
    let Value::Object(fields) = value else {
        return Err(Error::invalid("journal line must be a JSON object"));
    };
    let id = match fields.get(&decl.id_field) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            return Err(Error::invalid(format!(
                "journal object {} is missing",
                decl.id_field
            )));
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
    let valid = store::validate_fields(
        &parsed.fields,
        &decl.fields,
        Some(decl.id_field.as_str()),
        false,
    )
    .is_ok()
        && decl
            .id_pattern
            .as_ref()
            .is_none_or(|pattern| pattern.is_match(&parsed.id));
    let mut record = Record::default()
        .with_id(parsed.id)
        .with_collection(decl.name.clone())
        .with_path(rel)
        .with_valid(valid)
        .with_raw(parsed.raw);
    store::put_fields(&mut record, parsed.fields)?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::{canonical_json, civil_from_unix_days, write_canonical};
    use serde_json::json;

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
    fn segment_seq_is_eight_lowercase_hex() {
        assert_eq!(super::parse_hex_seq("00000001"), Some(1));
        assert_eq!(super::parse_hex_seq("0000000a"), Some(10));
        assert_eq!(super::parse_hex_seq("ffffffff"), Some(u32::MAX));
        assert_eq!(super::parse_hex_seq("0001"), None);
        assert_eq!(super::parse_hex_seq("+0000001"), None);
        assert_eq!(format!("{:08x}", 10u32), "0000000a");
    }
}
