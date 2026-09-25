//! Browser guardian (ADR 0007): a small process started from the running Broxser
//! executable before each browser. Its stdin is a pipe whose only writer is the
//! owner. However the owner ends, including SIGKILL and crashes, the kernel
//! closes that pipe; without a prior `release`, the guardian stops the owner's
//! browser and removes its profile. It never replays browser actions.

use crate::browser::{self, EXIT_TIMEOUT, ProcessIdentity};
use crate::cdp::{Cancellation, Cancelled};
use crate::profile::{self, Lease};
use anyhow::{Context, Result, anyhow, ensure};
use std::env;
use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// Command-line marker that selects the guardian in Broxser binaries.
const GUARDIAN_ARG: &str = "--broxser-browser-guardian";
const PROFILE_ENV: &str = "BROXSER_GUARDIAN_PROFILE";
const OWNER_ENV: &str = "BROXSER_GUARDIAN_OWNER";
const READY: &str = "broxser-guardian ready";
const REFUSED: &str = "broxser-guardian refused: ";
const STARTUP: Duration = Duration::from_secs(5);
const RELEASE: Duration = Duration::from_secs(2);
const MAX_COMMAND: u64 = 128;

/// Runs the browser guardian and exits if this process was started as one.
/// Call it first in `main` of every binary that launches browsers: the engine
/// starts a guardian by re-executing the current executable, and refuses to
/// launch a browser if the guardian does not report that it is ready.
pub fn run_guardian_if_requested() {
    let mut args = env::args_os().skip(1);
    if args.next().as_deref() == Some(OsStr::new(GUARDIAN_ARG)) && args.next().is_none() {
        process::exit(main());
    }
}

/// The owner's end: the pipe into the guardian's stdin and the child to reap.
pub(crate) struct Guardian {
    child: Child,
    commands: ChildStdin,
    identity: ProcessIdentity,
}

impl Guardian {
    /// Starts the guardian of `profile`, whose lease already names `owner`, and
    /// waits until it is ready to clean up.
    pub(crate) fn start(
        profile: &Path,
        owner: &ProcessIdentity,
        cancel: &Cancellation,
    ) -> Result<Self> {
        let mut child = command()
            .env(PROFILE_ENV, profile)
            .env(OWNER_ENV, format!("{} {}", owner.pid, owner.start_time))
            // Do not keep the owner's working directory busy.
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("start the browser guardian")?;
        let commands = child.stdin.take().expect("guardian stdin is piped");
        let stdout = child.stdout.take().expect("guardian stdout is piped");
        let (sender, answers) = mpsc::channel();
        thread::spawn(move || {
            // Only protocol lines count; test binaries print libtest lines first.
            for line in BufReader::new(stdout.take(64 * 1024)).lines() {
                let Ok(line) = line else { break };
                let answer = if line == READY {
                    Ok(())
                } else if let Some(reason) = line.strip_prefix(REFUSED) {
                    Err(reason.to_owned())
                } else {
                    continue;
                };
                let _ = sender.send(answer);
                return;
            }
        });
        let deadline = Instant::now() + STARTUP;
        let ready = loop {
            match answers.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(())) => break Ok(()),
                Ok(Err(reason)) => {
                    break Err(anyhow!("the browser guardian refused to start: {reason}"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    break Err(anyhow!(
                        "the browser guardian exited before it was ready; the executable must \
                         call broxser_engine::run_guardian_if_requested() first in main"
                    ));
                }
                Err(RecvTimeoutError::Timeout) if cancel.is_cancelled() => {
                    break Err(Cancelled.into());
                }
                Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    break Err(anyhow!(
                        "the browser guardian was not ready within {} seconds",
                        STARTUP.as_secs()
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        };
        let identity = browser::identity(child.id());
        match (ready, identity) {
            (Ok(()), Some(identity)) => Ok(Self {
                child,
                commands,
                identity,
            }),
            (ready, _) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(ready
                    .err()
                    .unwrap_or_else(|| anyhow!("read the browser guardian's identity")))
            }
        }
    }

    pub(crate) fn identity(&self) -> ProcessIdentity {
        self.identity
    }

    /// Reports the browser started for the guarded profile.
    pub(crate) fn watch(&mut self, browser: &ProcessIdentity) -> Result<()> {
        writeln!(
            self.commands,
            "browser {} {}",
            browser.pid, browser.start_time
        )
        .and_then(|()| self.commands.flush())
        .context("hand the browser to its guardian")
    }

    /// Tells the guardian that its owner finished cleaning up, then reaps it.
    /// A guardian that does not exit promptly is stopped: nothing is left to guard.
    pub(crate) fn release(self) {
        let Self {
            mut child,
            mut commands,
            ..
        } = self;
        let _ = writeln!(commands, "release").and_then(|()| commands.flush());
        drop(commands);
        let deadline = Instant::now() + RELEASE;
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Closes the pipe as a dying owner would and returns the guardian to reap.
    #[cfg(test)]
    pub(crate) fn abandon(self) -> Child {
        self.child
    }
}

fn command() -> Command {
    #[cfg(not(test))]
    {
        use std::os::unix::process::CommandExt;
        // The running binary, even if its file has been replaced since it started.
        let mut command = Command::new("/proc/self/exe");
        command.arg0("broxser-guardian").arg(GUARDIAN_ARG);
        command
    }
    // Test binaries run the guardian through libtest; see `test_support`.
    #[cfg(test)]
    crate::test_support::role_command("guardian")
}

enum Message {
    Browser(ProcessIdentity),
    Release,
}

fn parse(line: &[u8]) -> Option<Message> {
    let line = std::str::from_utf8(line).ok()?.strip_suffix('\n')?;
    if line == "release" {
        return Some(Message::Release);
    }
    parse_identity(line.strip_prefix("browser ")?).map(Message::Browser)
}

fn parse_identity(text: &str) -> Option<ProcessIdentity> {
    let (pid, start_time) = text.split_once(' ')?;
    Some(ProcessIdentity {
        pid: pid
            .parse()
            .ok()
            .filter(|pid| (1..=i32::MAX as u32).contains(pid))?,
        start_time: start_time.parse().ok()?,
    })
}

/// Guardian entry point; returns the process exit status.
pub(crate) fn main() -> i32 {
    let (profile, owner) = match arm() {
        Ok(armed) => armed,
        Err(error) => {
            let mut stdout = io::stdout();
            let _ = writeln!(stdout, "{REFUSED}{error:#}").and_then(|()| stdout.flush());
            return 2;
        }
    };
    let mut reported = None;
    let mut input = io::stdin().lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        // End of input without `release`: the owner exited, crashed or was killed.
        match (&mut input).take(MAX_COMMAND).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        match parse(&line) {
            Some(Message::Release) => return 0,
            Some(Message::Browser(identity)) if reported.is_none() => reported = Some(identity),
            _ => {}
        }
    }
    if cleanup(&profile, &owner, reported) {
        0
    } else {
        1
    }
}

fn arm() -> Result<(PathBuf, ProcessIdentity)> {
    // A new session: signals for the owner's process group, such as Ctrl+C or a
    // terminal hangup, do not stop the process that cleans up after the owner.
    rustix::process::setsid().context("start a new session")?;
    let profile = PathBuf::from(env::var_os(PROFILE_ENV).context("no profile to guard")?);
    let owner = env::var(OWNER_ENV)
        .ok()
        .and_then(|owner| parse_identity(&owner))
        .context("no owner identity")?;
    ensure!(
        Lease::read(&profile)?.owner == owner,
        "the profile lease names another owner"
    );
    browser::check_pidfd()?;
    let mut stdout = io::stdout();
    writeln!(stdout, "{READY}")
        .and_then(|()| stdout.flush())
        .context("report readiness")?;
    Ok((profile, owner))
}

/// Stops what remains of the owner's browser and removes its profile. Returns
/// false if a process survived or the profile could not be removed.
fn cleanup(profile: &Path, owner: &ProcessIdentity, reported: Option<ProcessIdentity>) -> bool {
    // A browser spawned just before its owner died may not have been reported.
    let mut browsers = browser::launched_with(profile);
    if let Some(reported) = reported
        && !browsers.contains(&reported)
    {
        browsers.push(reported);
    }
    let mut processes: Vec<ProcessIdentity> = Vec::new();
    for process in browsers
        .iter()
        .flat_map(browser::descendants)
        .chain(browser::referencing(profile))
    {
        if !processes.contains(&process) {
            processes.push(process);
        }
    }
    for browser in &browsers {
        let _ = browser::terminate(browser);
    }
    let survivors = browser::wait_for_release(&processes, profile, EXIT_TIMEOUT);
    let removed = profile::remove_guarded(profile, owner);
    let mut stderr = io::stderr();
    if survivors > 0 {
        let _ = writeln!(
            stderr,
            "broxser guardian: {survivors} processes of the browser or naming its profile did \
             not exit"
        );
    }
    if let Err(error) = &removed {
        let _ = writeln!(stderr, "broxser guardian: {error:#}");
    }
    survivors == 0 && removed.is_ok()
}

#[cfg(test)]
pub(crate) mod tests;
