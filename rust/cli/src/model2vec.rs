//! Operator folder for `--model2vec-embedder`.
//!
//! The CLI does not ship weights and does not download from the Hub.
//! The directory must contain Model2Vec files (`config.json`,
//! `tokenizer.json`, `model.safetensors`). Hidden names are ignored.
//! Extra files (README) are allowed. Missing required files is
//! `INVALID_ARGUMENT`.

use std::num::NonZeroUsize;
use std::path::Path;

use model2vec_rs::model::StaticModel;
use scheda::v1::embedder_ref::EmbedderRef;
use scheda::{Error, FixedEmbedder};
use sha2::{Digest, Sha256};

use crate::stdio::{invalid, unreadable};

const CONFIG: &str = "config.json";
const TOKENIZER: &str = "tokenizer.json";
const MODEL: &str = "model.safetensors";

/// How many texts one call to the model takes at most.
const PORTION: NonZeroUsize = match NonZeroUsize::new(1024) {
    Some(n) => n,
    None => unreachable!(),
};

/// The embedder of the Model2Vec model in `dir`, as a reference for
/// `VaultOpening`.
///
/// # Errors
///
/// `INVALID_ARGUMENT` when `dir` is not a folder, lacks one of the model
/// files, or holds a model that cannot be loaded.
pub(crate) fn embedder(dir: &Path) -> Result<EmbedderRef, Error> {
    require_model_dir(dir)?;
    let model_id = model_identity(dir)?;
    let model = StaticModel::from_pretrained(dir, None, None, None)
        .map_err(|err| invalid(format!("cannot load the model2vec model: {err}")))?;
    let dimensions = u32::try_from(model.encode_single("x").len())
        .ok()
        .filter(|dimensions| *dimensions > 0)
        .ok_or_else(|| invalid("model2vec embeddings have no dimensions"))?;
    Ok(EmbedderRef::from_sync(FixedEmbedder::new(
        model_id,
        dimensions,
        PORTION,
        move |texts: Vec<String>| match texts.as_slice() {
            [one] => Ok(vec![model.encode_single(one)]),
            many => Ok(model.encode(many)),
        },
    )))
}

fn require_model_dir(dir: &Path) -> Result<(), Error> {
    if !dir.is_dir() {
        return Err(invalid(format!(
            "operator dir is not a directory: {}",
            dir.display()
        )));
    }
    for name in [CONFIG, TOKENIZER, MODEL] {
        let path = dir.join(name);
        if !path.is_file() {
            return Err(invalid(format!("missing {name}")));
        }
    }
    Ok(())
}

/// The model id of a Model2Vec directory: `model2vec:` plus the first 16
/// hex digits of the SHA-256 of `config.json` followed by the byte length of
/// `model.safetensors`. The identity comes from the model files, not from
/// where the directory is: moving or renaming it keeps the Index vectors
/// valid, while another model (a different config or weight file size) reads
/// as a model change. The weights are not hashed: that would read hundreds
/// of megabytes on every command.
fn model_identity(dir: &Path) -> Result<String, Error> {
    use std::fmt::Write as _;

    let config_path = dir.join(CONFIG);
    let config = std::fs::read(&config_path)
        .map_err(|err| unreadable(&config_path.display().to_string(), err))?;
    let weights_path = dir.join(MODEL);
    let weights = std::fs::metadata(&weights_path)
        .map_err(|err| unreadable(&weights_path.display().to_string(), err))?
        .len();
    let mut hasher = Sha256::new();
    hasher.update(&config);
    hasher.update(weights.to_le_bytes());
    let digest = hasher.finalize();
    let mut id = String::from("model2vec:");
    for byte in digest.iter().take(8) {
        // Writing into a `String` cannot fail.
        let _ = write!(id, "{byte:02x}");
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{CONFIG, MODEL, model_identity};

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("scheda-m2v-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn model(dir: &Dir, config: &str, weights: &[u8]) {
        fs::write(dir.0.join(CONFIG), config).unwrap();
        fs::write(dir.0.join(MODEL), weights).unwrap();
    }

    #[test]
    fn identity_follows_the_model_files_not_the_directory() {
        let a = Dir::new("a");
        let b = Dir::new("b");
        model(&a, r#"{"hidden_dim":4}"#, b"weights");
        model(&b, r#"{"hidden_dim":4}"#, b"weights");
        let id = model_identity(&a.0).unwrap();
        assert_eq!(id, model_identity(&b.0).unwrap());
        assert!(id.starts_with("model2vec:"), "{id}");
        assert_eq!(id.len(), "model2vec:".len() + 16, "{id}");
        assert!(!id.contains(a.0.to_str().unwrap()), "{id}");

        model(&b, r#"{"hidden_dim":8}"#, b"weights");
        assert_ne!(id, model_identity(&b.0).unwrap());
        model(&b, r#"{"hidden_dim":4}"#, b"longer weights");
        assert_ne!(id, model_identity(&b.0).unwrap());
    }
}
