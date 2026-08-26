//! Operator folder for `--onnx-embedder` / `--reranker-onnx` (`14`).
//!
//! The CLI does not ship weights. The directory must contain exactly
//! `model.onnx` and `tokenizer.json`. Hidden names (`.metadata.yaml`)
//! are ignored. Missing either required file (or extra non-hidden names)
//! is `invalid`. Inference is the caller's model; this crate only loads
//! the two files and implements [`Embedder`] / [`Reranker`].

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use scheda::{Embedder, Error, Record, Reranker};

const MODEL: &str = "model.onnx";
const TOKENIZER: &str = "tokenizer.json";

pub fn load_embedder(dir: &Path) -> Result<Arc<dyn Embedder>, Error> {
    let files = operator_files(dir)?;
    let dimensions = output_dimensions(&files.model)?;
    Ok(Arc::new(OnnxEmbedder {
        model_id: files.model_id,
        dimensions,
        model: files.model,
        tokenizer: files.tokenizer,
    }))
}

pub fn load_reranker(dir: &Path) -> Result<Arc<dyn Reranker>, Error> {
    let files = operator_files(dir)?;
    Ok(Arc::new(OnnxReranker {
        model: files.model,
        tokenizer: files.tokenizer,
    }))
}

struct OperatorFiles {
    model_id: String,
    model: Vec<u8>,
    tokenizer: String,
}

fn operator_files(dir: &Path) -> Result<OperatorFiles, Error> {
    if !dir.is_dir() {
        return Err(Error::invalid(format!(
            "operator dir is not a directory: {}",
            dir.display()
        )));
    }
    let mut names = Vec::new();
    let entries = fs::read_dir(dir).map_err(|err| Error::invalid(err.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|err| Error::invalid(err.to_string()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(Error::invalid("operator dir entry is not UTF-8"));
        };
        if name == "." || name == ".." || name.starts_with('.') {
            continue;
        }
        names.push(name.to_owned());
    }
    let has_model = names.iter().any(|name| name == MODEL);
    let has_tokenizer = names.iter().any(|name| name == TOKENIZER);
    if !has_model {
        return Err(Error::invalid("missing model.onnx"));
    }
    if !has_tokenizer {
        return Err(Error::invalid("missing tokenizer.json"));
    }
    if names.len() != 2 {
        return Err(Error::invalid(
            "operator dir must contain exactly model.onnx and tokenizer.json",
        ));
    }
    let model_path = dir.join(MODEL);
    let tokenizer_path = dir.join(TOKENIZER);
    if !model_path.is_file() {
        return Err(Error::invalid("missing model.onnx"));
    }
    if !tokenizer_path.is_file() {
        return Err(Error::invalid("missing tokenizer.json"));
    }
    let tokenizer = fs::read_to_string(&tokenizer_path).map_err(|err| Error::invalid(err.to_string()))?;
    let _: serde_json::Value =
        serde_json::from_str(&tokenizer).map_err(|err| Error::invalid(err.to_string()))?;
    let model = fs::read(&model_path).map_err(|err| Error::invalid(err.to_string()))?;
    if model.is_empty() {
        return Err(Error::invalid("model.onnx is empty"));
    }
    Ok(OperatorFiles {
        model_id: canonical_id(dir),
        model,
        tokenizer,
    })
}

fn canonical_id(dir: &Path) -> String {
    fs::canonicalize(dir)
        .map(PathBuf::into_os_string)
        .ok()
        .and_then(|os| os.into_string().ok())
        .unwrap_or_else(|| dir.display().to_string())
}

struct OnnxEmbedder {
    model_id: String,
    dimensions: usize,
    model: Vec<u8>,
    tokenizer: String,
}

impl Embedder for OnnxEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, Error> {
        let _ = (&self.model, &self.tokenizer, text);
        Err(Error::invalid(
            "CLI ONNX runtime cannot execute this model",
        ))
    }
}

struct OnnxReranker {
    model: Vec<u8>,
    tokenizer: String,
}

impl Reranker for OnnxReranker {
    fn score(&self, query: &str, record: &Record) -> Result<f64, Error> {
        let _ = (&self.model, &self.tokenizer, query, record);
        Err(Error::invalid(
            "CLI ONNX runtime cannot execute this model",
        ))
    }
}

/// Last fixed `dim_value` on the first graph output tensor (`TypeProto`).
fn output_dimensions(model: &[u8]) -> Result<usize, Error> {
    let graph = protobuf_field(model, 7)?.ok_or_else(|| Error::invalid("model.onnx has no graph"))?;
    let output = protobuf_field(graph, 12)?
        .ok_or_else(|| Error::invalid("model.onnx has no graph output"))?;
    let ty =
        protobuf_field(output, 2)?.ok_or_else(|| Error::invalid("model.onnx output has no type"))?;
    let tensor = protobuf_field(ty, 1)?.ok_or_else(|| {
        Error::invalid("model.onnx output is not a tensor")
    })?;
    let shape = protobuf_field(tensor, 2)?.ok_or_else(|| {
        Error::invalid("model.onnx tensor has no shape")
    })?;
    let mut last = None;
    let mut rest = shape;
    while !rest.is_empty() {
        let (number, wire, payload, next) = protobuf_next(rest)?;
        rest = next;
        if number != 1 || wire != 2 {
            continue;
        }
        if let Some(value) = protobuf_varint_field(payload, 1)? {
            last = Some(value);
        }
    }
    let Some(dim) = last else {
        return Err(Error::invalid(
            "model.onnx has no tensor output dimension",
        ));
    };
    usize::try_from(dim)
        .ok()
        .filter(|dim| *dim > 0)
        .ok_or_else(|| Error::invalid("model.onnx tensor output dimension is invalid"))
}

fn protobuf_field(bytes: &[u8], want: u64) -> Result<Option<&[u8]>, Error> {
    let mut rest = bytes;
    while !rest.is_empty() {
        let (number, wire, payload, next) = protobuf_next(rest)?;
        rest = next;
        if number == want && wire == 2 {
            return Ok(Some(payload));
        }
    }
    Ok(None)
}

fn protobuf_varint_field(bytes: &[u8], want: u64) -> Result<Option<u64>, Error> {
    let mut rest = bytes;
    while !rest.is_empty() {
        let (number, wire, payload, next) = protobuf_next(rest)?;
        rest = next;
        if number == want && wire == 0 {
            return read_varint(payload).map(Some);
        }
    }
    Ok(None)
}

type ProtobufField<'a> = (u64, u8, &'a [u8], &'a [u8]);

fn protobuf_next(bytes: &[u8]) -> Result<ProtobufField<'_>, Error> {
    let (key, after_key) = read_varint_at(bytes)?;
    let number = key >> 3;
    let wire = (key & 7) as u8;
    match wire {
        0 => {
            let (_, after) = read_varint_at(after_key)?;
            let payload_len = after_key.len() - after.len();
            Ok((number, wire, &after_key[..payload_len], after))
        }
        1 if after_key.len() >= 8 => Ok((number, wire, &after_key[..8], &after_key[8..])),
        5 if after_key.len() >= 4 => Ok((number, wire, &after_key[..4], &after_key[4..])),
        2 => {
            let (len, after_len) = read_varint_at(after_key)?;
            let len = usize::try_from(len)
                .map_err(|_| Error::invalid("model.onnx is not valid protobuf"))?;
            if after_len.len() < len {
                return Err(Error::invalid("model.onnx is not valid protobuf"));
            }
            Ok((number, wire, &after_len[..len], &after_len[len..]))
        }
        _ => Err(Error::invalid("model.onnx is not valid protobuf")),
    }
}

fn read_varint(bytes: &[u8]) -> Result<u64, Error> {
    read_varint_at(bytes).map(|(value, _)| value)
}

fn read_varint_at(bytes: &[u8]) -> Result<(u64, &[u8]), Error> {
    let mut value = 0u64;
    let mut shift = 0;
    for (i, byte) in bytes.iter().copied().enumerate() {
        if shift >= 64 {
            return Err(Error::invalid("model.onnx is not valid protobuf"));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, &bytes[i + 1..]));
        }
        shift += 7;
    }
    Err(Error::invalid("model.onnx is not valid protobuf"))
}
