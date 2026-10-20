//! Messages Scheda keeps in files: the marker, a collection file, and every
//! other file that is the Protobuf JSON (or YAML) of a message.
//!
//! One mechanism serves them all, with no code per message. A file is read
//! into an ordered tree, then decoded into the generated message with the
//! keys the message does not know passed over and recorded. The recorded
//! keys are the file's *unknown fields*: a later version of Scheda may have
//! added them, so a rewrite puts them back where they were. The message is
//! then checked by its protovalidate rules.
//!
//! A command a caller sends is parsed the other way: a key the message does
//! not know is a refusal, because ignoring it would do something other than
//! what the caller asked.
//!
//! The module works on bytes. The caller reads and writes the file, and adds
//! the file's path to a failure with [`Fault::with_path`].

use std::fmt::Write as _;
use std::path::Path;

use protovalidate_buffa::{Validate, ValidationError};
use scheda_api::v1::VaultErrorCode;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_ignored::Path as SerdePath;
use yaml_serde::Value;

use crate::error::{Error, Fault};

/// The syntax of a stored file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    /// YAML (`.yaml`, `.yml`). JSON is YAML, but a file that is named JSON
    /// is read and written as JSON.
    Yaml,
    /// JSON (`.json`).
    Json,
}

impl Format {
    /// The format of a file from its extension: `.yaml` and `.yml` are
    /// [`Format::Yaml`], `.json` is [`Format::Json`], anything else is
    /// `None`.
    pub(crate) fn of_path(path: &Path) -> Option<Format> {
        match path.extension()?.to_str()? {
            "yaml" | "yml" => Some(Format::Yaml),
            "json" => Some(Format::Json),
            _ => None,
        }
    }
}

/// One step of the path from the root of a file to a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Segment {
    /// A key of a mapping.
    Key(String),
    /// An element of a list.
    Index(usize),
}

/// A key of a file that the message did not know when it was read, with
/// its value as the file had it.
#[derive(Clone, Debug)]
pub(crate) struct Unknown {
    path: Vec<Segment>,
    value: Value,
}

impl Unknown {
    /// The path of the key, ending with the key itself.
    pub(crate) fn path(&self) -> &[Segment] {
        &self.path
    }

    /// The value the file had under the key.
    #[cfg(test)]
    pub(crate) fn value(&self) -> &Value {
        &self.value
    }
}

/// The unknown fields of one file, in the order the file had them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Unknowns(Vec<Unknown>);

impl Unknowns {
    /// No unknown field.
    pub(crate) fn none() -> Unknowns {
        Unknowns(Vec::new())
    }

    /// Whether the file had no unknown field.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The fields, in file order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Unknown> {
        self.0.iter()
    }
}

/// A message read from a file, with the unknown fields of that file.
#[derive(Clone, Debug)]
pub(crate) struct Stored<T> {
    /// The message, validated.
    pub(crate) message: T,
    /// What the file had that the message does not know.
    pub(crate) unknown: Unknowns,
}

/// Parses `bytes` into an ordered tree. A syntax error is
/// `INVALID_ARGUMENT`. JSON parses into the same tree as YAML, so one tree
/// type carries both formats and keeps the order of every mapping.
pub(crate) fn parse_tree(bytes: &[u8], format: Format) -> Result<Value, Fault> {
    match format {
        Format::Yaml => yaml_serde::from_slice::<Value>(bytes).map_err(|err| {
            let message = format!("the YAML does not parse: {err}");
            Fault::new(VaultErrorCode::InvalidArgument, message).with_source(err)
        }),
        Format::Json => serde_json::from_slice::<Value>(bytes).map_err(|err| {
            let message = format!("the JSON does not parse: {err}");
            Fault::new(VaultErrorCode::InvalidArgument, message).with_source(err)
        }),
    }
}

/// Reads the message `T` from `bytes`: parses the tree, decodes `T` with
/// the keys it does not know passed over and recorded, and checks the
/// protovalidate rules of `T`.
///
/// A syntax error, a value of the wrong type, and a broken rule are
/// `INVALID_ARGUMENT`; the message of a broken rule lists the violations
/// with the field path of each.
pub(crate) fn read<T>(bytes: &[u8], format: Format) -> Result<Stored<T>, Fault>
where
    T: DeserializeOwned + Validate,
{
    let tree = parse_tree(bytes, format)?;
    decode(&tree)
}

/// Reads the message `T` from a parsed tree, as [`read`] does from bytes.
pub(crate) fn decode<T>(tree: &Value) -> Result<Stored<T>, Fault>
where
    T: DeserializeOwned + Validate,
{
    let mut passed_over: Vec<Vec<Segment>> = Vec::new();
    let message: T = serde_ignored::deserialize(tree, |path| passed_over.push(segments(&path)))
        .map_err(decode_fault)?;
    validate(&message)?;
    let unknown = passed_over
        .into_iter()
        .filter_map(|path| {
            let value = value_at(tree, &path)?.clone();
            Some(Unknown { path, value })
        })
        .collect();
    Ok(Stored {
        message,
        unknown: Unknowns(unknown),
    })
}

/// Encodes `message` as the bytes of a file in `format`, with `unknown`
/// put back.
///
/// The message is serialized with its generated serde into the ordered
/// tree: known keys in message order, under their Protobuf JSON names. Each
/// unknown field then goes back under its parent, after the known keys of
/// that parent and in the order it had in the file. An unknown field
/// belongs to the message that held it: when a parent on its path is not in
/// the new tree (another arm of a `oneof`, a field that was cleared, a list
/// element that is gone), the field goes with it. A list element is told by
/// its position. JSON is written indented, YAML as is, both with a trailing
/// newline.
pub(crate) fn encode<T: Serialize>(
    message: &T,
    unknown: &Unknowns,
    format: Format,
) -> Result<Vec<u8>, Fault> {
    let mut tree = yaml_serde::to_value(message).map_err(|err| {
        let text = format!("the message does not serialize: {err}");
        Fault::new(VaultErrorCode::InvalidArgument, text).with_source(err)
    })?;
    put_back(&mut tree, unknown);
    match format {
        Format::Yaml => yaml_serde::to_string(&tree)
            .map(String::into_bytes)
            .map_err(|err| {
                let text = format!("the YAML does not serialize: {err}");
                Fault::new(VaultErrorCode::InvalidArgument, text).with_source(err)
            }),
        Format::Json => {
            let mut bytes = serde_json::to_vec_pretty(&tree).map_err(|err| {
                let text = format!("the JSON does not serialize: {err}");
                Fault::new(VaultErrorCode::InvalidArgument, text).with_source(err)
            })?;
            bytes.push(b'\n');
            Ok(bytes)
        }
    }
}

/// Parses the Protobuf JSON of a request a caller sends (a message of
/// [`v1`](crate::v1)), strictly: a key the message `T` does not know is
/// `INVALID_ARGUMENT` naming its path, because ignoring it would do
/// something other than what the caller asked; so is JSON that does not
/// parse and a broken protovalidate rule. A file Scheda stores is read the
/// other way, its unknown keys kept.
///
/// # Errors
///
/// `INVALID_ARGUMENT`, as above.
pub fn parse_command<T>(json: &[u8]) -> Result<T, Error>
where
    T: DeserializeOwned + Validate,
{
    let tree = parse_tree(json, Format::Json)?;
    let stored: Stored<T> = decode(&tree)?;
    if !stored.unknown.is_empty() {
        let mut names = String::new();
        for (i, field) in stored.unknown.iter().enumerate() {
            if i > 0 {
                names.push_str(", ");
            }
            names.push_str(&path_string(field.path()));
        }
        let message = format!("the command has fields this version does not know: {names}");
        return Err(Fault::new(VaultErrorCode::InvalidArgument, message).into());
    }
    Ok(stored.message)
}

/// The path of a key as text: `schema.journal.extra`, `columns[2].extra`.
pub(crate) fn path_string(path: &[Segment]) -> String {
    let mut out = String::new();
    for segment in path {
        match segment {
            Segment::Key(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            Segment::Index(index) => {
                let _ = write!(out, "[{index}]");
            }
        }
    }
    out
}

/// `INVALID_ARGUMENT` when `message` breaks a protovalidate rule; the
/// message lists every violation with its field path.
pub(crate) fn validate<T: Validate>(message: &T) -> Result<(), Fault> {
    match message.validate() {
        Ok(()) => Ok(()),
        Err(err) => Err(violations_fault(err)),
    }
}

fn violations_fault(err: ValidationError) -> Fault {
    let mut text = String::new();
    for violation in &err.violations {
        if !text.is_empty() {
            text.push_str("; ");
        }
        let field = violation.field.to_string();
        if field.is_empty() {
            text.push_str(&violation.message);
        } else {
            let _ = write!(text, "{field}: {}", violation.message);
        }
    }
    for problem in [&err.compile_error, &err.runtime_error]
        .into_iter()
        .flatten()
    {
        if !text.is_empty() {
            text.push_str("; ");
        }
        text.push_str(problem);
    }
    if text.is_empty() {
        text.push_str("the message breaks a rule");
    }
    Fault::new(VaultErrorCode::InvalidArgument, text).with_source(err)
}

fn decode_fault(err: yaml_serde::Error) -> Fault {
    let message = format!("the message does not decode: {err}");
    Fault::new(VaultErrorCode::InvalidArgument, message).with_source(err)
}

/// The path serde reports for a passed-over key, as segments. The wrappers
/// `Some` and newtype are not steps of the file, so they are left out.
fn segments(path: &SerdePath<'_>) -> Vec<Segment> {
    fn walk(path: &SerdePath<'_>, out: &mut Vec<Segment>) {
        match path {
            SerdePath::Root => {}
            SerdePath::Seq { parent, index } => {
                walk(parent, out);
                out.push(Segment::Index(*index));
            }
            SerdePath::Map { parent, key } => {
                walk(parent, out);
                out.push(Segment::Key(key.clone()));
            }
            SerdePath::Some { parent }
            | SerdePath::NewtypeStruct { parent }
            | SerdePath::NewtypeVariant { parent } => walk(parent, out),
        }
    }
    let mut out = Vec::new();
    walk(path, &mut out);
    out
}

/// The value under `path` in `tree`.
fn value_at<'a>(tree: &'a Value, path: &[Segment]) -> Option<&'a Value> {
    let mut node = tree;
    for segment in path {
        node = match (segment, node) {
            (Segment::Key(key), Value::Mapping(map)) => map.get(key.as_str())?,
            (Segment::Index(index), Value::Sequence(list)) => list.get(*index)?,
            _ => return None,
        };
    }
    Some(node)
}

/// Puts each unknown field back into `tree`, under its parent.
fn put_back(tree: &mut Value, unknown: &Unknowns) {
    'fields: for field in &unknown.0 {
        let Some((Segment::Key(key), parents)) = field.path.split_last() else {
            continue;
        };
        let mut node = &mut *tree;
        for segment in parents {
            let next = match (segment, node) {
                (Segment::Key(parent), Value::Mapping(map)) => map.get_mut(parent.as_str()),
                (Segment::Index(index), Value::Sequence(list)) => list.get_mut(*index),
                _ => None,
            };
            match next {
                Some(next) => node = next,
                None => continue 'fields,
            }
        }
        if let Value::Mapping(map) = node
            && !map.contains_key(key.as_str())
        {
            map.insert(Value::String(key.clone()), field.value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use scheda_api::v1::{
        CollectionDeclaration, IndexSchema, OpenOptions, VaultErrorCode, VaultMarker,
    };

    use super::{Format, Segment, Stored, Unknowns, encode, parse_command, path_string, read};
    use crate::error::Error;

    const JOURNAL_YAML: &str = "\
name: log
future: top
schema:
  journal:
    segmentMaxLines: '10'
    rotation: weekly
    nested:
      deep: 1
  encrypted: false
  later: [1, 2]
";

    const JOURNAL_JSON: &str = r#"{
  "name": "log",
  "future": "top",
  "schema": {
    "journal": {
      "segmentMaxLines": "10",
      "rotation": "weekly",
      "nested": {"deep": 1}
    },
    "encrypted": false,
    "later": [1, 2]
  }
}"#;

    fn code(fault: crate::error::Fault) -> Error {
        Error::from(fault)
    }

    fn keys(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|line| line.split_once(':').map(|(key, _)| key.trim().to_owned()))
            .collect()
    }

    fn read_collection(text: &str, format: Format) -> Stored<CollectionDeclaration> {
        read::<CollectionDeclaration>(text.as_bytes(), format).unwrap()
    }

    #[test]
    fn unknown_keys_are_read_past_and_recorded_in_yaml_and_json() {
        for (text, format) in [(JOURNAL_YAML, Format::Yaml), (JOURNAL_JSON, Format::Json)] {
            let stored = read_collection(text, format);
            assert_eq!(stored.message.name.as_deref(), Some("log"));
            let paths: Vec<String> = stored
                .unknown
                .iter()
                .map(|u| path_string(u.path()))
                .collect();
            assert_eq!(
                paths,
                [
                    "future",
                    "schema.journal.rotation",
                    "schema.journal.nested",
                    "schema.later"
                ],
                "{format:?}"
            );
        }
    }

    #[test]
    fn a_rewrite_keeps_unknown_keys_at_their_depth_after_the_known_ones() {
        for (text, format) in [(JOURNAL_YAML, Format::Yaml), (JOURNAL_JSON, Format::Json)] {
            let stored = read_collection(text, format);
            let bytes = encode(&stored.message, &stored.unknown, format).unwrap();
            assert_eq!(bytes.last(), Some(&b'\n'));
            let again = read::<CollectionDeclaration>(&bytes, format).unwrap();
            assert_eq!(again.message, stored.message);
            let paths: Vec<String> = again
                .unknown
                .iter()
                .map(|u| path_string(u.path()))
                .collect();
            assert_eq!(
                paths,
                [
                    "schema.journal.rotation",
                    "schema.journal.nested",
                    "schema.later",
                    "future"
                ],
                "{format:?}"
            );
            for (was, now) in stored.unknown.iter().zip(
                [
                    "future",
                    "schema.journal.rotation",
                    "schema.journal.nested",
                    "schema.later",
                ]
                .iter(),
            ) {
                assert_eq!(path_string(was.path()), *now);
            }
            let tree = super::parse_tree(&bytes, format).unwrap();
            let yaml_serde::Value::Mapping(top) = &tree else {
                panic!("not a mapping");
            };
            let top_keys: Vec<&str> = top.keys().filter_map(yaml_serde::Value::as_str).collect();
            assert_eq!(top_keys, ["name", "schema", "future"], "{format:?}");
            let schema = top
                .get("schema")
                .and_then(yaml_serde::Value::as_mapping)
                .unwrap();
            let schema_keys: Vec<&str> = schema
                .keys()
                .filter_map(yaml_serde::Value::as_str)
                .collect();
            assert_eq!(schema_keys, ["encrypted", "journal", "later"], "{format:?}");
            let journal = schema
                .get("journal")
                .and_then(yaml_serde::Value::as_mapping)
                .unwrap();
            let journal_keys: Vec<&str> = journal
                .keys()
                .filter_map(yaml_serde::Value::as_str)
                .collect();
            assert_eq!(
                journal_keys,
                ["segmentMaxLines", "rotation", "nested"],
                "{format:?}"
            );
        }
    }

    #[test]
    fn json_is_written_indented_and_yaml_is_not_json() {
        let stored = read_collection(JOURNAL_YAML, Format::Yaml);
        let json = encode(&stored.message, &stored.unknown, Format::Json).unwrap();
        let text = String::from_utf8(json).unwrap();
        assert!(text.starts_with("{\n  \""), "{text}");
        let yaml = encode(&stored.message, &stored.unknown, Format::Yaml).unwrap();
        let text = String::from_utf8(yaml).unwrap();
        assert!(!text.starts_with('{'), "{text}");
        assert_eq!(keys(&text)[0], "name");
    }

    #[test]
    fn changing_the_kind_drops_the_unknown_keys_of_the_old_kind() {
        let stored = read_collection(JOURNAL_YAML, Format::Yaml);
        let mut message = stored.message.clone();
        let mut schema = message.schema.as_option().cloned().unwrap();
        schema.kind = scheda_api::v1::Standard::default().into();
        message.schema = schema.into();
        let bytes = encode(&message, &stored.unknown, Format::Yaml).unwrap();
        let again = read::<CollectionDeclaration>(&bytes, Format::Yaml).unwrap();
        let paths: Vec<String> = again
            .unknown
            .iter()
            .map(|u| path_string(u.path()))
            .collect();
        assert!(
            paths.iter().all(|path| !path.starts_with("schema.journal")),
            "{paths:?}"
        );
        assert!(paths.contains(&String::from("future")));
        assert!(paths.contains(&String::from("schema.later")));
    }

    #[test]
    fn an_unknown_key_in_a_list_element_is_kept_with_the_element() {
        let text = "columns:\n- {name: a, kind: COLUMN_KIND_TEXT}\n- {name: b, kind: COLUMN_KIND_TEXT, extra: 1}\n";
        let stored = read::<IndexSchema>(text.as_bytes(), Format::Yaml).unwrap();
        assert_eq!(stored.message.columns.len(), 2);
        let paths: Vec<String> = stored
            .unknown
            .iter()
            .map(|u| path_string(u.path()))
            .collect();
        assert_eq!(paths, ["columns[1].extra"]);
        let extra = stored.unknown.iter().next().unwrap();
        assert_eq!(extra.path()[1], Segment::Index(1));
        assert_eq!(extra.value().as_u64(), Some(1));

        let bytes = encode(&stored.message, &stored.unknown, Format::Yaml).unwrap();
        let again = read::<IndexSchema>(&bytes, Format::Yaml).unwrap();
        let paths: Vec<String> = again
            .unknown
            .iter()
            .map(|u| path_string(u.path()))
            .collect();
        assert_eq!(paths, ["columns[1].extra"]);

        let mut message = stored.message.clone();
        message.columns.remove(1);
        let bytes = encode(&message, &stored.unknown, Format::Yaml).unwrap();
        let again = read::<IndexSchema>(&bytes, Format::Yaml).unwrap();
        assert_eq!(again.message.columns.len(), 1);
        // A list element is told by its position, so the fields of the last
        // element go when the list gets shorter.
        assert!(
            again.unknown.is_empty(),
            "the element went, its fields go with it"
        );

        let mut message = stored.message.clone();
        message.columns.clear();
        let bytes = encode(&message, &stored.unknown, Format::Yaml).unwrap();
        let again = read::<IndexSchema>(&bytes, Format::Yaml).unwrap();
        assert!(again.unknown.is_empty());
    }

    #[test]
    fn an_empty_set_of_unknown_fields_writes_the_message_alone() {
        let stored = read_collection("name: c\nschema:\n  standard: {}\n", Format::Yaml);
        assert!(stored.unknown.is_empty());
        let bytes = encode(&stored.message, &Unknowns::none(), Format::Yaml).unwrap();
        assert_eq!(
            read::<CollectionDeclaration>(&bytes, Format::Yaml)
                .unwrap()
                .message,
            stored.message
        );
    }

    #[test]
    fn a_syntax_error_is_invalid_argument() {
        let error = code(read::<CollectionDeclaration>(b"name: [c\n", Format::Yaml).unwrap_err());
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = code(read::<CollectionDeclaration>(b"{\"name\": ", Format::Json).unwrap_err());
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = code(read::<CollectionDeclaration>(b"", Format::Yaml).unwrap_err());
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[test]
    fn a_value_of_the_wrong_type_is_invalid_argument() {
        let error = code(read::<CollectionDeclaration>(b"name: [a]\n", Format::Yaml).unwrap_err());
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[test]
    fn a_broken_rule_is_invalid_argument_and_names_the_field() {
        let fault = read::<VaultMarker>(b"version: 4\n", Format::Yaml).unwrap_err();
        assert!(fault.message().contains("version"), "{}", fault.message());
        assert!(code(fault).is(VaultErrorCode::InvalidArgument));
        let error = code(read::<CollectionDeclaration>(b"name: ''\n", Format::Yaml).unwrap_err());
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[test]
    fn a_command_with_an_unknown_field_is_invalid_argument_naming_its_path() {
        let json = br#"{"name": "c", "schema": {"journal": {"bogus": true}, "standard": null}}"#;
        let error = parse_command::<CollectionDeclaration>(json).unwrap_err();
        assert!(
            error.to_string().contains("schema.journal.bogus"),
            "{error}"
        );
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = parse_command::<CollectionDeclaration>(
            br#"{"name": "c", "extra": 1, "schema": {"standard": {}}}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("extra"), "{error}");
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[test]
    fn a_retired_index_key_of_a_declaration_is_refused_not_dropped() {
        // Dropped, `excludeVectorIndex` would index vectors the caller
        // asked to leave out.
        for key in ["excludeLexicalIndex", "excludeVectorIndex"] {
            let json = format!(r#"{{"name": "c", "schema": {{"standard": {{}}, "{key}": true}}}}"#);
            let error = parse_command::<CollectionDeclaration>(json.as_bytes()).unwrap_err();
            assert!(error.to_string().contains(key), "{error}");
            assert!(error.is(VaultErrorCode::InvalidArgument));
        }
    }

    #[test]
    fn a_stale_key_of_the_open_options_is_refused_not_dropped() {
        // The key `encryption` once was `isolatedEncryption`: dropped, it
        // would open a plaintext Vault without an error.
        let stale = br#"{"isolatedEncryption": {"passphrase": "secret"}}"#;
        let error = parse_command::<OpenOptions>(stale).unwrap_err();
        assert!(error.to_string().contains("isolatedEncryption"), "{error}");
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let current = br#"{"encryption": {"passphrase": "secret"}}"#;
        assert!(parse_command::<OpenOptions>(current).is_ok());
    }

    #[test]
    fn a_command_with_known_fields_only_parses_and_is_validated() {
        let message = parse_command::<CollectionDeclaration>(
            br#"{"name": "c", "schema": {"journal": {"segmentMaxLines": "5"}}}"#,
        )
        .unwrap();
        assert_eq!(message.name.as_deref(), Some("c"));
        let error = parse_command::<CollectionDeclaration>(br#"{"name": "c"}"#).unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = parse_command::<CollectionDeclaration>(b"{").unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[test]
    fn the_format_comes_from_the_extension() {
        use std::path::Path;
        assert_eq!(
            Format::of_path(Path::new("a/.collection.yaml")),
            Some(Format::Yaml)
        );
        assert_eq!(
            Format::of_path(Path::new("a/.collection.yml")),
            Some(Format::Yaml)
        );
        assert_eq!(
            Format::of_path(Path::new("a/.collection.json")),
            Some(Format::Json)
        );
        assert_eq!(Format::of_path(Path::new("a/.collection.toml")), None);
        assert_eq!(Format::of_path(Path::new("a/noext")), None);
    }
}
