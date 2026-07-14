//! Human-readable explanations for download failures.
//!
//! Raw `anyhow` chains ("tcp connect error ... os error 10013") are useless to
//! end users. This maps the common failure classes seen in the field to a
//! plain-language cause + fix, keeping the technical detail on a separate line.

/// Wrap a failed HTTP `send()` error with a plain-language explanation.
///
/// `what` names the thing being downloaded (e.g. "engine", "model").
pub fn explain_send_error(err: reqwest::Error, what: &str, url: &str) -> anyhow::Error {
    let chain = format!("{err:#}");

    // WSAEACCES: Windows denied the socket outright. In practice this is
    // firewall / security software blocking this specific executable, since
    // the same host is usually reachable from a browser or PowerShell.
    let explanation = if chain.contains("10013") {
        "Windows blocked this program from accessing the network (socket error 10013).\n\
         Your firewall or security software is denying outbound connections for\n\
         lmforge.exe specifically. Allow it in your firewall settings and retry."
    } else if err.is_timeout() {
        "The connection timed out. Check your internet connection, VPN, or proxy\n\
         and retry."
    } else if err.is_connect() {
        "Could not connect to the download server. Check your internet connection,\n\
         VPN/proxy settings, and firewall, then retry."
    } else {
        "The download request failed before any data was received. Check your\n\
         network and retry."
    };

    anyhow::Error::new(err).context(format!(
        "Could not download the {what} from {url}\n{explanation}"
    ))
}
