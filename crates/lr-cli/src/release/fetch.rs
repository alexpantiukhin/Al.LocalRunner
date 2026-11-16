//! The network side of `upgrade`: the only code of this program that opens an outgoing
//! connection to a download server. Everything it downloads is verified by [`super::update`]
//! before a byte of it is used.

use std::io::Read;
use std::time::Duration;

/// The repository releases are published in.
pub const REPOSITORY: &str = "alexpantiukhin/Al.LocalRunner";

const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(120);

/// Gets a URL as bytes; tests replace it with a map.
pub trait Fetch: Send + Sync {
    /// The body at `url`, or why it could not be read.
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// [`Fetch`] over HTTPS (rustls with the bundled web roots); a URL that is not `https://` is
/// refused.
pub struct UreqFetch;

impl Fetch for UreqFetch {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        if !url.starts_with("https://") {
            return Err(format!("{url}: only https downloads are allowed"));
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(TIMEOUT)
            .user_agent("localrunner-upgrade")
            .build();
        let response = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
        let mut body = Vec::new();
        response
            .into_reader()
            .take(MAX_DOWNLOAD + 1)
            .read_to_end(&mut body)
            .map_err(|e| format!("{url}: {e}"))?;
        if body.len() as u64 > MAX_DOWNLOAD {
            return Err(format!("{url}: larger than {} MB", MAX_DOWNLOAD >> 20));
        }
        Ok(body)
    }
}

/// The release archive for the platform this binary runs on, named as `release.yml` packs it.
pub fn asset_name() -> &'static str {
    if cfg!(windows) {
        "al-localrunner-windows-x64.zip"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "al-localrunner-macos-arm64.tar.gz"
        } else {
            "al-localrunner-macos-x64.tar.gz"
        }
    } else if cfg!(target_arch = "aarch64") {
        "al-localrunner-linux-arm64.tar.gz"
    } else {
        "al-localrunner-linux-x64.tar.gz"
    }
}

/// Whether `version` can name a release tag: digits and dots, with an optional `-suffix` of
/// letters, digits, dots and dashes.
pub fn valid_version(version: &str) -> bool {
    let (core, suffix) = version.split_once('-').unwrap_or((version, "x"));
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// Where a file of release `version` lives.
pub fn asset_url(version: &str, file: &str) -> String {
    format!("https://github.com/{REPOSITORY}/releases/download/v{version}/{file}")
}

/// The newest release that is neither a draft nor a pre-release, as `X.Y.Z`.
pub fn latest_version(fetch: &dyn Fetch) -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases");
    let body = fetch.get(&url)?;
    let releases: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("{url}: not JSON: {e}"))?;
    releases
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["draft"] != true && r["prerelease"] != true)
        .filter_map(|r| r["tag_name"].as_str())
        .map(|tag| tag.trim_start_matches('v'))
        .find(|v| valid_version(v))
        .map(String::from)
        .ok_or_else(|| "no release found".to_string())
}
