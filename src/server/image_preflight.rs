//! Image URL preflight: fetch remote `image_url.url` content blocks server-side
//! before forwarding to the engine, then rewrite them as `data:` URLs.
//!
//! Why: every engine (oMLX, llama.cpp, SGLang) ships its own HTTP client with
//! its own quirks. oMLX in particular sends an empty User-Agent which
//! Wikimedia / many CDNs reject with 403. When the fetch silently fails the
//! engine falls back to text-only and the model hallucinates instead of
//! describing the image. We cut that off at the door:
//!   1. Use a real browser-style User-Agent.
//!   2. Cap the per-image size at 20 MB (configurable by env).
//!   3. On any fetch failure (DNS, 4xx, 5xx, oversized), return a 400 with a
//!      precise message instead of letting the engine guess.
//!   4. On success, base64-encode and rewrite as `data:<ct>;base64,<...>` so
//!      the engine never has to make an outbound HTTP call.
//!
//! Skipped for `data:` URLs — they're already inline.

use axum::body::Body;
use axum::http::{Response, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use tracing::{debug, warn};

/// Default per-image cap. Override with `LMFORGE_IMAGE_MAX_BYTES`.
const DEFAULT_MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024; // 20 MB

/// Read the configured image-byte cap. Falls back to the default when the
/// env var is unset, malformed, or zero.
fn max_image_bytes() -> usize {
    std::env::var("LMFORGE_IMAGE_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_IMAGE_BYTES)
}

/// User-Agent used for all preflight fetches. Many CDNs (Wikimedia, CloudFront)
/// reject empty UAs with 403 — this matches a real browser request closely
/// enough to be accepted by every public host we've encountered.
const PREFLIGHT_UA: &str =
    "Mozilla/5.0 (compatible; LMForge/0.1 +https://github.com/phoenixtb/lmforge)";

/// Walk `messages[*].content[*]` and rewrite every `image_url.url` that points
/// to a remote `http(s)://` resource into an inline `data:` URL.
///
/// Returns `Err` with an HTTP response if any image fetch fails — the caller
/// should pass that response straight through to the client.
#[allow(clippy::result_large_err)]
pub async fn normalise_image_urls(body: &mut serde_json::Value) -> Result<(), Response<Body>> {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return Ok(());
    };

    let client = match reqwest::Client::builder()
        .user_agent(PREFLIGHT_UA)
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "image_preflight_init_failed",
                &format!("Failed to initialise preflight client: {e}"),
            ));
        }
    };

    let max_bytes = max_image_bytes();

    for msg in messages.iter_mut() {
        let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        for block in content.iter_mut() {
            rewrite_block(block, &client, max_bytes).await?;
        }
    }

    Ok(())
}

/// Rewrite a single content block in place. Only `image_url.url` blocks with
/// `http(s)://` URLs are touched — everything else passes through.
#[allow(clippy::result_large_err)]
async fn rewrite_block(
    block: &mut serde_json::Value,
    client: &reqwest::Client,
    max_bytes: usize,
) -> Result<(), Response<Body>> {
    let block_type = block
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if block_type != "image_url" && block_type != "input_image" {
        return Ok(());
    }

    // Two URL shapes per OpenAI spec:
    //   { "type": "image_url", "image_url": { "url": "..." } }   ← canonical
    //   { "type": "input_image", "image_url": "..." }            ← Responses API alias
    let url = block
        .get("image_url")
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else {
                v.get("url").and_then(|u| u.as_str()).map(String::from)
            }
        })
        .unwrap_or_default();

    if url.is_empty() {
        return Ok(());
    }
    if url.starts_with("data:") {
        super::metrics::observe_image("data_url");
        return Ok(());
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        // Unknown scheme — leave it for the engine to deal with (or reject).
        return Ok(());
    }

    debug!(url = %url, "preflight: fetching image");
    let resp = client.get(&url).send().await.map_err(|e| {
        warn!(url = %url, error = %e, "image preflight fetch failed");
        super::metrics::observe_image("rejected");
        error_response(
            StatusCode::BAD_REQUEST,
            "image_fetch_failed",
            &format!("Failed to fetch image at {url}: {e}"),
        )
    })?;

    let status = resp.status();
    if !status.is_success() {
        warn!(url = %url, status = %status, "image preflight returned non-2xx");
        super::metrics::observe_image("rejected");
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "image_fetch_failed",
            &format!(
                "Image fetch returned HTTP {} for {}. \
                 Some hosts (Wikimedia, GitHub web pages, etc.) require a direct asset URL \
                 rather than an HTML page URL — make sure this resolves to the image bytes.",
                status.as_u16(),
                url
            ),
        ));
    }

    // Capture the content-type so the data URL is faithful (matters for some
    // engines that sniff it). Default to image/jpeg when missing.
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
        .filter(|s| s.starts_with("image/"))
        .unwrap_or_else(|| "image/jpeg".to_string());

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "image_fetch_failed",
                &format!("Failed to read image body from {url}: {e}"),
            ));
        }
    };

    if bytes.len() > max_bytes {
        super::metrics::observe_image("rejected");
        return Err(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "image_too_large",
            &format!(
                "Image at {} is {} bytes, exceeds the {} byte limit. \
                 Resize the image or raise LMFORGE_IMAGE_MAX_BYTES.",
                url,
                bytes.len(),
                max_bytes
            ),
        ));
    }

    super::metrics::observe_image("accepted");
    let encoded = B64.encode(&bytes);
    let data_url = format!("data:{content_type};base64,{encoded}");
    debug!(
        url = %url,
        bytes = bytes.len(),
        content_type = %content_type,
        "preflight: rewrote remote image as data URL"
    );

    // Write back into the canonical OpenAI shape regardless of input shape.
    if let Some(obj) = block.as_object_mut() {
        obj.insert(
            "type".to_string(),
            serde_json::Value::String("image_url".to_string()),
        );
        obj.insert(
            "image_url".to_string(),
            serde_json::json!({ "url": data_url }),
        );
    }
    Ok(())
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "invalid_request_error",
            "param": "messages",
            "code": code,
        }
    });
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn data_url_passes_through_untouched() {
        let mut body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [{"type":"image_url","image_url":{"url":"data:image/png;base64,XYZ"}}]
            }]
        });
        normalise_image_urls(&mut body).await.unwrap();
        assert_eq!(
            body["messages"][0]["content"][0]["image_url"]["url"],
            "data:image/png;base64,XYZ"
        );
    }

    #[tokio::test]
    async fn text_only_request_is_a_noop() {
        let original = serde_json::json!({
            "messages": [{"role":"user","content":"hello"}]
        });
        let mut body = original.clone();
        normalise_image_urls(&mut body).await.unwrap();
        assert_eq!(body, original);
    }

    #[tokio::test]
    async fn http_image_is_fetched_and_rewritten_as_data_url() {
        let server = MockServer::start().await;
        let png_bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        Mock::given(method("GET"))
            .and(path("/cat.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(png_bytes.clone())
                    .insert_header("content-type", "image/png"),
            )
            .mount(&server)
            .await;

        let mut body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "what?"},
                    {"type": "image_url", "image_url": {"url": format!("{}/cat.png", server.uri())}}
                ]
            }]
        });
        normalise_image_urls(&mut body).await.unwrap();

        let url = body["messages"][0]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        let encoded = url.trim_start_matches("data:image/png;base64,");
        let decoded = B64.decode(encoded).unwrap();
        assert_eq!(decoded, png_bytes);
    }

    #[tokio::test]
    async fn non_2xx_response_returns_400_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/forbidden.jpg"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let mut body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [{"type":"image_url","image_url":{"url":format!("{}/forbidden.jpg", server.uri())}}]
            }]
        });
        let err = normalise_image_urls(&mut body)
            .await
            .expect_err("403 must surface as a 400");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn oversized_image_is_rejected_with_413() {
        // SAFETY: this test mutates a process-global env var. Cargo runs unit
        // tests in parallel by default — keep the value tiny so other tests
        // that fetch real-sized payloads aren't affected, and reset it after.
        // (We accept the small cross-test risk since no other test sets this var.)
        unsafe {
            std::env::set_var("LMFORGE_IMAGE_MAX_BYTES", "8");
        }

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/big.jpg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(vec![0u8; 64])
                    .insert_header("content-type", "image/jpeg"),
            )
            .mount(&server)
            .await;

        let mut body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [{"type":"image_url","image_url":{"url":format!("{}/big.jpg", server.uri())}}]
            }]
        });
        let err = normalise_image_urls(&mut body)
            .await
            .expect_err("oversized image must be rejected");
        assert_eq!(err.status(), StatusCode::PAYLOAD_TOO_LARGE);

        unsafe {
            std::env::remove_var("LMFORGE_IMAGE_MAX_BYTES");
        }
    }

    #[tokio::test]
    async fn input_image_responses_alias_is_normalised() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x.jpg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(vec![1u8, 2, 3])
                    .insert_header("content-type", "image/jpeg"),
            )
            .mount(&server)
            .await;

        let mut body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "input_image",
                    "image_url": format!("{}/x.jpg", server.uri())
                }]
            }]
        });
        normalise_image_urls(&mut body).await.unwrap();

        // After normalisation it should look like the canonical image_url block
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["type"], "image_url");
        assert!(
            block["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
    }
}
