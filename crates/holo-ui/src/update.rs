//! Updates from GitHub releases. The tray checks `releases/latest`; for an installed copy
//! it downloads `holo-setup-<version>.exe`, checks it against the release's
//! `SHA256SUMS.txt` and runs it silently. The installer stops the daemon and the tray,
//! replaces the binaries and starts both again (`/relaunch=1`, see `installer/holo.iss`).
//! Portable copies only get a link to the release page.

use std::{
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::Context as _;
use sha2::{Digest, Sha256};

const REPO: &str = "HeyyCzer/holo-files";
/// `AppId` in `installer/holo.iss`: Inno Setup's uninstall key is `<AppId>_is1`.
const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{B31B6BE9-5ED5-4619-8672-5ADB8AB099F1}_is1";

pub type Version = (u64, u64, u64);

/// `1.2.3` or `v1.2.3`; anything else (pre-release suffixes included) is `None`.
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut it = s.split('.').map(|p| p.parse::<u64>().ok());
    let v = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(v)
}

pub fn current() -> Version {
    parse_version(env!("CARGO_PKG_VERSION")).expect("workspace version is x.y.z")
}

pub fn display((a, b, c): Version) -> String {
    format!("v{a}.{b}.{c}")
}

#[derive(Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    /// Release page, for portable copies.
    pub page: String,
    setup: Option<Asset>,
    sums: Option<Asset>,
}

#[derive(Debug, PartialEq)]
struct Asset {
    name: String,
    url: String,
}

/// The parts of a `releases/latest` response that matter here.
fn parse_release(json: &serde_json::Value) -> Option<Release> {
    if json["draft"].as_bool() == Some(true) || json["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let version = parse_version(json["tag_name"].as_str()?)?;
    let setup_name = format!("holo-setup-{}.{}.{}.exe", version.0, version.1, version.2);
    let asset = |name: &str| {
        json["assets"].as_array()?.iter().find_map(|a| {
            if a["name"].as_str()? != name {
                return None;
            }
            Some(Asset {
                name: name.to_string(),
                url: a["browser_download_url"].as_str()?.to_string(),
            })
        })
    };
    Some(Release {
        version,
        page: json["html_url"].as_str().unwrap_or_default().to_string(),
        setup: asset(&setup_name),
        sums: asset("SHA256SUMS.txt"),
    })
}

/// Hash of `name` in a `sha256sum`-style file (`<hex>  <name>`).
fn sha256_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, file) = l.trim().split_once(char::is_whitespace)?;
        (file.trim_start().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

fn hex_sha256(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn client() -> anyhow::Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("holo-ui/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()?)
}

fn get(client: &reqwest::blocking::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    client
        .get(url)
        .send()?
        .error_for_status()?
        .read_to_end(&mut body)?;
    Ok(body)
}

/// Whether this executable is the one the installer put in place (vs. a portable copy).
pub fn installed() -> bool {
    let Ok(out) = crate::actions::hidden(Command::new("reg").args([
        "query",
        UNINSTALL_KEY,
        "/v",
        "InstallLocation",
    ]))
    .stdin(Stdio::null())
    .output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(location) = text
        .lines()
        .find_map(|l| l.split_once("REG_SZ").map(|(_, v)| v.trim().to_string()))
    else {
        return false;
    };
    let dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf));
    dir.is_some_and(|d| same_dir(&d, Path::new(&location)))
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// Result of a check.
#[derive(Debug)]
pub enum Found {
    UpToDate,
    /// Installed copy: verified installer ready to run.
    Ready {
        version: Version,
        installer: PathBuf,
    },
    /// Portable copy (or a release without an installer): only the page.
    Manual {
        version: Version,
        page: String,
    },
}

/// Look for a newer release and, for an installed copy, download and verify its installer.
/// Blocking: run it off the UI thread.
pub fn check() -> anyhow::Result<Found> {
    let client = client()?;
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let res = client
        .get(&url)
        .send()
        .context("querying GitHub releases")?;
    // No published release yet.
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Found::UpToDate);
    }
    let json: serde_json::Value = res.error_for_status()?.json()?;
    let Some(release) = parse_release(&json).filter(|r| r.version > current()) else {
        return Ok(Found::UpToDate);
    };
    let (Some(setup), Some(sums), true) = (&release.setup, &release.sums, installed()) else {
        return Ok(Found::Manual {
            version: release.version,
            page: release.page,
        });
    };

    let sums = String::from_utf8(get(&client, &sums.url)?).context("SHA256SUMS.txt")?;
    let expected = sha256_for(&sums, &setup.name)
        .with_context(|| format!("{} missing from SHA256SUMS.txt", setup.name))?;
    let dir = std::env::temp_dir().join("holo-update");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&setup.name);
    // Already downloaded by an earlier check.
    if std::fs::read(&path).is_ok_and(|d| hex_sha256(&d) == expected) {
        return Ok(Found::Ready {
            version: release.version,
            installer: path,
        });
    }
    let data = get(&client, &setup.url).context("downloading the installer")?;
    let actual = hex_sha256(&data);
    anyhow::ensure!(
        actual == expected,
        "{}: SHA-256 {actual} does not match {expected}",
        setup.name
    );
    std::fs::write(&path, data)?;
    Ok(Found::Ready {
        version: release.version,
        installer: path,
    })
}

/// Run the installer silently. It closes this process (and the window, and the daemon) and
/// starts the tray again when done; the caller should exit right after.
pub fn install(installer: &Path) -> anyhow::Result<()> {
    Command::new(installer)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/SP-",
            "/relaunch=1",
        ])
        .spawn()
        .with_context(|| format!("starting {}", installer.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str) -> serde_json::Value {
        json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/{REPO}/releases/tag/{tag}"),
            "draft": false,
            "prerelease": false,
            "assets": [
                {"name": "holo-setup-0.2.0.exe", "browser_download_url": "https://x/setup"},
                {"name": "SHA256SUMS.txt", "browser_download_url": "https://x/sums"},
                {"name": "holo.exe", "browser_download_url": "https://x/holo"}
            ]
        })
    }

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(parse_version("v0.10.2"), Some((0, 10, 2)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("v1.2.3-beta"), None);
        assert!(parse_version("v0.10.0") > parse_version("v0.9.9"));
        assert_eq!(display((1, 2, 3)), "v1.2.3");
        assert_eq!(parse_version(env!("CARGO_PKG_VERSION")), Some(current()));
    }

    #[test]
    fn release_picks_the_matching_installer_and_sums() {
        let r = parse_release(&release("v0.2.0")).unwrap();
        assert_eq!(r.version, (0, 2, 0));
        assert_eq!(r.setup.unwrap().url, "https://x/setup");
        assert_eq!(r.sums.unwrap().url, "https://x/sums");
        assert!(r.page.ends_with("/v0.2.0"));
        // The installer name must carry the release's own version.
        assert!(parse_release(&release("v0.3.0")).unwrap().setup.is_none());
    }

    #[test]
    fn drafts_prereleases_and_odd_tags_are_ignored() {
        let mut j = release("v0.2.0");
        j["prerelease"] = json!(true);
        assert!(parse_release(&j).is_none());
        assert!(parse_release(&release("nightly")).is_none());
    }

    #[test]
    fn sums_lookup() {
        let sums = "ABC123  holo-setup-0.2.0.exe\r\ndef456 *holo.exe\n";
        assert_eq!(
            sha256_for(sums, "holo-setup-0.2.0.exe").as_deref(),
            Some("abc123")
        );
        assert_eq!(sha256_for(sums, "holo.exe").as_deref(), Some("def456"));
        assert_eq!(sha256_for(sums, "holo-ui.exe"), None);
        assert_eq!(
            hex_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn install_dir_comparison_ignores_case_and_trailing_slash() {
        assert!(same_dir(
            Path::new(r"C:\Users\a\AppData\Local\Programs\holo"),
            Path::new(r"c:\users\a\appdata\local\programs\holo\")
        ));
        assert!(!same_dir(Path::new(r"C:\holo"), Path::new(r"C:\holo2")));
    }
}
