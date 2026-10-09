// Fixed upstream 5da0a01e1eedda054db0c81dd3a780000c40d9f0 noninteractive process timeout.
use std::io::{self, Read as _};
use std::process::{Child, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Waiting policy for one noninteractive ssh call: the caller's cancellation
/// flag, the extra deadline granted once a Tailscale SSH `check` banner shows,
/// and the hook fired once with that banner's URL.
pub(super) struct WaitHooks<'a> {
    pub(super) cancel: &'a AtomicBool,
    pub(super) auth_wait: Duration,
    pub(super) on_auth_url: &'a mut dyn FnMut(&str),
}

/// Extract the `To authenticate, visit: <URL>` target a Tailscale SSH `check`
/// banner writes to stderr. Only a newline-terminated http(s) URL counts: a
/// partial line may still be arriving and must not open a truncated address.
pub(super) fn tailscale_check_url(stderr_so_far: &[u8]) -> Option<String> {
    const MARKER: &str = "To authenticate, visit:";
    let text = String::from_utf8_lossy(stderr_so_far);
    let rest = &text[text.find(MARKER)? + MARKER.len()..];
    let end = rest.find('\n')?;
    let url = rest[..end].trim();
    (url.starts_with("https://") || url.starts_with("http://")).then(|| url.to_owned())
}

fn kill_and_join(
    mut child: Child,
    stdout: JoinHandle<io::Result<Vec<u8>>>,
    stderr: JoinHandle<io::Result<Vec<u8>>>,
    error: io::Error,
) -> io::Error {
    let _ = child.kill();
    let _ = child.wait();
    let _ = stdout.join();
    let _ = stderr.join();
    error
}

pub(super) fn wait_with_output_timeout(
    mut child: Child,
    timeout: Duration,
    hooks: &mut WaitHooks<'_>,
) -> io::Result<Output> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("SSH command stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("SSH command stderr was not captured"))?;
    let stdout = thread::spawn(move || {
        let mut stdout = stdout;
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    // stderr is mirrored into `observed` so a Tailscale check banner is noticed
    // while the ssh process is still blocked waiting for approval.
    let observed = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr = {
        let observed = Arc::clone(&observed);
        thread::spawn(move || {
            let mut stderr = stderr;
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                match stderr.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => {
                        bytes.extend_from_slice(&chunk[..count]);
                        if let Ok(mut seen) = observed.lock() {
                            seen.extend_from_slice(&chunk[..count]);
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(bytes)
        })
    };
    let mut deadline = Instant::now() + timeout;
    let mut auth_wait_started = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => return Err(kill_and_join(child, stdout, stderr, error)),
        }
        if hooks.cancel.load(Ordering::Acquire) {
            return Err(kill_and_join(
                child,
                stdout,
                stderr,
                io::Error::new(
                    io::ErrorKind::Interrupted,
                    "saved SSH connection attempt was cancelled",
                ),
            ));
        }
        if !auth_wait_started {
            let url = observed
                .lock()
                .ok()
                .and_then(|seen| tailscale_check_url(seen.as_slice()));
            if let Some(url) = url {
                auth_wait_started = true;
                // tailssh itself waits this long for the check approval, so the
                // same ssh stays alive and resumes once the visit is approved.
                deadline = Instant::now() + hooks.auth_wait;
                (hooks.on_auth_url)(&url);
            }
        }
        if Instant::now() >= deadline {
            return Err(kill_and_join(
                child,
                stdout,
                stderr,
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    if auth_wait_started {
                        "noninteractive SSH command timed out waiting for Tailscale SSH approval"
                    } else {
                        "noninteractive SSH command timed out"
                    },
                ),
            ));
        }
        thread::sleep(POLL_INTERVAL);
    };
    let stdout = stdout
        .join()
        .map_err(|_| io::Error::other("SSH stdout reader panicked"))??;
    let stderr = stderr
        .join()
        .map_err(|_| io::Error::other("SSH stderr reader panicked"))??;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}
