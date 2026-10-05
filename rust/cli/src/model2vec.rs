//! Operator folder for `--model2vec-embedder`.
//!
//! The CLI does not ship weights and does not download from the Hub.
//! The directory must contain Model2Vec files (`config.json`,
//! `tokenizer.json`, `model.safetensors`). Hidden names are ignored.
//! Extra files (README) are allowed. Missing required files is `invalid`.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use model2vec_rs::model::StaticModel;
use scheda::{Embedder, Error};
use sha2::{Digest, Sha256};

const CONFIG: &str = "config.json";
const TOKENIZER: &str = "tokenizer.json";
const MODEL: &str = "model.safetensors";

pub fn load_embedder(dir: &Path) -> Result<Arc<dyn Embedder>, Error> {
    require_model_dir(dir)?;
    let model_id = model_identity(dir)?;
    let model = StaticModel::from_pretrained(dir, None, None, None)
        .map_err(|err| Error::invalid(err.to_string()))?;
    let dimensions = NonZeroUsize::new(model.encode_single("x").len())
        .ok_or_else(|| Error::invalid("model2vec embeddings have no dimensions"))?;
    Ok(Arc::new(Model2VecEmbedder {
        model_id,
        dimensions,
        model,
    }))
}

fn require_model_dir(dir: &Path) -> Result<(), Error> {
    if !dir.is_dir() {
        return Err(Error::invalid(format!(
            "operator dir is not a directory: {}",
            dir.display()
        )));
    }
    for name in [CONFIG, TOKENIZER, MODEL] {
        let path = dir.join(name);
        if !path.is_file() {
            return Err(Error::invalid(format!("missing {name}")));
        }
    }
    Ok(())
}

/// `Embedder::model_id` for a Model2Vec directory: `model2vec:` plus the
/// first 16 hex digits of the SHA-256 of `config.json` followed by the byte
/// length of `model.safetensors`. The identity comes from the model files,
/// not from where the directory is: moving or renaming it keeps the Index
/// vectors valid, while another model (a different config or weight file
/// size) reads as a model change. The weights are not hashed: that would
/// read hundreds of megabytes on every command.
fn model_identity(dir: &Path) -> Result<String, Error> {
    use std::fmt::Write as _;

    let config_path = dir.join(CONFIG);
    let config =
        std::fs::read(&config_path).map_err(|err| crate::io_error(&err, Some(&config_path)))?;
    let weights_path = dir.join(MODEL);
    let weights = std::fs::metadata(&weights_path)
        .map_err(|err| crate::io_error(&err, Some(&weights_path)))?
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

struct Model2VecEmbedder {
    model_id: String,
    dimensions: NonZeroUsize,
    model: StaticModel,
}

impl Embedder for Model2VecEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> NonZeroUsize {
        self.dimensions
    }

    fn many_limit(&self) -> NonZeroUsize {
        const LIMIT: NonZeroUsize = match NonZeroUsize::new(1024) {
            Some(n) => n,
            None => unreachable!(),
        };
        LIMIT
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, Error> {
        Ok(self.model.encode_single(text))
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let owned: Vec<String> = texts.iter().map(|text| (*text).to_owned()).collect();
        Ok(self.model.encode(&owned))
    }
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
