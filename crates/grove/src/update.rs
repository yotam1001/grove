//! In-app update check against this repository's GitHub Releases.
//!
//! Ported in spirit from Tide's updater: only HTTPS GitHub release hosts, a
//! bounded download, an exact size and SHA-256 match before anything runs, a
//! private staging folder and a hand-off to the installer. Each release
//! publishes `GroveUpdate.json` naming its installer, size and SHA-256.
//!
//! Grove releases are not code-signed yet, so the checksum proves the file is
//! the one the release published, not who published it. Once signing exists
//! the manifest signature check belongs in `verify_manifest`, as in Tide.
//! While Grove itself is a pre-release it also accepts newer pre-releases.
use crate::{GroveSystem, GroveWindow};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use slint::ComponentHandle;
use std::{
    cell::RefCell,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

const RELEASES: &str = "https://api.github.com/repos/yotam1001/grove/releases?per_page=10";
pub const MANIFEST_ASSET: &str = "GroveUpdate.json";
const MAX_INSTALLER: u64 = 96 * 1024 * 1024;
const MAX_JSON: u64 = 512 * 1024;
const USER_AGENT: &str = concat!("Grove-Updater/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<GitHubAsset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

/// The release manifest written by .github/workflows/release.yml.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: String,
    installer: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    pub installer: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// A release version newer than `current`, or None. Pre-releases are offered
/// only while the installed Grove is itself a pre-release.
pub fn offered(tag: &str, prerelease: bool, current: &str) -> Option<Version> {
    let version = Version::parse(tag.strip_prefix('v')?).ok()?;
    let current = Version::parse(current).ok()?;
    let candidate_pre = prerelease || !version.pre.is_empty();
    if !version.build.is_empty() || (candidate_pre && current.pre.is_empty()) {
        return None;
    }
    (version > current).then_some(version)
}

/// The newest eligible, published release.
pub fn choose<'a>(
    releases: &'a [GitHubRelease],
    current: &str,
) -> Option<(&'a GitHubRelease, Version)> {
    releases
        .iter()
        .filter(|r| !r.draft)
        .filter_map(|r| offered(&r.tag_name, r.prerelease, current).map(|v| (r, v)))
        .max_by(|a, b| a.1.cmp(&b.1))
}

pub fn allowed_download(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https"
            && u.username().is_empty()
            && u.password().is_none()
            && u.port().is_none()
            && u.fragment().is_none()
            && matches!(
                u.host_str(),
                Some(
                    "github.com"
                        | "api.github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                )
            )
    })
}

fn installer_name_ok(name: &str, version: &Version) -> bool {
    name == format!("GroveSetup-{version}.exe")
}

/// Check the manifest's claims against the release that published it.
pub fn verify_manifest(
    bytes: &[u8],
    release: &GitHubRelease,
    version: &Version,
) -> Result<Release, String> {
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|_| "This release's update manifest is invalid.")?;
    if manifest.version != version.to_string() {
        return Err("The update manifest names a different version.".into());
    }
    if !installer_name_ok(&manifest.installer, version) {
        return Err("The update manifest names an unexpected installer.".into());
    }
    if manifest.sha256.len() != 64 || !manifest.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("This release has no valid installer checksum.".into());
    }
    if !(1..=MAX_INSTALLER).contains(&manifest.size) {
        return Err("The update installer size is invalid.".into());
    }
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == manifest.installer)
        .ok_or("The release does not contain its installer.")?;
    if asset.size != manifest.size {
        return Err("The installer size does not match the update manifest.".into());
    }
    if !allowed_download(&asset.browser_download_url) {
        return Err("Unexpected installer download address.".into());
    }
    Ok(Release {
        version: version.clone(),
        installer: manifest.installer,
        url: asset.browser_download_url.clone(),
        size: manifest.size,
        sha256: manifest.sha256.to_ascii_lowercase(),
    })
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .https_only(true)
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(15))
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || !allowed_download(attempt.url().as_str()) {
                attempt.error("Unexpected update redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|_| "Could not start the update check.".into())
}

fn get_bounded(
    client: &reqwest::blocking::Client,
    url: &str,
    limit: u64,
) -> Result<Vec<u8>, String> {
    if !allowed_download(url) {
        return Err("Unexpected update address.".into());
    }
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|_| "Could not reach GitHub. Try again later.")?;
    match response.status().as_u16() {
        200 => {}
        403 | 429 => return Err("GitHub is busy right now. Try again in a few minutes.".into()),
        _ => return Err("GitHub did not return the release information.".into()),
    }
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the release information.")?;
    if bytes.len() as u64 > limit {
        return Err("The release information is too large.".into());
    }
    Ok(bytes)
}

/// Network: list releases, pick the newest eligible one and check its manifest.
pub fn check() -> Result<Option<Release>, String> {
    let client = client()?;
    let releases: Vec<GitHubRelease> =
        serde_json::from_slice(&get_bounded(&client, RELEASES, MAX_JSON)?)
            .map_err(|_| "GitHub returned unreadable release information.")?;
    let Some((release, version)) = choose(&releases, env!("CARGO_PKG_VERSION")) else {
        return Ok(None);
    };
    let manifest = release
        .assets
        .iter()
        .find(|a| a.name == MANIFEST_ASSET)
        .ok_or("The newest release has no update manifest yet. Download it from GitHub instead.")?;
    let bytes = get_bounded(&client, &manifest.browser_download_url, 64 * 1024)?;
    verify_manifest(&bytes, release, &version).map(Some)
}

/// Updates are staged under %LOCALAPPDATA%\GroveMusic\updates, never roaming.
pub fn staging_dir() -> Result<PathBuf, String> {
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or("No local update folder is available.")?;
    Ok(root.join("GroveMusic").join("updates"))
}

/// Refuse links and junctions anywhere on the staging path (as Tide does).
fn plain(path: &Path) -> Result<(), String> {
    for part in path.ancestors() {
        match fs::symlink_metadata(part) {
            Ok(metadata) => {
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    metadata.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let linked = metadata.file_type().is_symlink();
                if linked {
                    return Err("Updates cannot use a linked folder or file.".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Could not inspect the update folder.".into()),
        }
    }
    Ok(())
}

/// Hash a reader up to `limit` bytes. Returns (lowercase hex, size).
fn hash(
    mut reader: impl Read,
    limit: u64,
    mut sink: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(String, u64), String> {
    let mut sha = Sha256::new();
    let mut count = 0u64;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let size = reader
            .read(&mut buffer)
            .map_err(|_| "The download was interrupted.")?;
        if size == 0 {
            break;
        }
        count += size as u64;
        if count > limit {
            return Err("The installer is larger than the release says.".into());
        }
        sha.update(&buffer[..size]);
        sink(&buffer[..size])?;
    }
    Ok((format!("{:x}", sha.finalize()), count))
}

/// Exact size and SHA-256 match, re-checked right before launching.
pub fn verify_file(path: &Path, release: &Release) -> Result<(), String> {
    plain(path)?;
    let file = File::open(path).map_err(|_| "The staged update is missing.")?;
    let (digest, size) = hash(file, release.size, |_| Ok(()))?;
    if size != release.size || digest != release.sha256 {
        return Err("The staged update does not match its published checksum.".into());
    }
    Ok(())
}

/// Download into the staging folder and verify. Returns the staged installer.
pub fn download(release: &Release, dir: &Path) -> Result<PathBuf, String> {
    use std::io::Write;
    plain(dir)?;
    fs::create_dir_all(dir).map_err(|_| "Could not create the update folder.")?;
    let target = dir.join(&release.installer);
    if verify_file(&target, release).is_ok() {
        return Ok(target);
    }
    let partial = dir.join(format!("{}.partial", release.installer));
    let _ = fs::remove_file(&partial);
    let client = client()?;
    if !allowed_download(&release.url) {
        return Err("Unexpected installer download address.".into());
    }
    let response = client
        .get(&release.url)
        .send()
        .map_err(|_| "Could not download the update. Try again later.")?;
    if response.status().as_u16() != 200 {
        return Err("GitHub did not return the installer.".into());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|_| "Could not write the update.")?;
    let result = hash(response, release.size, |chunk| {
        file.write_all(chunk)
            .map_err(|_| "Could not write the update.".into())
    });
    drop(file);
    let outcome = result.and_then(|(digest, size)| {
        if size != release.size || digest != release.sha256 {
            Err("The download did not match its published checksum. Nothing was installed.".into())
        } else {
            let _ = fs::remove_file(&target);
            fs::rename(&partial, &target).map_err(|_| "Could not stage the update.".into())
        }
    });
    if outcome.is_err() {
        let _ = fs::remove_file(&partial);
    }
    outcome.map(|()| target)
}

/// `--verify-update <GroveUpdate.json> <installer>`: offline release audit used
/// by the release workflow. Nothing is downloaded or executed.
pub fn verify_local(manifest: &Path, installer: &Path) -> Result<String, String> {
    let bytes = fs::read(manifest).map_err(|_| "Could not read the update manifest.")?;
    let name = installer
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid installer name.")?
        .to_owned();
    let size = fs::metadata(installer)
        .map_err(|_| "Could not read the installer.")?
        .len();
    let version =
        Version::parse(env!("CARGO_PKG_VERSION")).map_err(|_| "Invalid Grove version.")?;
    let release = GitHubRelease {
        tag_name: format!("v{version}"),
        draft: false,
        prerelease: !version.pre.is_empty(),
        assets: vec![GitHubAsset {
            browser_download_url: format!(
                "https://github.com/yotam1001/grove/releases/download/v{version}/{name}"
            ),
            name,
            size,
        }],
    };
    let release = verify_manifest(&bytes, &release, &version)?;
    verify_file(installer, &release)?;
    Ok(format!(
        "PASS update manifest, installer size and SHA-256 for Grove {version}"
    ))
}

#[derive(Default)]
struct Updater {
    release: Option<Release>,
    staged: Option<PathBuf>,
    busy: bool,
}

thread_local! {
    // UI-thread state; workers hand results back with upgrade_in_event_loop.
    static UPDATER: RefCell<Updater> = RefCell::default();
}

fn set(ui: &GroveWindow, state: i32, status: &str) {
    let system = ui.global::<GroveSystem>();
    system.set_update_state(state);
    system.set_update_status(status.into());
}

fn begin() -> bool {
    UPDATER.with(|u| !std::mem::replace(&mut u.borrow_mut().busy, true))
}

fn checked(ui: &GroveWindow, result: Result<Option<Release>, String>) {
    UPDATER.with(|u| {
        let mut u = u.borrow_mut();
        u.busy = false;
        match result {
            Ok(Some(release)) => {
                ui.global::<GroveSystem>()
                    .set_update_version(release.version.to_string().into());
                set(
                    ui,
                    3,
                    &format!(
                        "{} MB from GitHub Releases.",
                        release.size.div_ceil(1024 * 1024)
                    ),
                );
                u.staged = None;
                u.release = Some(release);
            }
            Ok(None) => set(ui, 2, "You have the newest Grove."),
            Err(error) => set(ui, 6, &error),
        }
    });
}

fn downloaded(ui: &GroveWindow, result: Result<PathBuf, String>) {
    UPDATER.with(|u| {
        let mut u = u.borrow_mut();
        u.busy = false;
        match result {
            Ok(path) => {
                u.staged = Some(path);
                set(
                    ui,
                    5,
                    "Checksum verified. Grove will close and the installer will open.",
                );
            }
            Err(error) => set(ui, 6, &error),
        }
    });
}

/// Wire the Settings buttons. Offline reviews never touch the network.
pub fn install(ui: &GroveWindow, offline: bool) {
    let system = ui.global::<GroveSystem>();
    let weak = ui.as_weak();
    system.on_check_updates(move || {
        let Some(ui) = weak.upgrade() else { return };
        if offline {
            set(&ui, 2, "Update checks are off in review mode.");
            return;
        }
        if !begin() {
            return;
        }
        set(&ui, 1, "Checking GitHub for a newer Grove…");
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let result = check();
            let _ = weak.upgrade_in_event_loop(move |ui| checked(&ui, result));
        });
    });
    let weak = ui.as_weak();
    system.on_download_update(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some(release) = UPDATER.with(|u| u.borrow().release.clone()) else {
            return;
        };
        if offline || !begin() {
            return;
        }
        set(&ui, 4, "Downloading and checking the installer…");
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let result = staging_dir().and_then(|dir| download(&release, &dir));
            let _ = weak.upgrade_in_event_loop(move |ui| downloaded(&ui, result));
        });
    });
    let weak = ui.as_weak();
    system.on_install_update(move || {
        let Some(ui) = weak.upgrade() else { return };
        let staged = UPDATER.with(|u| {
            let u = u.borrow();
            u.release.clone().zip(u.staged.clone())
        });
        let Some((release, path)) = staged else {
            return;
        };
        if offline {
            return;
        }
        // Re-verify immediately before running: the file must not have changed.
        match verify_file(&path, &release).and_then(|()| {
            std::process::Command::new(&path)
                .current_dir(path.parent().unwrap_or(Path::new(".")))
                .spawn()
                .map(|_| ())
                .map_err(|_| "Could not open the installer.".to_string())
        }) {
            Ok(()) => {
                let _ = ui.hide();
                let _ = slint::quit_event_loop();
            }
            Err(error) => set(&ui, 6, &error),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.into(),
            draft: false,
            prerelease,
            assets: vec![],
        }
    }

    #[test]
    fn pre_releases_are_offered_only_to_pre_release_installs() {
        assert_eq!(
            offered("v0.1.0-beta.2", true, "0.1.0-beta.1"),
            Some(Version::parse("0.1.0-beta.2").unwrap())
        );
        assert_eq!(
            offered("v0.1.0", false, "0.1.0-beta.1"),
            Some(Version::parse("0.1.0").unwrap())
        );
        assert_eq!(
            offered("v0.2.0-beta.1", true, "0.1.0"),
            None,
            "stable installs skip betas"
        );
        assert_eq!(
            offered("v0.1.0-beta.1", true, "0.1.0-beta.1"),
            None,
            "same version"
        );
        assert_eq!(offered("v0.0.9", false, "0.1.0-beta.1"), None, "older");
        assert_eq!(offered("0.2.0", false, "0.1.0"), None, "tags start with v");
        assert_eq!(
            offered("v0.2.0+build", false, "0.1.0"),
            None,
            "no build metadata"
        );
        assert_eq!(offered("vnext", false, "0.1.0"), None);
        let mut draft = release("v9.0.0", false);
        draft.draft = true;
        let list = [
            release("v0.1.0-beta.3", true),
            draft,
            release("v0.1.0-beta.2", true),
        ];
        assert_eq!(
            choose(&list, "0.1.0-beta.1").unwrap().1.to_string(),
            "0.1.0-beta.3"
        );
    }

    #[test]
    fn manifest_must_match_its_release() {
        let version = Version::parse("0.1.0-beta.2").unwrap();
        let mut gh = release("v0.1.0-beta.2", true);
        gh.assets.push(GitHubAsset {
            name: "GroveSetup-0.1.0-beta.2.exe".into(),
            browser_download_url: "https://github.com/yotam1001/grove/releases/download/v0.1.0-beta.2/GroveSetup-0.1.0-beta.2.exe".into(),
            size: 1234,
        });
        let good = br#"{"version":"0.1.0-beta.2","installer":"GroveSetup-0.1.0-beta.2.exe","size":1234,"sha256":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#;
        let ok = verify_manifest(good, &gh, &version).unwrap();
        assert_eq!(ok.sha256, "a".repeat(64));
        for bad in [
            r#"{"version":"0.1.0-beta.3","installer":"GroveSetup-0.1.0-beta.2.exe","size":1234,"sha256":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#,
            r#"{"version":"0.1.0-beta.2","installer":"..\\GroveSetup-0.1.0-beta.2.exe","size":1234,"sha256":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#,
            r#"{"version":"0.1.0-beta.2","installer":"GroveSetup-0.1.0-beta.2.exe","size":999,"sha256":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#,
            r#"{"version":"0.1.0-beta.2","installer":"GroveSetup-0.1.0-beta.2.exe","size":1234,"sha256":"not-a-digest"}"#,
            r#"{"version":"0.1.0-beta.2","installer":"GroveSetup-0.1.0-beta.2.exe","size":1234,"sha256":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","url":"https://evil.test"}"#,
        ] {
            assert!(
                verify_manifest(bad.as_bytes(), &gh, &version).is_err(),
                "{bad}"
            );
        }
        gh.assets[0].browser_download_url = "https://evil.test/GroveSetup-0.1.0-beta.2.exe".into();
        assert!(
            verify_manifest(good, &gh, &version).is_err(),
            "foreign host"
        );
    }

    #[test]
    fn only_github_release_hosts_are_allowed() {
        assert!(allowed_download(
            "https://github.com/yotam1001/grove/releases/download/v1/x.exe"
        ));
        assert!(allowed_download("https://objects.githubusercontent.com/x"));
        for url in [
            "http://github.com/x",
            "https://github.com.evil.test/x",
            "https://user@github.com/x",
            "https://github.com:444/x",
            "file:///C:/x",
        ] {
            assert!(!allowed_download(url), "{url}");
        }
    }

    #[test]
    fn staged_files_are_checked_by_size_and_digest() {
        // Canonical: macOS reaches its temp folder through the /var symlink,
        // which the staging path check rightly refuses.
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("grove-update-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("GroveSetup-0.1.0-beta.2.exe");
        fs::write(&path, b"synthetic installer").unwrap();
        let digest = format!("{:x}", Sha256::digest(b"synthetic installer"));
        let mut release = Release {
            version: Version::parse("0.1.0-beta.2").unwrap(),
            installer: "GroveSetup-0.1.0-beta.2.exe".into(),
            url: String::new(),
            size: 19,
            sha256: digest,
        };
        verify_file(&path, &release).unwrap();
        // A matching staged file is reused without any download.
        assert_eq!(download(&release, &dir).unwrap(), path);
        release.sha256 = "0".repeat(64);
        assert!(verify_file(&path, &release).is_err());
        release.size = 5;
        assert!(verify_file(&path, &release).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
