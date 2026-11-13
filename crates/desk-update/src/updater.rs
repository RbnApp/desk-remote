//! Check, download and verify: the [`Updater`] state machine.

use crate::release::{default_feed_url, parse_manifest, parse_release, ReleaseInfo};
use crate::status::UpdateStatus;
use crate::{MAX_INSTALLER_BYTES, MAX_MANIFEST_BYTES, MAX_RELEASE_BYTES};
use desk_error::{DeskError, DeskResult};
use desk_version::SemVer;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Which installer a consumer updates itself with. A component never touches the other's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Agent,
    Panel,
}

impl Component {
    /// Installer file name of this component for `version`.
    #[must_use]
    pub fn asset_name(self, version: &str) -> String {
        match self {
            Self::Agent => desk_version::agent_asset_name(version),
            Self::Panel => desk_version::panel_asset_name(version),
        }
    }

    /// Start of every installer file name of this component (used to clear stale downloads).
    fn file_prefix(self) -> String {
        self.asset_name("").trim_end_matches(".exe").to_string()
    }
}

/// Network access of the updater (a fake in tests, `desk_transport::http` in production).
pub trait Transport: Send + Sync {
    /// GET into memory, at most `max_bytes`.
    fn get(&self, url: &str, max_bytes: u64) -> DeskResult<Vec<u8>>;
    /// GET into `dest`; `progress(received, total)` after every chunk.
    fn download(
        &self,
        url: &str,
        dest: &Path,
        max_bytes: u64,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> DeskResult<u64>;
}

/// The real network.
#[derive(Debug, Clone, Copy, Default)]
pub struct NetTransport;

impl Transport for NetTransport {
    fn get(&self, url: &str, max_bytes: u64) -> DeskResult<Vec<u8>> {
        desk_transport::http::get_bytes(url, max_bytes)
    }

    fn download(
        &self,
        url: &str,
        dest: &Path,
        max_bytes: u64,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> DeskResult<u64> {
        desk_transport::http::download(url, dest, max_bytes, progress)
    }
}

type Observer = Box<dyn Fn(&UpdateStatus) + Send + Sync>;

/// Progress is reported to the observer at most once per this many bytes.
const PROGRESS_STEP: u64 = 256 * 1024;

/// Update client of one component.
pub struct Updater<T: Transport> {
    component: Component,
    current: SemVer,
    feed_url: String,
    work_dir: PathBuf,
    transport: T,
    status: Mutex<UpdateStatus>,
    observer: Mutex<Option<Observer>>,
}

fn lock<V>(m: &Mutex<V>) -> MutexGuard<'_, V> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn failure(message: impl Into<String>) -> DeskError {
    DeskError::Update(message.into())
}

impl<T: Transport> Updater<T> {
    /// `current` is the running version, `feed_url` overrides the GitHub feed (`None` =
    /// production), `work_dir` is where installers are downloaded.
    pub fn new(
        component: Component,
        current: SemVer,
        feed_url: Option<String>,
        work_dir: PathBuf,
        transport: T,
    ) -> Self {
        Self {
            component,
            current,
            feed_url: feed_url.unwrap_or_else(default_feed_url),
            work_dir,
            transport,
            status: Mutex::new(UpdateStatus::NoUpdate),
            observer: Mutex::new(None),
        }
    }

    /// Registers the function called with every new status (state events for a UI).
    pub fn on_status(&self, observer: impl Fn(&UpdateStatus) + Send + Sync + 'static) {
        *lock(&self.observer) = Some(Box::new(observer));
    }

    /// Current status.
    #[must_use]
    pub fn status(&self) -> UpdateStatus {
        lock(&self.status).clone()
    }

    fn set(&self, status: UpdateStatus) {
        *lock(&self.status) = status.clone();
        if let Some(observer) = lock(&self.observer).as_ref() {
            observer(&status);
        }
    }

    fn fail<V>(&self, error: DeskError) -> DeskResult<V> {
        self.set(UpdateStatus::Failed {
            message: error.to_string(),
        });
        Err(error)
    }

    /// Asks the feed for the latest release. Ends as `NoUpdate`, `Available` or `Failed`.
    pub fn check(&self) -> DeskResult<UpdateStatus> {
        self.set(UpdateStatus::Checking);
        let found = self
            .transport
            .get(&self.feed_url, MAX_RELEASE_BYTES)
            .and_then(|json| parse_release(&json, self.component))
            .and_then(|release| self.vet_urls(release));
        match found {
            Ok(release) if release.version > self.current => {
                tracing::info!(current = %self.current, latest = %release.version, "update available");
                self.set(UpdateStatus::Available(release));
            }
            Ok(_) => self.set(UpdateStatus::NoUpdate),
            Err(e) => return self.fail(e),
        }
        Ok(self.status())
    }

    /// An HTTPS feed may only point at HTTPS files (no downgrade through the release data).
    fn vet_urls(&self, release: ReleaseInfo) -> DeskResult<ReleaseInfo> {
        if self.feed_url.starts_with("https://")
            && !(release.asset_url.starts_with("https://")
                && release.manifest_url.starts_with("https://"))
        {
            return Err(failure("release links must use https"));
        }
        Ok(release)
    }

    /// Downloads the installer of the available release and checks its SHA-256 against the
    /// manifest. Returns the path of the verified file; on any failure no installer is left.
    pub fn download(&self) -> DeskResult<PathBuf> {
        let release = match self.status() {
            UpdateStatus::Available(r) | UpdateStatus::Ready { release: r, .. } => r,
            _ => return Err(failure("no update is available to download")),
        };
        match self.fetch_verified(&release) {
            Ok(path) => {
                self.set(UpdateStatus::Ready {
                    release,
                    path: path.clone(),
                });
                Ok(path)
            }
            Err(e) => self.fail(e),
        }
    }

    /// [`check`](Self::check), then [`download`](Self::download) when a newer release exists.
    pub fn check_and_download(&self) -> DeskResult<UpdateStatus> {
        if matches!(self.check()?, UpdateStatus::Available(_)) {
            self.download()?;
        }
        Ok(self.status())
    }

    fn fetch_verified(&self, release: &ReleaseInfo) -> DeskResult<PathBuf> {
        std::fs::create_dir_all(&self.work_dir)?;
        self.clear_stale(&release.asset_name);
        let manifest = self
            .transport
            .get(&release.manifest_url, MAX_MANIFEST_BYTES)?;
        let manifest =
            String::from_utf8(manifest).map_err(|_| failure("the manifest is not valid text"))?;
        let expected = parse_manifest(&manifest, &release.asset_name)?;

        let final_path = self.work_dir.join(&release.asset_name);
        if final_path.is_file() && file_sha256(&final_path)? == expected {
            return Ok(final_path);
        }
        let partial = self
            .work_dir
            .join(format!("{}.partial", release.asset_name));
        let mut last_reported = 0u64;
        let result = self.transport.download(
            &release.asset_url,
            &partial,
            MAX_INSTALLER_BYTES,
            &mut |received, total| {
                if received.saturating_sub(last_reported) >= PROGRESS_STEP
                    || Some(received) == total
                {
                    last_reported = received;
                    self.set(UpdateStatus::Downloading {
                        release: release.clone(),
                        received,
                        total,
                    });
                }
            },
        );
        let verified = result.and_then(|size| {
            if release.asset_size != 0 && size != release.asset_size {
                return Err(failure(format!(
                    "downloaded {size} bytes, the release announces {}",
                    release.asset_size
                )));
            }
            if file_sha256(&partial)? != expected {
                return Err(failure("SHA-256 does not match the release manifest"));
            }
            Ok(())
        });
        if let Err(e) = verified {
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
        let _ = std::fs::remove_file(&final_path);
        std::fs::rename(&partial, &final_path)?;
        Ok(final_path)
    }

    /// Removes leftovers of earlier downloads of this component (other versions, `.partial`).
    fn clear_stale(&self, keep: &str) {
        let prefix = self.component.file_prefix();
        let Ok(entries) = std::fs::read_dir(&self.work_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name != keep {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Lower-case hex SHA-256 of a file.
fn file_sha256(path: &Path) -> DeskResult<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Release source held in memory: URL -> bytes. A missing URL is a network error.
    #[derive(Clone, Default)]
    struct Fake {
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        requests: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn put(&self, url: &str, body: &[u8]) {
            lock(&self.files).insert(url.to_string(), body.to_vec());
        }
        fn body(&self, url: &str) -> DeskResult<Vec<u8>> {
            lock(&self.requests).push(url.to_string());
            lock(&self.files)
                .get(url)
                .cloned()
                .ok_or_else(|| DeskError::Transport("network unreachable".to_string()))
        }
    }

    impl Transport for Fake {
        fn get(&self, url: &str, max_bytes: u64) -> DeskResult<Vec<u8>> {
            let body = self.body(url)?;
            if body.len() as u64 > max_bytes {
                return Err(DeskError::Transport("too large".to_string()));
            }
            Ok(body)
        }
        fn download(
            &self,
            url: &str,
            dest: &Path,
            _max: u64,
            progress: &mut dyn FnMut(u64, Option<u64>),
        ) -> DeskResult<u64> {
            let body = self.body(url)?;
            std::fs::write(dest, &body)?;
            progress(body.len() as u64, Some(body.len() as u64));
            Ok(body.len() as u64)
        }
    }

    const FEED: &str = "https://feed.test/latest";
    const INSTALLER: &[u8] = b"pretend installer bytes";

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A feed offering `version` with `INSTALLER`, manifest hash `hash`.
    fn publish(fake: &Fake, version: &str, hash: &str) {
        let agent = format!("DeskRemote-Agent-Setup-{version}.exe");
        let release = format!(
            r#"{{"tag_name":"v{version}","assets":[
              {{"name":"{agent}","browser_download_url":"https://dl.test/{agent}","size":{}}},
              {{"name":"DeskRemote-Panel-Setup-{version}.exe","browser_download_url":"https://dl.test/panel.exe","size":5}},
              {{"name":"SHA256SUMS.txt","browser_download_url":"https://dl.test/SHA256SUMS.txt","size":99}}]}}"#,
            INSTALLER.len()
        );
        fake.put(FEED, release.as_bytes());
        fake.put(&format!("https://dl.test/{agent}"), INSTALLER);
        fake.put("https://dl.test/panel.exe", b"panel");
        fake.put(
            "https://dl.test/SHA256SUMS.txt",
            format!(
                "{hash}  {agent}\n{}  DeskRemote-Panel-Setup-{version}.exe\n",
                sha(b"panel")
            )
            .as_bytes(),
        );
    }

    fn updater(fake: &Fake, dir: &Path, current: &str) -> Updater<Fake> {
        Updater::new(
            Component::Agent,
            SemVer::parse(current).expect("version"),
            Some(FEED.to_string()),
            dir.to_path_buf(),
            fake.clone(),
        )
    }

    #[test]
    fn same_or_older_release_is_no_update() -> DeskResult<()> {
        let dir = tempfile::tempdir()?;
        let fake = Fake::default();
        publish(&fake, "0.1.0", &sha(INSTALLER));
        let u = updater(&fake, dir.path(), "0.1.0");
        assert_eq!(u.check()?, UpdateStatus::NoUpdate);
        let newer = updater(&fake, dir.path(), "0.2.0");
        assert_eq!(newer.check()?, UpdateStatus::NoUpdate);
        Ok(())
    }

    #[test]
    fn newer_release_is_available_then_downloaded_and_verified() -> DeskResult<()> {
        let dir = tempfile::tempdir()?;
        let fake = Fake::default();
        publish(&fake, "0.1.1", &sha(INSTALLER));
        let u = updater(&fake, dir.path(), "0.1.0");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        u.on_status(move |s| lock(&sink).push(s.clone()));
        let status = u.check_and_download()?;
        let UpdateStatus::Ready { release, path } = status else {
            return Err(failure("expected Ready"));
        };
        assert_eq!(release.version.to_string(), "0.1.1");
        assert_eq!(std::fs::read(&path)?, INSTALLER);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("DeskRemote-Agent-Setup-0.1.1.exe")
        );
        let kinds: Vec<&str> = lock(&seen)
            .iter()
            .map(|s| match s {
                UpdateStatus::Checking => "checking",
                UpdateStatus::Available(_) => "available",
                UpdateStatus::Downloading { .. } => "downloading",
                UpdateStatus::Ready { .. } => "ready",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["checking", "available", "downloading", "ready"]);
        Ok(())
    }

    #[test]
    fn a_sha256_mismatch_is_rejected_and_leaves_no_file() -> DeskResult<()> {
        let dir = tempfile::tempdir()?;
        let fake = Fake::default();
        publish(&fake, "0.1.1", &sha(b"something else"));
        let u = updater(&fake, dir.path(), "0.1.0");
        assert!(matches!(u.check()?, UpdateStatus::Available(_)));
        let err = u.download().err().map(|e| e.to_string());
        assert!(err.is_some_and(|m| m.contains("SHA-256")));
        assert!(matches!(u.status(), UpdateStatus::Failed { .. }));
        assert_eq!(
            std::fs::read_dir(dir.path())?.count(),
            0,
            "nothing left behind"
        );
        Ok(())
    }

    #[test]
    fn an_announced_size_mismatch_is_rejected() -> DeskResult<()> {
        let dir = tempfile::tempdir()?;
        let fake = Fake::default();
        publish(&fake, "0.1.1", &sha(INSTALLER));
        let agent = "https://dl.test/DeskRemote-Agent-Setup-0.1.1.exe";
        fake.put(agent, b"truncated");
        let u = updater(&fake, dir.path(), "0.1.0");
        u.check()?;
        assert!(u.download().is_err());
        assert_eq!(std::fs::read_dir(dir.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn no_network_is_a_failed_status_and_an_error() -> DeskResult<()> {
        let dir = tempfile::tempdir()?;
        let u = updater(&Fake::default(), dir.path(), "0.1.0");
        assert!(u.check().is_err());
        assert!(matches!(u.status(), UpdateStatus::Failed { .. }));
        assert!(
            u.download().is_err(),
            "nothing to download after a failed check"
        );
        Ok(())
    }

    #[test]
    fn a_component_only_fetches_its_own_installer() -> DeskResult<()> {
        let dir = tempfile::tempdir()?;
        let fake = Fake::default();
        publish(&fake, "0.1.1", &sha(INSTALLER));
        let u = updater(&fake, dir.path(), "0.1.0");
        u.check_and_download()?;
        assert!(!lock(&fake.requests).iter().any(|u| u.contains("panel")));
        let panel = Updater::new(
            Component::Panel,
            SemVer::parse("0.1.0").expect("version"),
            Some(FEED.to_string()),
            dir.path().to_path_buf(),
            fake.clone(),
        );
        let UpdateStatus::Ready { path, .. } = panel.check_and_download()? else {
            return Err(failure("expected Ready"));
        };
        assert!(path.ends_with("DeskRemote-Panel-Setup-0.1.1.exe"));
        Ok(())
    }

    #[test]
    fn a_release_without_the_manifest_or_installer_is_refused() -> DeskResult<()> {
        let dir = tempfile::tempdir()?;
        let fake = Fake::default();
        fake.put(
            FEED,
            br#"{"tag_name":"v0.1.1","assets":[{"name":"DeskRemote-Agent-Setup-0.1.1.exe","browser_download_url":"https://x/a.exe"}]}"#,
        );
        let u = updater(&fake, dir.path(), "0.1.0");
        assert!(u.check().is_err(), "no SHA256SUMS.txt");
        fake.put(FEED, br#"{"tag_name":"v0.1.1","assets":[]}"#);
        assert!(u.check().is_err(), "no installer");
        fake.put(FEED, br#"{"tag_name":"nightly","assets":[]}"#);
        assert!(u.check().is_err(), "tag is not a version");
        fake.put(
            FEED,
            br#"{"tag_name":"v9.0.0","prerelease":true,"assets":[]}"#,
        );
        assert!(u.check().is_err(), "pre-release");
        Ok(())
    }

    #[test]
    fn an_https_feed_cannot_downgrade_to_http_links() -> DeskResult<()> {
        let dir = tempfile::tempdir()?;
        let fake = Fake::default();
        fake.put(
            FEED,
            br#"{"tag_name":"v0.1.1","assets":[
              {"name":"DeskRemote-Agent-Setup-0.1.1.exe","browser_download_url":"http://x/a.exe"},
              {"name":"SHA256SUMS.txt","browser_download_url":"https://x/s"}]}"#,
        );
        assert!(updater(&fake, dir.path(), "0.1.0").check().is_err());
        Ok(())
    }

    #[test]
    fn a_verified_download_is_reused_and_stale_versions_are_cleared() -> DeskResult<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("DeskRemote-Agent-Setup-0.0.9.exe"), b"old")?;
        std::fs::write(
            dir.path().join("DeskRemote-Agent-Setup-0.1.1.exe.partial"),
            b"half",
        )?;
        std::fs::write(dir.path().join("unrelated.txt"), b"keep")?;
        let fake = Fake::default();
        publish(&fake, "0.1.1", &sha(INSTALLER));
        let u = updater(&fake, dir.path(), "0.1.0");
        u.check_and_download()?;
        let mut names: Vec<String> = std::fs::read_dir(dir.path())?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["DeskRemote-Agent-Setup-0.1.1.exe", "unrelated.txt"]);
        let before = lock(&fake.requests).len();
        u.download()?;
        let after: Vec<String> = lock(&fake.requests)[before..].to_vec();
        assert!(
            after.iter().all(|r| r.ends_with("SHA256SUMS.txt")),
            "only the manifest is fetched again: {after:?}"
        );
        Ok(())
    }

    #[test]
    fn manifest_lines_are_matched_by_exact_file_name() -> DeskResult<()> {
        let hash = sha(b"x");
        let text = format!("{hash} *a.exe\n{}  b.exe\n", sha(b"y"));
        assert_eq!(parse_manifest(&text, "a.exe")?, hash);
        assert!(parse_manifest(&text, "c.exe").is_err());
        assert!(parse_manifest("nothex  a.exe\n", "a.exe").is_err());
        assert!(parse_manifest(&format!("{hash}  xa.exe\n"), "a.exe").is_err());
        Ok(())
    }
}
