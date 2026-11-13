//! Keeps the embedded tailnet helper running (DAPP-113).
//!
//! The helper is a long-lived child: while the node still needs its one-time sign-in it keeps
//! reporting `needs_auth`; when it exits for any reason it is started again after an
//! exponentially growing delay. The login itself lives in the helper's state folder, so a restart
//! never asks for it again. Every report is mirrored into [`Control`] (tray) and the status file.

use crate::control::Control;
use crate::takeover::TakeoverStatus;
use crate::tracker::Backoff;
use serde::Deserialize;
use std::io::BufReader;
use std::process::{Child, ChildStdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// First and longest wait before the helper is started again.
pub(crate) const RESTART_MIN: Duration = Duration::from_secs(1);
pub(crate) const RESTART_MAX: Duration = Duration::from_secs(60);
/// A helper that ran this long counts as healthy; the next crash starts the delay from scratch.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// Granularity at which waiting loops notice the stop flag.
const STOP_POLL: Duration = Duration::from_millis(100);

/// One JSON line the helper prints.
#[derive(Deserialize)]
pub(crate) struct HelperReport {
    pub status: String,
    #[serde(default)]
    pub ips: Vec<String>,
    #[serde(default)]
    pub auth_url: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// The child process, shared so the owner can kill it while the worker is blocked reading it.
pub(crate) type ChildSlot = Arc<Mutex<Option<Child>>>;

/// Starts one helper process.
pub(crate) type SpawnFn = Box<dyn FnMut() -> Option<Child> + Send>;
/// Hand-over of an official client for the given tailnet addresses.
pub(crate) type TakeOverFn = Box<dyn FnMut(&[String]) -> TakeoverStatus + Send>;
/// Records `(state, takeover)` in the status file.
pub(crate) type PublishFn = Box<dyn FnMut(&str, &TakeoverStatus) + Send>;

/// Everything the restart loop needs; the pieces that touch the machine are closures so the
/// loop can be tested with a stand-in process.
pub(crate) struct Runner {
    pub spawn: SpawnFn,
    /// Hand-over of an official client once the helper is verified; runs on `listening`.
    pub take_over: TakeOverFn,
    /// Records `(state, takeover)` after every change (the status file).
    pub publish: PublishFn,
    pub control: Arc<Control>,
    pub stop: Arc<AtomicBool>,
    pub child: ChildSlot,
    pub backoff: Backoff,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Runner {
    /// Runs until the stop flag is set.
    pub fn run(mut self) {
        let mut takeover = TakeoverStatus::Unverified;
        while !self.stop.load(Ordering::SeqCst) {
            let started = Instant::now();
            let mut listened = false;
            if let Some(stdout) = self.start_child() {
                listened = self.read_reports(stdout, &mut takeover);
                if let Some(mut child) = lock(&self.child).take() {
                    let _ = child.wait();
                }
            }
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            tracing::warn!("tailnet helper is not running; it will be started again");
            self.control.set_tailnet_report("error", &[], None);
            (self.publish)("error", &takeover);
            if listened || started.elapsed() >= STABLE_AFTER {
                self.backoff.reset();
            }
            let delay = self.backoff.next_delay();
            self.wait(delay);
        }
    }

    /// Starts the helper and parks the process handle; returns its report stream.
    fn start_child(&mut self) -> Option<ChildStdout> {
        let mut child = (self.spawn)()?;
        let stdout = child.stdout.take()?;
        let mut slot = lock(&self.child);
        if self.stop.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        *slot = Some(child);
        Some(stdout)
    }

    /// Mirrors reports until the helper exits; returns whether it ever reached `listening`.
    fn read_reports(&mut self, stdout: ChildStdout, takeover: &mut TakeoverStatus) -> bool {
        let mut listened = false;
        let stream = serde_json::Deserializer::from_reader(BufReader::new(stdout))
            .into_iter::<HelperReport>();
        for item in stream {
            let Ok(report) = item else { break };
            tracing::info!(status = %report.status, ips = ?report.ips, "tailnet helper report");
            if let Some(error) = &report.error {
                tracing::warn!(%error, "tailnet helper reported an error");
            }
            self.control.set_tailnet_report(
                &report.status,
                &report.ips,
                report.auth_url.as_deref(),
            );
            if report.status == "listening" {
                listened = true;
                // Retry a hand-over that did not happen yet; a finished one is not repeated.
                if !matches!(
                    takeover,
                    TakeoverStatus::TookOver | TakeoverStatus::NotNeeded
                ) {
                    *takeover = (self.take_over)(&report.ips);
                    if *takeover == TakeoverStatus::NeedsAdmin {
                        self.control.set_takeover_needs_admin(true);
                    }
                }
            }
            (self.publish)(state_name(&report), takeover);
        }
        listened
    }

    /// Sleeps for `delay`, ending early when asked to stop.
    fn wait(&self, delay: Duration) {
        let end = Instant::now() + delay;
        while Instant::now() < end && !self.stop.load(Ordering::SeqCst) {
            std::thread::sleep(STOP_POLL.min(end.saturating_duration_since(Instant::now())));
        }
    }
}

/// Name used in the status file for what the helper just reported.
fn state_name(report: &HelperReport) -> &'static str {
    match report.status.as_str() {
        "listening" | "running" if !report.ips.is_empty() => "connected",
        "needs_auth" => "needs_login",
        _ => "off",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::atomic::AtomicUsize;

    const LOGIN_REPORT: &str =
        r#"{"status":"needs_auth","auth_url":"https://login.example.com/a"}"#;

    /// A stand-in helper: prints one report and exits at once (a crash).
    fn crashing_helper(line: &str) -> Option<Child> {
        Command::new(desk_paths::DeskPaths::system_tool("cmd.exe"))
            .raw_arg(format!("/c echo {line}"))
            .stdout(Stdio::piped())
            .spawn()
            .ok()
    }

    fn runner(spawn: SpawnFn, control: &Arc<Control>, stop: &Arc<AtomicBool>) -> Runner {
        Runner {
            spawn,
            take_over: Box::new(|_| TakeoverStatus::NotNeeded),
            publish: Box::new(|_, _| {}),
            control: Arc::clone(control),
            stop: Arc::clone(stop),
            child: Arc::new(Mutex::new(None)),
            backoff: Backoff::new(Duration::from_millis(20), Duration::from_millis(80)),
        }
    }

    #[test]
    fn a_crashing_helper_is_started_again_with_growing_delays() {
        let control = Arc::new(Control::default());
        let stop = Arc::new(AtomicBool::new(false));
        let starts = Arc::new(AtomicUsize::new(0));
        let (n, flag) = (Arc::clone(&starts), Arc::clone(&stop));
        let spawn = Box::new(move || {
            if n.fetch_add(1, Ordering::SeqCst) + 1 >= 4 {
                flag.store(true, Ordering::SeqCst);
            }
            crashing_helper(LOGIN_REPORT)
        });
        let began = Instant::now();
        runner(spawn, &control, &stop).run();
        // The fourth start sets the stop flag, so the loop ends after that run.
        assert_eq!(
            starts.load(Ordering::SeqCst),
            4,
            "restarted after each exit"
        );
        // Waits between the first three exits: 20 + 40 + 80 ms.
        assert!(began.elapsed() >= Duration::from_millis(140));
    }

    #[test]
    fn reports_reach_the_tray_and_a_dead_helper_shows_as_error() -> desk_error::DeskResult<()> {
        let control = Arc::new(Control::default());
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (log, flag) = (Arc::clone(&seen), Arc::clone(&stop));
        let mut r = runner(Box::new(|| crashing_helper(LOGIN_REPORT)), &control, &stop);
        r.publish = Box::new(move |state, _| {
            lock(&log).push(state.to_string());
            if state == "error" {
                flag.store(true, Ordering::SeqCst);
            }
        });
        r.run();
        assert_eq!(*lock(&seen), vec!["needs_login", "error"]);
        let identity = desk_crypto::DeviceIdentity::generate()?;
        let info = control.snapshot(&identity, 0, false, Vec::new());
        assert_eq!(info.tailnet, desk_ipc::TailnetState::Failed);
        Ok(())
    }
}
