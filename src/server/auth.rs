// Auth middleware placeholder — will be expanded with full middleware in M9
// For v0.1: loopback accepts all, non-loopback requires api_key if configured

use tracing::debug;

/// Check if a request should be authorized.
/// Returns true if authorized, false if rejected.
pub fn check_auth(bind_address: &str, api_key: &Option<String>, auth_header: Option<&str>) -> bool {
    // Loopback: always accept
    if bind_address == "127.0.0.1" || bind_address == "localhost" || bind_address == "::1" {
        return true;
    }

    // Non-loopback with API key configured: require matching Bearer token
    if let Some(expected_key) = api_key {
        if let Some(header) = auth_header
            && let Some(token) = header.strip_prefix("Bearer ")
            && token == expected_key
        {
            return true;
        }
        debug!("Auth rejected: invalid or missing API key");
        return false;
    }

    // Non-loopback with no API key: accept (but user was warned at start)
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loopback_always_passes() {
        assert!(check_auth("127.0.0.1", &None, None));
        assert!(check_auth("127.0.0.1", &Some("secret".to_string()), None));
        assert!(check_auth("localhost", &None, None));
    }

    #[test]
    fn test_non_loopback_no_key_passes() {
        assert!(check_auth("0.0.0.0", &None, None));
    }

    #[test]
    fn test_non_loopback_with_key_requires_auth() {
        assert!(!check_auth("0.0.0.0", &Some("secret".to_string()), None));
        assert!(!check_auth(
            "0.0.0.0",
            &Some("secret".to_string()),
            Some("Bearer wrong")
        ));
        assert!(check_auth(
            "0.0.0.0",
            &Some("secret".to_string()),
            Some("Bearer secret")
        ));
    }
}
