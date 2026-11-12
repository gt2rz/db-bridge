//! Aviso de versiones nuevas: consulta la última Release publicada en GitHub como mucho una vez
//! al día. No descarga ni instala nada; la interfaz solo enlaza a la página de la Release.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/gt2rz/db-bridge/releases/latest";
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Release {
    /// Versión sin la `v` de la etiqueta, p. ej. `0.3.0`.
    pub version: String,
    /// Página de la Release, con las notas y los binarios.
    pub url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Si toca volver a consultar GitHub. Un reloj que va hacia atrás también fuerza la consulta.
pub fn is_due(last_check: u64, now: u64) -> bool {
    now < last_check || now - last_check >= CHECK_INTERVAL.as_secs()
}

/// Si `candidate` es posterior a `current`. Las versiones que no se pueden interpretar nunca lo
/// son, para no avisar por una etiqueta mal escrita.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (
        semver::Version::parse(candidate),
        semver::Version::parse(current),
    ) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => false,
    }
}

fn parse_release(body: &str) -> Result<Release> {
    let release: GithubRelease =
        serde_json::from_str(body).context("respuesta de GitHub inesperada")?;
    if release.draft || release.prerelease {
        bail!("la última Release no es estable");
    }
    let version = release.tag_name.trim_start_matches('v').to_string();
    semver::Version::parse(&version)
        .with_context(|| format!("etiqueta no válida: {}", release.tag_name))?;
    Ok(Release {
        version,
        url: release.html_url,
    })
}

/// Pide a GitHub la última Release estable. Bloquea: se llama desde un hilo de `tasks`.
pub fn fetch_latest() -> Result<Release> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(format!("db-bridge/{CURRENT_VERSION}"))
        .build()
        .into();
    let body = agent
        .get(LATEST_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("no se pudo consultar GitHub")?
        .body_mut()
        .read_to_string()
        .context("no se pudo leer la respuesta de GitHub")?;
    parse_release(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semantic_versions() {
        assert!(is_newer("0.0.2", "0.0.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(!is_newer("0.0.1", "0.0.1"));
        assert!(!is_newer("0.0.1", "0.0.2"));
        assert!(!is_newer("v0.0.2", "0.0.1"));
        assert!(!is_newer("basura", "0.0.1"));
    }

    #[test]
    fn checks_at_most_once_a_day() {
        let day = CHECK_INTERVAL.as_secs();
        assert!(is_due(0, now_secs()));
        assert!(!is_due(1_000, 1_000 + day - 1));
        assert!(is_due(1_000, 1_000 + day));
        assert!(is_due(1_000, 500));
    }

    #[test]
    fn parses_the_latest_release() {
        let body = r#"{
            "tag_name": "v0.3.0",
            "html_url": "https://github.com/gt2rz/db-bridge/releases/tag/v0.3.0",
            "draft": false,
            "prerelease": false
        }"#;
        assert_eq!(
            parse_release(body).unwrap(),
            Release {
                version: "0.3.0".to_string(),
                url: "https://github.com/gt2rz/db-bridge/releases/tag/v0.3.0".to_string(),
            }
        );
    }

    #[test]
    fn rejects_prereleases_and_invalid_tags() {
        let prerelease = r#"{"tag_name": "v0.3.0-rc.1", "html_url": "x", "prerelease": true}"#;
        assert!(parse_release(prerelease).is_err());
        let invalid = r#"{"tag_name": "latest", "html_url": "x"}"#;
        assert!(parse_release(invalid).is_err());
    }
}
