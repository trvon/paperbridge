//! BM25F tunable parameters. Defaults match Robertson-Zaragoza recommended
//! starting points (`k1 = 1.2`, `b = 0.75`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOptions {
    /// BM25 saturation parameter. Higher values diminish the impact of term
    /// frequency more slowly.
    pub k1: f32,
    /// Per-field length-normalization (`b`) and weight. Index order is
    /// authoritative — `fields[i]` describes field id `i`.
    pub fields: Vec<FieldParams>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldParams {
    /// Stable field name (e.g. "title", "abstract", "full_text").
    pub name: String,
    /// Length-normalization strength for this field. `0.0` disables length
    /// normalization (treat all docs as the same length); `1.0` fully
    /// normalizes by `dl / avg_dl`.
    pub b: f32,
    /// Multiplicative weight applied to per-field tf contributions before the
    /// saturation step. Higher means matches in this field count for more.
    pub weight: f32,
}

impl BuildOptions {
    pub fn field_id(&self, name: &str) -> Option<u8> {
        self.fields
            .iter()
            .position(|f| f.name == name)
            .and_then(|i| u8::try_from(i).ok())
    }

    pub fn field_count(&self) -> usize {
        self.fields.len()
    }
}

/// Default field configuration for the paperseed corpus. Title and authors
/// get higher weight; full_text gets stronger length normalization since
/// long PDFs would otherwise dominate.
pub fn paperseed_defaults() -> BuildOptions {
    BuildOptions {
        k1: 1.2,
        fields: vec![
            FieldParams {
                name: "title".to_string(),
                b: 0.5,
                weight: 3.0,
            },
            FieldParams {
                name: "authors".to_string(),
                b: 0.5,
                weight: 2.0,
            },
            FieldParams {
                name: "venue".to_string(),
                b: 0.5,
                weight: 1.0,
            },
            FieldParams {
                name: "abstract".to_string(),
                b: 0.75,
                weight: 1.5,
            },
            FieldParams {
                name: "full_text".to_string(),
                b: 0.85,
                weight: 1.0,
            },
        ],
    }
}
