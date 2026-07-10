//! WASM bindings around [`knack_core::RegistryIndex::search`].
//!
//! This crate exists so a *static* knack registry deployment (an
//! `index.json` snapshot produced by `knack-registry build-static`,
//! served from an edge function like the Cloudflare Worker in
//! `examples/cloudflare-worker/`) can run the exact same
//! matching/ranking algorithm as a *live* `knack-registry serve`
//! instance, instead of a hand-written JS reimplementation that can
//! silently drift out of sync with the Rust scoring model (as
//! happened previously: the JS `/search` handler filtered results
//! but never computed or returned a relevance score at all).
//!
//! The exported [`search`] function takes the whole index as a JSON
//! string (the same bytes already fetched as `index.json`) plus a
//! query string, and returns a JSON-encoded array of `IndexedSkill`
//! with `score` populated — the same shape the live registry's
//! `/search` endpoint returns. Passing plain strings across the
//! WASM boundary (rather than marshalling structured `JsValue`s)
//! keeps this binding intentionally thin: all the actual logic stays
//! in `knack-core`, so there is exactly one implementation of the
//! search algorithm for both deployment modes to share.

use knack_core::RegistryIndex;
use wasm_bindgen::prelude::*;

/// Parses `index_json` as a [`RegistryIndex`], runs
/// [`RegistryIndex::search`] against `query`, and returns the ranked
/// results (each with `score` set) re-encoded as a JSON array.
///
/// Returns `Err` with a human-readable message if `index_json` isn't
/// a valid registry index or if re-serializing the results fails
/// (the latter should never happen in practice).
#[wasm_bindgen]
pub fn search(index_json: &str, query: &str) -> Result<String, String> {
    let index: RegistryIndex = serde_json::from_str(index_json)
        .map_err(|err| format!("invalid registry index JSON: {err}"))?;

    let results: Vec<_> = index
        .search(query)
        .into_iter()
        .map(|(skill, score)| {
            let mut skill = skill.clone();
            skill.score = Some(score);
            skill
        })
        .collect();

    serde_json::to_string(&results).map_err(|err| format!("failed to encode results: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_json_encoded_index_and_returns_scored_results() {
        let index_json = r#"{
            "skill": [
                {
                    "name": "pdf",
                    "namespace": "anthropics",
                    "description": "Work with PDF documents",
                    "source": "anthropics/pdf",
                    "tags": ["documents", "ocr"]
                },
                {
                    "name": "rust-code-review",
                    "description": "Review Rust code",
                    "source": "rust-code-review",
                    "tags": ["rust"]
                }
            ],
            "source": []
        }"#;

        let raw = search(index_json, "pdf").expect("search should succeed");
        let results: Vec<serde_json::Value> =
            serde_json::from_str(&raw).expect("results should be valid JSON");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["name"], "pdf");
        assert!(
            results[0]["score"].as_f64().unwrap() > 0.0,
            "the whole point of this binding is that a score comes back, \
             unlike the hand-written JS filter it replaces"
        );
    }

    #[test]
    fn rejects_malformed_index_json() {
        let err = search("not json", "pdf").expect_err("malformed JSON should be rejected");
        assert!(err.contains("invalid registry index JSON"));
    }
}
