use axum::{
    body::Body,
    extract::{Query, State},
    response::IntoResponse,
};
use serde::Deserialize;

use super::AppState;
use crate::model::catalog::list_for_ui_from_dir;

#[derive(Deserialize)]
pub struct CatalogQuery {
    /// Optional format filter: `"mlx"`, `"safetensors"`, or `"gguf"`. Omit to get all three.
    pub format: Option<String>,
}

/// `GET /lf/catalog[?format=mlx|safetensors|gguf]`
///
/// Returns curated model shortcuts. Prefers the user's runtime `catalogs_dir`
/// files (so a customised catalog shows up in the UI) and falls back to the
/// bundled catalog per format. Each entry includes the shortcut key, resolved
/// HuggingFace repo, format, colon-split tags, and an inferred capability role.
pub async fn catalog_list(
    State(state): State<AppState>,
    Query(params): Query<CatalogQuery>,
) -> impl IntoResponse {
    let format = params.format.as_deref().unwrap_or("");
    let catalogs_dir = state.config.read().await.catalogs_dir();
    let entries = list_for_ui_from_dir(format, Some(catalogs_dir.as_path()));
    let body = serde_json::json!({ "entries": entries });

    Body::from(serde_json::to_vec(&body).unwrap_or_default()).into_response()
}
