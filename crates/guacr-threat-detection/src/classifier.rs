//! Local ONNX-based action effect classifier.
//!
//! A BERT-based text classifier that predicts risk severity from the LLM's
//! `reasoning` field. Acts as a fast, local second opinion alongside the
//! remote BAML API analysis. Currently enabled only for SSH protocol
//! (matching the Python reference implementation).
//!
//! Requires the `onnx-classifier` feature flag:
//! ```toml
//! guacr-threat-detection = { path = "...", features = ["onnx-classifier"] }
//! ```
//!
//! Model directory must contain:
//!   - model.onnx       (ONNX-exported BERT classification model)
//!   - tokenizer.json   (HuggingFace tokenizer configuration)

use std::path::{Path, PathBuf};

use log::{debug, info, warn};
use ndarray::Ix2;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;
use parking_lot::Mutex;
use tokenizers::Tokenizer;

use crate::threat::{RiskSource, ThreatLevel, ThreatResult};

/// Labels for the 4-class action effect classifier.
/// Order must match the model's output class indices.
const LABELS: &[&str] = &["Critical", "High", "Medium", "Low"];

/// Protocols where the classifier is enabled (matching Python reference).
const CLASSIFIER_ENABLED_PROTOCOLS: &[&str] = &["ssh"];

/// ONNX-based BERT text classifier for action effect severity.
///
/// Loads a fine-tuned BERT model exported to ONNX format and a HuggingFace
/// tokenizer. The model classifies text into 4 severity classes:
/// Critical, High, Medium, Low.
///
/// Thread-safe: Session is wrapped in Mutex because ort 2.0's `Session::run()`
/// requires `&mut self`. This is acceptable since inference is not on the hot
/// path (called once per command, not per keystroke).
pub struct ActionEffectClassifier {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    model_dir: PathBuf,
}

impl ActionEffectClassifier {
    /// Load the ONNX model and tokenizer from a directory.
    ///
    /// Returns `None` if the model directory doesn't exist or model files
    /// are missing (graceful degradation -- classifier is optional).
    pub fn new(model_dir: &str) -> Option<Self> {
        let model_dir_path = Path::new(model_dir);

        if !model_dir_path.exists() {
            warn!(
                "ONNX classifier model directory not found: {} -- classifier disabled",
                model_dir
            );
            return None;
        }

        let model_path = model_dir_path.join("model.onnx");
        let tokenizer_path = model_dir_path.join("tokenizer.json");

        if !model_path.exists() {
            warn!(
                "ONNX model file not found: {} -- classifier disabled",
                model_path.display()
            );
            return None;
        }

        if !tokenizer_path.exists() {
            warn!(
                "Tokenizer file not found: {} -- classifier disabled",
                tokenizer_path.display()
            );
            return None;
        }

        // Build the ONNX Runtime session.
        // Level3 enables all graph optimizations (constant folding, node fusion, etc.).
        // Single intra-thread keeps latency predictable for single-request inference.
        let session = match Session::builder() {
            Ok(builder) => {
                let builder = match builder.with_optimization_level(GraphOptimizationLevel::Level3)
                {
                    Ok(b) => b,
                    Err(e) => {
                        warn!(
                            "Failed to set ONNX optimization level: {} -- classifier disabled",
                            e
                        );
                        return None;
                    }
                };
                let builder = match builder.with_intra_threads(1) {
                    Ok(b) => b,
                    Err(e) => {
                        warn!(
                            "Failed to set ONNX thread count: {} -- classifier disabled",
                            e
                        );
                        return None;
                    }
                };
                match builder.commit_from_file(&model_path) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(
                            "Failed to load ONNX model from {}: {} -- classifier disabled",
                            model_path.display(),
                            e
                        );
                        return None;
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to create ONNX session builder: {} -- classifier disabled",
                    e
                );
                return None;
            }
        };

        // Load the HuggingFace tokenizer.
        let tokenizer = match Tokenizer::from_file(&tokenizer_path) {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    "Failed to load tokenizer from {}: {} -- classifier disabled",
                    tokenizer_path.display(),
                    e
                );
                return None;
            }
        };

        info!("ONNX action effect classifier loaded from {}", model_dir);

        Some(Self {
            session: Mutex::new(session),
            tokenizer,
            model_dir: model_dir_path.to_path_buf(),
        })
    }

    /// Check if the classifier is enabled for the given protocol.
    pub fn is_enabled_for_protocol(protocol: &str) -> bool {
        CLASSIFIER_ENABLED_PROTOCOLS.contains(&protocol)
    }

    /// Classify a text string and return the predicted label and confidence.
    ///
    /// The input text is typically the `reasoning` field from an LLM analysis.
    /// Returns `None` if tokenization or inference fails (graceful degradation).
    pub fn predict(&self, text: &str) -> Option<(String, f64)> {
        if text.is_empty() {
            return None;
        }

        // Tokenize: encode adds [CLS] and [SEP] automatically.
        let encoding = match self.tokenizer.encode(text, true) {
            Ok(e) => e,
            Err(e) => {
                warn!("Tokenization failed: {}", e);
                return None;
            }
        };

        let seq_len = encoding.get_ids().len();
        if seq_len == 0 {
            return None;
        }

        // Convert u32 token data to i64 (ONNX BERT models expect int64 tensors).
        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let token_type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();

        // Build input tensors: shape [1, seq_len] (batch size = 1).
        let ids_tensor = match TensorRef::from_array_view(([1_usize, seq_len], &*input_ids)) {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to create input_ids tensor: {}", e);
                return None;
            }
        };
        let mask_tensor = match TensorRef::from_array_view(([1_usize, seq_len], &*attention_mask)) {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to create attention_mask tensor: {}", e);
                return None;
            }
        };
        let type_tensor = match TensorRef::from_array_view(([1_usize, seq_len], &*token_type_ids)) {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to create token_type_ids tensor: {}", e);
                return None;
            }
        };

        // Run inference with named inputs.
        // Mutex is fine here: inference is not on the hot path.
        let mut session = self.session.lock();
        let outputs = match session.run(ort::inputs![
            "input_ids" => ids_tensor,
            "attention_mask" => mask_tensor,
            "token_type_ids" => type_tensor
        ]) {
            Ok(o) => o,
            Err(e) => {
                warn!("ONNX inference failed: {}", e);
                return None;
            }
        };

        // Extract logits: shape [1, num_classes].
        let logits_array = match outputs[0].try_extract_array::<f32>() {
            Ok(arr) => match arr.into_dimensionality::<Ix2>() {
                Ok(a) => a,
                Err(e) => {
                    warn!("Logits tensor is not 2-dimensional: {}", e);
                    return None;
                }
            },
            Err(e) => {
                warn!("Failed to extract logits tensor: {}", e);
                return None;
            }
        };

        let logits: Vec<f32> = logits_array.row(0).to_vec();
        let probabilities = softmax(&logits);
        let predicted_idx = argmax(&probabilities);

        let label = LABELS.get(predicted_idx).unwrap_or(&"Unknown").to_string();
        let confidence = probabilities[predicted_idx] as f64;

        debug!(
            "ONNX classifier: '{}...' -> {} (confidence: {:.3})",
            &text[..text.len().min(50)],
            label,
            confidence
        );

        Some((label, confidence))
    }

    /// Update a ThreatResult's risk score based on classifier prediction.
    ///
    /// If the classifier predicts a different severity than the LLM, updates
    /// the risk_score and risk_level_source. Returns None if the classifier
    /// can't produce a prediction.
    pub fn update_threat_result(
        &self,
        threat: &mut ThreatResult,
        protocol: &str,
    ) -> Option<(String, f64)> {
        if !Self::is_enabled_for_protocol(protocol) {
            return None;
        }

        // Extract reasoning text from the description (format: "category - reasoning")
        let text = &threat.description;
        if text.is_empty() {
            return None;
        }

        let (label, confidence) = self.predict(text)?;

        // Convert label to ThreatLevel
        let classified_level = match label.to_lowercase().as_str() {
            "critical" => ThreatLevel::Critical,
            "high" => ThreatLevel::High,
            "medium" => ThreatLevel::Medium,
            "low" => ThreatLevel::Low,
            _ => return None,
        };

        let classified_score = classified_level.to_risk_score();

        // Only override if the classifier disagrees with the LLM
        if classified_level != threat.level {
            debug!(
                "ONNX classifier disagrees: LLM={:?} (score {}), classifier={:?} (score {}, confidence {:.3})",
                threat.level, threat.risk_score, classified_level, classified_score, confidence
            );

            threat.risk_score = classified_score;
            threat.level = classified_level;
            threat.risk_level_source = RiskSource::ActionEffectClassifier;
        }

        Some((label, confidence))
    }

    /// Get the model directory path.
    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }
}

/// Numerically stable softmax over a slice of logits.
fn softmax(logits: &[f32]) -> Vec<f32> {
    let max = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = logits.iter().map(|&x| (x - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    exps.iter().map(|&e| e / sum).collect()
}

/// Return the index of the maximum value in a slice.
fn argmax(values: &[f32]) -> usize {
    values
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_softmax_basic() {
        let logits = vec![2.0, 1.0, 0.1];
        let probs = softmax(&logits);
        assert!(probs.iter().all(|&p| p > 0.0));
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
        assert!(probs[0] > probs[1]);
        assert!(probs[1] > probs[2]);
    }

    #[test]
    fn test_softmax_numerical_stability() {
        // Large logits that would overflow without max subtraction.
        let logits = vec![1000.0, 1000.0, 1000.0];
        let probs = softmax(&logits);
        for &p in &probs {
            assert!((p - 1.0 / 3.0).abs() < 1e-5, "got {} expected ~0.333", p);
        }
    }

    #[test]
    fn test_softmax_single_element() {
        let probs = softmax(&[5.0]);
        assert!((probs[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_argmax() {
        assert_eq!(argmax(&[0.1, 0.7, 0.15, 0.05]), 1);
        assert_eq!(argmax(&[0.9, 0.05, 0.03, 0.02]), 0);
        assert_eq!(argmax(&[0.1, 0.2, 0.3, 0.4]), 3);
    }

    #[test]
    fn test_argmax_equal() {
        // When values are equal, returns the first occurrence
        let idx = argmax(&[0.5, 0.5]);
        assert!(idx == 0 || idx == 1);
    }

    #[test]
    fn test_is_enabled_for_protocol() {
        assert!(ActionEffectClassifier::is_enabled_for_protocol("ssh"));
        assert!(!ActionEffectClassifier::is_enabled_for_protocol("rdp"));
        assert!(!ActionEffectClassifier::is_enabled_for_protocol("vnc"));
        assert!(!ActionEffectClassifier::is_enabled_for_protocol("telnet"));
    }

    #[test]
    fn test_labels_order() {
        assert_eq!(LABELS.len(), 4);
        assert_eq!(LABELS[0], "Critical");
        assert_eq!(LABELS[1], "High");
        assert_eq!(LABELS[2], "Medium");
        assert_eq!(LABELS[3], "Low");
    }

    #[test]
    fn test_new_missing_dir() {
        let classifier = ActionEffectClassifier::new("/nonexistent/path/to/model");
        assert!(classifier.is_none());
    }
}
