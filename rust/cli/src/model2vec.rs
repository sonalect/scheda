//! Operator folder for `--model2vec-embedder`.
//!
//! The CLI does not ship weights and does not download from the Hub.
//! The directory must contain Model2Vec files (`config.json`,
//! `tokenizer.json`, `model.safetensors`). Hidden names are ignored.
//! Extra files (README) are allowed. Missing required files is `invalid`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use model2vec_rs::model::StaticModel;
use scheda::{Embedder, Error};

const CONFIG: &str = "config.json";
const TOKENIZER: &str = "tokenizer.json";
const MODEL: &str = "model.safetensors";

pub fn load_embedder(dir: &Path) -> Result<Arc<dyn Embedder>, Error> {
    require_model_dir(dir)?;
    let model = StaticModel::from_pretrained(dir, None, None, None)
        .map_err(|err| Error::invalid(err.to_string()))?;
    let dimensions = model.encode_single("x").len();
    if dimensions == 0 {
        return Err(Error::invalid("model2vec embeddings have no dimensions"));
    }
    Ok(Arc::new(Model2VecEmbedder {
        model_id: canonical_id(dir),
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

fn canonical_id(dir: &Path) -> String {
    dir.canonicalize()
        .map(PathBuf::into_os_string)
        .ok()
        .and_then(|os| os.into_string().ok())
        .unwrap_or_else(|| dir.display().to_string())
}

struct Model2VecEmbedder {
    model_id: String,
    dimensions: usize,
    model: StaticModel,
}

impl Embedder for Model2VecEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, Error> {
        Ok(self.model.encode_single(text))
    }
}
