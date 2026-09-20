//! The clerk binary at its process boundary: started with an environment,
//! read through its logs and its own `/health` and `/metrics`, stopped with
//! SIGTERM the way an operator's process manager would.
//!
//! Modelled on the Hermes suite's `RuntimeRun` (ticket #23): the binary is
//! the one Cargo built for this test run (`CARGO_BIN_EXE_twalk-clerk`), its
//! stdout and stderr are forwarded to the test's own so a failure comes
//! with the clerk's account of itself, and every line is kept so a test
//! can wait for one.

use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use twalk_test_harness::poll_until;

/// One clerk process.
pub struct ClerkProc {
    /// Where this clerk serves `/health` and `/metrics`.
    pub listen: SocketAddr,
    child: Option<Child>,
    log_lines: Arc<Mutex<Vec<String>>>,
    http: reqwest::Client,
}

impl ClerkProc {
    /// Starts the clerk with exactly `env` as its `CLERK_*` environment
    /// (see `relay_env`), on a listen port the kernel had free — unless
    /// `env` names `CLERK_LISTEN` itself. Returns as soon as the process is
    /// spawned: waiting for it to be up is the test's own
    /// [`wait_for_log`](Self::wait_for_log), because what a clerk says
    /// before it is running is sometimes the assertion.
    pub async fn start(mut env: Vec<(String, String)>) -> Result<Self> {
        let listen = match env.iter().find(|(name, _)| name == "CLERK_LISTEN") {
            Some((_, value)) => value.parse().context("CLERK_LISTEN is host:port")?,
            None => {
                let listen = free_loopback_port()?;
                env.push(("CLERK_LISTEN".to_owned(), listen.to_string()));
                listen
            }
        };
        let log_lines = Arc::new(Mutex::new(Vec::new()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_twalk-clerk"))
            // The clerk's configuration is its environment and nothing
            // else, so it gets this one and none of the test's own.
            .env_clear()
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start the clerk binary")?;
        forward(
            child.stdout.take().expect("stdout is piped"),
            false,
            log_lines.clone(),
        );
        forward(
            child.stderr.take().expect("stderr is piped"),
            true,
            log_lines.clone(),
        );
        Ok(Self {
            listen,
            child: Some(child),
            log_lines,
            http: reqwest::Client::new(),
        })
    }

    /// Everything the clerk has logged so far, both streams, in order of
    /// arrival.
    pub async fn logs(&self) -> String {
        self.log_lines.lock().await.join("\n")
    }

    /// Polls until a line containing `needle` has been logged. The failure
    /// carries the whole log, because a line that never came is the
    /// failure a process-boundary test diagnoses most often.
    pub async fn wait_for_log(&self, needle: &str) -> Result<()> {
        let found = poll_until(
            || async { self.logs().await.contains(needle).then_some(()) },
            &format!("a log line containing {needle:?}"),
        )
        .await;
        match found {
            Ok(()) => Ok(()),
            Err(error) => bail!("{error}; the clerk's logs were:\n{}", self.logs().await),
        }
    }

    /// Whether the clerk process is still up.
    pub fn is_running(&mut self) -> bool {
        match &mut self.child {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// `http://<listen>`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.listen)
    }

    /// `GET /health`, as the status it answered.
    pub async fn health(&self) -> Result<reqwest::StatusCode> {
        Ok(self
            .http
            .get(format!("{}/health", self.base_url()))
            .send()
            .await
            .context("GET /health")?
            .status())
    }

    /// `GET /metrics`, as the text an operator's scraper reads.
    pub async fn metrics(&self) -> Result<String> {
        let response = self
            .http
            .get(format!("{}/metrics", self.base_url()))
            .send()
            .await
            .context("GET /metrics")?;
        let status = response.status();
        let body = response.text().await.context("reading /metrics")?;
        if !status.is_success() {
            bail!("GET /metrics answered {status}: {body}");
        }
        Ok(body)
    }

    /// Sends SIGTERM — as an operator's process manager would — and waits
    /// for the clerk to exit. (`Child::kill` only sends SIGKILL, which
    /// cannot exercise a graceful shutdown.)
    pub async fn stop(&mut self) -> Result<std::process::ExitStatus> {
        let child = self.child.as_mut().context("the clerk is not running")?;
        let pid = child.id().context("the clerk has already exited")?;
        let status = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await
            .context("failed to run kill(1)")?;
        anyhow::ensure!(status.success(), "kill(1) failed with {status}");
        let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
            .await
            .context("the clerk did not exit within 30s of SIGTERM")??;
        self.child = None;
        Ok(status)
    }
}

impl ClerkProc {
    /// Ends the process without waiting on it, from a context that cannot
    /// await — a `Drop`: a SIGTERM first, which is what lets it say
    /// goodbye in the log a failure is read from, then the `kill_on_drop`
    /// SIGKILL as the child handle goes. A clerk already stopped is left
    /// alone.
    pub fn terminate(&mut self) {
        if let Some(child) = self.child.take() {
            if let Some(pid) = child.id() {
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
            }
            // `child` goes out of scope here: `kill_on_drop` finishes it.
        }
    }
}

impl Drop for ClerkProc {
    /// A test that panicked never reached `stop`; the process must still
    /// go.
    fn drop(&mut self) {
        self.terminate();
    }
}

/// A loopback port the kernel had free a moment ago: bound, read and
/// released, so the clerk can bind it itself. (The clerk needs a known
/// port — it is asked for `/metrics` — so `127.0.0.1:0` is no use.)
fn free_loopback_port() -> Result<SocketAddr> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").context("binding a loopback port")?;
    listener.local_addr().context("reading the bound port")
}

fn forward<S>(stream: S, is_stderr: bool, store: Arc<Mutex<Vec<String>>>)
where
    S: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if is_stderr {
                eprintln!("{line}");
            } else {
                println!("{line}");
            }
            store.lock().await.push(line);
        }
    });
}
