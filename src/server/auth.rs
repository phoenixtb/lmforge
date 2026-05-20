//! Request authentication middleware.
//!
//! Decision matrix (per request):
//!
//! | Source IP                | `api_key` set? | `Authorization` header | Result    |
//! |--------------------------|----------------|------------------------|-----------|
//! | matches `trusted_networks` | any         | any                    | allow     |
//! | outside trusted          | no             | any                    | 401       |
//! | outside trusted          | yes            | matches `Bearer <key>` | allow     |
//! | outside trusted          | yes            | missing / wrong        | 401       |
//! | any                      | any            | any                    | allow if `unsafe_disable_auth=true` |
//!
//! `/lf/shutdown` is enforced loopback-only inside its own handler on top of
//! this layer (defense in depth).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, Response, StatusCode, header};
use axum::middleware::Next;
use axum::response::IntoResponse;
use ipnetwork::IpNetwork;
use tracing::{debug, warn};

/// Pre-parsed auth policy. Built once at startup, shared via `Arc`.
#[derive(Debug, Clone)]
pub struct AuthPolicy {
    pub api_key: Option<String>,
    pub trusted_networks: Vec<IpNetwork>,
    pub unsafe_disable_auth: bool,
}

impl AuthPolicy {
    /// Build the policy from raw config values. Invalid CIDR strings are logged
    /// and silently dropped — startup must not fail on a typo'd allowlist.
    pub fn from_config(
        api_key: Option<String>,
        trusted_networks: &[String],
        unsafe_disable_auth: bool,
    ) -> Self {
        let trusted_networks = trusted_networks
            .iter()
            .filter_map(|s| match s.parse::<IpNetwork>() {
                Ok(n) => Some(n),
                Err(e) => {
                    warn!(cidr = %s, error = %e, "Skipping invalid CIDR in trusted_networks");
                    None
                }
            })
            .collect();
        Self {
            api_key,
            trusted_networks,
            unsafe_disable_auth,
        }
    }

    /// Return true if the IP is in any trusted CIDR range.
    pub fn is_trusted(&self, ip: IpAddr) -> bool {
        self.trusted_networks.iter().any(|n| n.contains(ip))
    }

    /// Evaluate the decision matrix for a single request.
    pub fn allow(&self, client_ip: IpAddr, auth_header: Option<&str>) -> bool {
        if self.unsafe_disable_auth {
            return true;
        }
        if self.is_trusted(client_ip) {
            return true;
        }
        match self.api_key.as_deref() {
            None => false, // outside trusted, no token configured → reject
            Some(expected) => matches!(
                auth_header.and_then(|h| h.strip_prefix("Bearer ")),
                Some(token) if token == expected
            ),
        }
    }
}

/// Axum middleware: rejects requests that fail the policy with a 401.
///
/// Wire with:
///   `.layer(axum::middleware::from_fn_with_state(policy.clone(), auth_layer))`
///
/// Requires the server to be started with `into_make_service_with_connect_info::<SocketAddr>()`
/// so `ConnectInfo<SocketAddr>` is available; without it we deny by default.
///
/// We pull `ConnectInfo` out of request extensions manually instead of using
/// it as a typed extractor so the function's signature stays compatible with
/// `axum::middleware::from_fn_with_state` regardless of axum version drift.
pub async fn auth_layer(
    State(policy): State<Arc<AuthPolicy>>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    // /health and /metrics stay unauthenticated so liveness probes, load
    // balancers, uptime monitors and Prometheus scrapers can reach the
    // daemon without leaking credentials. Operators that don't want to
    // expose /metrics publicly should bind 127.0.0.1 or scrape over a
    // private network — same trust boundary as /health.
    //
    // /ui/* serves static dashboard assets (HTML/CSS/JS bundle). The actual
    // privileged surface is /lf/* and /v1/*, which the JS calls separately
    // and which still go through this middleware. Bypassing /ui/* lets
    // browsers load the dashboard before the user has supplied credentials.
    let path = req.uri().path();
    if path == "/health" || path == "/metrics" || path.starts_with("/ui/") || path == "/ui" {
        return next.run(req).await;
    }

    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());

    let client_ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip())
        .unwrap_or_else(|| {
            // No ConnectInfo means the server wasn't started with
            // `into_make_service_with_connect_info::<SocketAddr>()`.
            // Fail closed — assume an arbitrary external address.
            warn!("auth_layer: no ConnectInfo on request — denying by default");
            IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0))
        });

    if policy.allow(client_ip, auth_header.as_deref()) {
        next.run(req).await
    } else {
        debug!(?client_ip, "Auth rejected");
        super::metrics::observe_auth_rejection();
        let body = r#"{"error":{"message":"Unauthorized: this address is not in trusted_networks and a valid Bearer token was not supplied.","type":"unauthorized","code":"missing_or_invalid_api_key"}}"#;
        (
            StatusCode::UNAUTHORIZED,
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::WWW_AUTHENTICATE, "Bearer"),
            ],
            body,
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn pol(api_key: Option<&str>, nets: &[&str], unsafe_off: bool) -> AuthPolicy {
        AuthPolicy::from_config(
            api_key.map(String::from),
            &nets.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            unsafe_off,
        )
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn loopback_v4_is_trusted_by_default() {
        let p = pol(None, &["127.0.0.0/8"], false);
        assert!(p.is_trusted(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(p.allow(ip("127.0.0.1"), None));
    }

    #[test]
    fn lan_192_168_is_trusted_by_default() {
        let p = pol(None, &["192.168.0.0/16"], false);
        assert!(p.allow(ip("192.168.1.42"), None));
    }

    #[test]
    fn public_ip_with_no_key_rejected() {
        let p = pol(None, &["127.0.0.0/8"], false);
        assert!(!p.allow(ip("8.8.8.8"), None));
    }

    #[test]
    fn public_ip_with_correct_token_allowed() {
        let p = pol(Some("s3cret"), &["127.0.0.0/8"], false);
        assert!(p.allow(ip("8.8.8.8"), Some("Bearer s3cret")));
    }

    #[test]
    fn public_ip_with_wrong_token_rejected() {
        let p = pol(Some("s3cret"), &["127.0.0.0/8"], false);
        assert!(!p.allow(ip("8.8.8.8"), Some("Bearer wrong")));
    }

    #[test]
    fn public_ip_with_missing_token_rejected_when_key_set() {
        let p = pol(Some("s3cret"), &["127.0.0.0/8"], false);
        assert!(!p.allow(ip("8.8.8.8"), None));
    }

    #[test]
    fn unsafe_disable_allows_everyone() {
        let p = pol(None, &[], true);
        assert!(p.allow(ip("8.8.8.8"), None));
    }

    #[test]
    fn invalid_cidr_strings_are_dropped() {
        let p = pol(None, &["not-a-cidr", "127.0.0.0/8", "999.999/8"], false);
        assert_eq!(p.trusted_networks.len(), 1);
        assert!(p.allow(ip("127.0.0.1"), None));
        assert!(!p.allow(ip("8.8.8.8"), None));
    }

    #[test]
    fn ipv6_loopback_is_trusted() {
        let p = pol(None, &["::1/128"], false);
        assert!(p.allow(ip("::1"), None));
    }

    #[test]
    fn rfc1918_default_set_covers_typical_home_lan() {
        let defaults = crate::config::default_trusted_networks();
        let p = pol(
            None,
            &defaults.iter().map(String::as_str).collect::<Vec<_>>(),
            false,
        );
        assert!(p.allow(ip("192.168.1.5"), None));
        assert!(p.allow(ip("10.0.0.42"), None));
        assert!(p.allow(ip("172.20.0.99"), None));
        assert!(!p.allow(ip("8.8.8.8"), None));
    }

    #[test]
    fn loopback_bind_with_no_key_still_works() {
        // Daemon binds 127.0.0.1, no api_key — every request comes from loopback
        // and must be allowed.
        let p = pol(
            None,
            &crate::config::default_trusted_networks()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            false,
        );
        assert!(p.allow(ip("127.0.0.1"), None));
    }
}
