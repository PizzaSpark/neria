//! Small HTTP helpers shared by the Spotify / YouTube Music / lyrics clients.

use std::time::Duration;

use anyhow::{Result, bail};

pub const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0 Safari/537.36";

pub fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(BROWSER_UA)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()?)
}

/// GET a URL and return the body as text (errors on non-2xx).
pub fn get_text(url: &str) -> Result<String> {
    let resp = client()?.get(url).send()?;
    let status = resp.status();
    if !status.is_success() {
        bail!("HTTP {status} for {url}");
    }
    Ok(String::from_utf8_lossy(&resp.bytes()?).into_owned())
}

/// Percent-encode a query component (RFC 3986 unreserved characters kept).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes() {
        assert_eq!(super::encode("Daft Punk & Co/é"), "Daft%20Punk%20%26%20Co%2F%C3%A9");
    }
}
