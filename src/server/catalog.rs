use axum::{extract::{Query, State}, response::IntoResponse, body::Body};
use serde::Deserialize;

use super::AppState;
use crate::model::catalog::list_for_ui;

#[derive(Deserialize)]
pub struct CatalogQuery {
    /// Optional format filter: `"mlx"` or `"gguf"`. Omit to get both.
    pub format: Option<String>,
}

/// `GET /lf/catalog[?format=mlx|gguf]`
///
/// Returns all curated model shortcuts from the bundled catalog.
/// Each entry includes the shortcut key, resolved HuggingFace repo, format,
/// colon-split tags, and an inferred capability role.
pub async fn catalog_list(
    State(_state): State<AppState>,
    Query(params): Query<CatalogQuery>,
) -> impl IntoResponse {
    let format = params.format.as_deref().unwrap_or("");
    let entries = list_for_ui(format);
    let body = serde_json::json!({ "entries": entries });

    Body::from(serde_json::to_vec(&body).unwrap_or_default())
        .into_response()
}
