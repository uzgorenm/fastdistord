//! Anonymous release metadata only. Never uses account tokens or installs code.
use std::time::Duration;

use serde::Deserialize;

pub const RELEASES_URL: &str = "https://github.com/uzgorenm/fastdistord/releases";
const API_URL: &str = "https://api.github.com/repos/uzgorenm/fastdistord/releases?per_page=20";
const MAX_RESPONSE_BYTES: usize = 128 * 1024;
const MAX_RELEASES: usize = 20;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    Current,
    Available {
        version: String,
        url: String,
    },
    Unavailable {
        message: String,
    },
}

impl UpdateState {
    pub fn label(&self) -> &str {
        match self {
            Self::Idle => "Updates have not been checked",
            Self::Checking => "Checking for updates…",
            Self::Current => "No newer release found",
            Self::Available { .. } => "An update is available",
            Self::Unavailable { message } => message,
        }
    }
}

fn unavailable(message: &str) -> UpdateState {
    UpdateState::Unavailable {
        message: message.into(),
    }
}

/// Call only after CheckForUpdates, or once at startup when the saved opt-in is on.
/// Includes published previews because Fastdistord's releases are prereleases.
pub async fn check_for_updates(current: &str) -> UpdateState {
    let Ok(client) = reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(10))
        .user_agent("Fastdistord-update-check")
        .build()
    else {
        return unavailable("Update checking is unavailable. Open releases in your browser.");
    };
    let Ok(mut response) = client
        .get(API_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
    else {
        return unavailable("Could not reach GitHub. Open releases in your browser.");
    };
    if let Some(state) = status_failure(response.status().as_u16()) {
        return state;
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return unavailable("GitHub's release response exceeded the size limit.");
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if append_bounded(&mut body, &chunk) => {}
            Ok(Some(_)) => {
                return unavailable("GitHub's release response exceeded the size limit.");
            }
            Ok(None) => break,
            Err(_) => return unavailable("Could not read GitHub's release response."),
        }
    }
    inspect_releases(current, &body)
}

fn status_failure(status: u16) -> Option<UpdateState> {
    match status {
        200 => None,
        401 | 404 => Some(unavailable(
            "GitHub did not expose these releases anonymously. Open releases in your browser to check access.",
        )),
        403 | 429 => Some(unavailable(
            "GitHub denied or rate-limited this check. Open releases in your signed-in browser.",
        )),
        _ => Some(unavailable(
            "GitHub did not return release metadata. Open releases in your browser.",
        )),
    }
}

fn append_bounded(body: &mut Vec<u8>, chunk: &[u8]) -> bool {
    if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
        return false;
    }
    body.extend_from_slice(chunk);
    true
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    draft: bool,
    published_at: Option<String>,
}

/// Display tags v0.02 and Cargo versions 0.0.2 identify the same release.
/// Parsing is integer based: v0.10 is newer than v0.09, never a float.
fn version(value: &str) -> Option<(u32, u32, u32)> {
    if value.len() > 32 {
        return None;
    }
    let value = value.strip_prefix('v').unwrap_or(value);
    let numbers = value
        .split('.')
        .map(|part| {
            (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| part.parse::<u32>().ok())
                .flatten()
        })
        .collect::<Option<Vec<_>>>()?;
    match numbers.as_slice() {
        [0, revision] => Some((0, 0, *revision)),
        [major, minor, patch] => Some((*major, *minor, *patch)),
        _ => None,
    }
}

fn inspect_releases(current: &str, body: &[u8]) -> UpdateState {
    let Some(current) = version(current) else {
        return unavailable("This build's version cannot be compared with releases.");
    };
    if body.len() > MAX_RESPONSE_BYTES {
        return unavailable("GitHub's release response exceeded the size limit.");
    }
    let Ok(releases) = serde_json::from_slice::<Vec<Release>>(body) else {
        return unavailable("GitHub returned unreadable release metadata.");
    };
    if releases.len() > MAX_RELEASES {
        return unavailable("GitHub's release list exceeded the entry limit.");
    }
    let mut newest = None;
    for release in releases {
        if release.draft || release.published_at.as_deref().is_none_or(str::is_empty) {
            continue;
        }
        let Some(parsed) = version(&release.tag_name) else {
            continue;
        };
        // The URL is both reconstructed and checked. No redirects, arbitrary
        // asset links, HTML, release-body text or download commands reach UI.
        let canonical = format!("{RELEASES_URL}/tag/{}", release.tag_name);
        if release.html_url != canonical {
            return unavailable("GitHub returned an unexpected release link.");
        }
        if newest.as_ref().is_none_or(|(v, _, _)| parsed > *v) {
            newest = Some((parsed, release.tag_name, canonical));
        }
    }
    match newest {
        Some((v, tag, url)) if v > current => UpdateState::Available {
            version: tag.trim_start_matches('v').into(),
            url,
        },
        Some(_) => UpdateState::Current,
        None => unavailable(
            "No comparable published release was returned. Open releases in your browser.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> serde_json::Value {
        serde_json::json!({"tag_name":tag,"html_url":format!("{RELEASES_URL}/tag/{tag}"),"draft":false,"published_at":"2026-10-09T12:00:00Z","prerelease":true})
    }
    fn check(items: Vec<serde_json::Value>) -> UpdateState {
        inspect_releases("0.02", &serde_json::to_vec(&items).unwrap())
    }

    #[test]
    fn display_and_package_versions_compare_as_integers() {
        assert_eq!(version("v0.02"), version("0.0.2"));
        assert!(version("v0.10") > version("v0.09"));
        for invalid in [
            "v0.02/evil",
            "0.2?x",
            "0.02#x",
            "0.-2",
            "1",
            "0.2.3.4",
            " 0.02",
            "0.4294967296",
        ] {
            assert_eq!(version(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn published_previews_are_compared_regardless_of_response_order() {
        assert_eq!(
            check(vec![release("v0.01"), release("v0.10"), release("v0.02")]),
            UpdateState::Available {
                version: "0.10".into(),
                url: format!("{RELEASES_URL}/tag/v0.10")
            }
        );
        assert_eq!(check(vec![release("v0.02")]), UpdateState::Current);
        let mut draft = release("v0.99");
        draft["draft"] = true.into();
        let mut unpublished = release("v0.98");
        unpublished["published_at"] = serde_json::Value::Null;
        assert_eq!(
            check(vec![draft, unpublished, release("v0.02")]),
            UpdateState::Current
        );
    }

    #[test]
    fn inaccessible_or_empty_releases_are_never_reported_as_current() {
        for status in [301, 401, 403, 404, 429, 500] {
            assert!(matches!(
                status_failure(status),
                Some(UpdateState::Unavailable { .. })
            ));
        }
        assert!(matches!(check(vec![]), UpdateState::Unavailable { .. }));
        assert!(matches!(
            inspect_releases("0.02", b"not json"),
            UpdateState::Unavailable { .. }
        ));
    }

    #[test]
    fn only_the_exact_repository_release_url_is_exposed() {
        for url in [
            "https://evil.example/v0.03",
            "https://github.com@evil.example/uzgorenm/fastdistord/releases/tag/v0.03",
            "https://github.com/uzgorenm/fastdistord/releases/tag/v0.03?redirect=evil",
            "http://github.com/uzgorenm/fastdistord/releases/tag/v0.03",
            "https://github.com/elsewhere/fastdistord/releases/tag/v0.03",
        ] {
            let mut item = release("v0.03");
            item["html_url"] = url.into();
            assert!(matches!(check(vec![item]), UpdateState::Unavailable { .. }));
        }
    }

    #[test]
    fn chunked_responses_and_release_counts_are_bounded() {
        let mut bytes = vec![0; MAX_RESPONSE_BYTES - 1];
        assert!(append_bounded(&mut bytes, &[0]));
        assert!(!append_bounded(&mut bytes, &[0]));
        assert_eq!(bytes.len(), MAX_RESPONSE_BYTES);
        assert!(matches!(
            check(vec![release("v0.03"); MAX_RELEASES + 1]),
            UpdateState::Unavailable { .. }
        ));
        assert!(matches!(
            inspect_releases("0.02", &vec![0; MAX_RESPONSE_BYTES + 1]),
            UpdateState::Unavailable { .. }
        ));
    }
}
