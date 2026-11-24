//! Local, in-process embedding adapter contract. The CLI can opt into BGE-M3 through
//! `AGENTRECALL_EMBEDDING_PROVIDER=bge-m3`; memory text must not be sent to a third party
//! without the owner's informed configuration and approval.

use std::{path::PathBuf, sync::Mutex};

use fastembed::{Bgem3Embedding, Bgem3InitOptions, Bgem3Model};

/// In-process CPU ONNX embedding provider for the quantized BGE-M3 model.
///
/// Calls are serialized because fastembed mutates its ONNX session; passing one
/// text at a time also makes the quantized output independent of request batches.
pub struct BgeM3Provider {
    embedding: Mutex<Bgem3Embedding>,
}

impl BgeM3Provider {
    pub fn new(cache_dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let options = Bgem3InitOptions::new(Bgem3Model::BGEM3Q).with_show_download_progress(false);
        let options = match cache_dir {
            Some(path) => options.with_cache_dir(path),
            None => options,
        };
        let embedding = Bgem3Embedding::try_new(options)
            .map_err(|_| anyhow::anyhow!("BGE-M3 model initialization failed (check model cache and Hugging Face connectivity)"))?;
        Ok(Self {
            embedding: Mutex::new(embedding),
        })
    }
}

impl EmbeddingProvider for BgeM3Provider {
    fn model(&self) -> EmbeddingModel {
        bge_m3_model_info()
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut embedding = self
            .embedding
            .lock()
            .map_err(|_| "embedding model lock poisoned".to_owned())?;
        let mut output = embedding
            .embed([text], Some(1))
            .map_err(|_| "BGE-M3 inference failed".to_owned())?;
        if output.dense.len() != 1 {
            return Err("BGE-M3 returned an unexpected number of dense vectors".to_owned());
        }
        Ok(output.dense.remove(0))
    }
}

fn bge_m3_model_info() -> EmbeddingModel {
    EmbeddingModel {
        name: "bge-m3-bg3m-q".to_owned(),
        version: "fastembed-7.8.0-gpahal-bge-m3-onnx-int8".to_owned(),
        dimension: 1024,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingModel {
    pub name: String,
    pub version: String,
    pub dimension: usize,
}

/// Synchronous local/in-process embedding adapter.
///
/// The service treats embeddings as derived search data. It persists vectors together
/// with this model identity, never as a replacement for canonical memory records.
pub trait EmbeddingProvider: Send + Sync + 'static {
    fn model(&self) -> EmbeddingModel;

    /// Return one finite, non-zero vector of exactly `model().dimension` values.
    /// Error strings are intentionally not logged or exposed to callers.
    fn embed(&self, text: &str) -> Result<Vec<f32>, String>;
}

pub(crate) fn validate_model(model: &EmbeddingModel) -> bool {
    (1..=4096).contains(&model.dimension)
        && valid_identifier(&model.name)
        && valid_identifier(&model.version)
}

fn valid_identifier(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 128 && !value.chars().any(char::is_control)
}

pub(crate) fn validate_vector(vector: &[f32], dimension: usize) -> bool {
    vector.len() == dimension
        && vector.iter().all(|value| value.is_finite())
        && vector.iter().any(|value| value.abs() > f32::EPSILON)
}

pub(crate) fn vector_literal(vector: &[f32]) -> Option<String> {
    if !validate_vector(vector, vector.len()) {
        return None;
    }
    let values = vector
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("[{values}]::FLOAT[{}]", vector.len()))
}

pub(crate) fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.len() != right.len() || left.is_empty() {
        return None;
    }
    let mut dot = 0.0_f64;
    let mut left_norm = 0.0_f64;
    let mut right_norm = 0.0_f64;
    for (left, right) in left.iter().zip(right) {
        if !left.is_finite() || !right.is_finite() {
            return None;
        }
        let left = f64::from(*left);
        let right = f64::from(*right);
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    if left_norm <= f64::EPSILON || right_norm <= f64::EPSILON {
        return None;
    }
    Some((dot / (left_norm.sqrt() * right_norm.sqrt())) as f32)
}

pub(crate) fn embedding_text(title: Option<&str>, content: &str, tags: &[String]) -> String {
    let mut text = String::new();
    if let Some(title) = title {
        text.push_str(title);
        text.push('\n');
    }
    text.push_str(content);
    if !tags.is_empty() {
        text.push('\n');
        text.push_str(&tags.join(" "));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_model_vectors_and_similarity() {
        let model = EmbeddingModel {
            name: "local-test".into(),
            version: "1".into(),
            dimension: 3,
        };
        assert!(validate_model(&model));
        assert!(validate_vector(&[1.0, 0.0, 0.0], 3));
        assert!(!validate_vector(&[f32::NAN, 0.0, 1.0], 3));
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.9, 0.1]), Some(0.9938837));
        assert_eq!(
            embedding_text(Some("title"), "body", &["tag".into()]),
            "title\nbody\ntag"
        );
        assert_eq!(
            vector_literal(&[1.0, -0.25]).as_deref(),
            Some("[1,-0.25]::FLOAT[2]")
        );
    }

    #[test]
    fn bge_m3_provider_declares_pinned_model_dimension() {
        let model = bge_m3_model_info();
        assert_eq!(model.dimension, 1024);
        assert_eq!(model.name, "bge-m3-bg3m-q");
        assert!(validate_model(&model));
    }
}
