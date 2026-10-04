//! The test stack's lifecycle: a real Synapse and a real NATS JetStream,
//! brought up from `compose.test.yaml` next to this crate.
//!
//! The stack is shared by every component's suite — it is the seam's other
//! side, not the Sensor's private fixture — and it is NOT the reference
//! deployment (that lives in `deploy/` and is exercised by
//! `sensor/tests/deployment.rs`).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

/// The appservice token the test Synapse is registered with
/// (`synapse/appservice-portals.yaml`), and the account that token acts as
/// when nothing names another.
///
/// The portal register's suite needs both (#171): Synapse honours `?user_id=`
/// only for an appservice token, so without a real registration the parameter
/// that decides which account reads a bridge's rooms is unobservable — which
/// is precisely why #105's suite could not fail on its absence.
pub const PORTALS_APPSERVICE_AS_TOKEN: &str = "test-only-portals-appservice-as-token";
/// The registration's `sender_localpart`: an account in no rooms, which is
/// what a generated mautrix registration's sender is.
pub const PORTALS_APPSERVICE_SENDER: &str = "@appservice_sender_in_no_rooms:test.twalk";

/// The Matrix server name the test Synapse answers for (see
/// `synapse/homeserver.yaml`).
pub const SERVER_NAME: &str = "test.twalk";

/// The stack is parameterizable so that parallel worktrees each run their
/// own isolated instance: TWALK_TEST_STACK names the compose project,
/// TWALK_TEST_SYNAPSE_PORT / TWALK_TEST_NATS_PORT move the host ports.
/// Defaults match the main checkout.
pub fn synapse_url() -> String {
    let port = std::env::var("TWALK_TEST_SYNAPSE_PORT").unwrap_or_else(|_| "18008".to_owned());
    format!("http://localhost:{port}")
}

pub fn nats_url() -> String {
    let port = std::env::var("TWALK_TEST_NATS_PORT").unwrap_or_else(|_| "14222".to_owned());
    format!("nats://localhost:{port}")
}

/// The compose project the stack runs under. The default keeps the name the
/// Sensor suite bootstrapped the stack with: renaming it would leave every
/// developer's running project behind, holding the host ports the new one
/// needs.
fn stack_id() -> String {
    std::env::var("TWALK_TEST_STACK").unwrap_or_else(|_| "twalk-sensor-test".to_owned())
}

/// This crate's own directory: the compose file, the Synapse configuration
/// and the provisioning script travel with the harness, so a component's
/// suite needs no knowledge of where they live.
fn harness_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Brings the compose stack up (idempotent) and provisions the bots.
/// Safe to call at the top of every test: the bootstrap is serialized
/// through a OnceCell (all tests of one binary share one process), so the
/// first caller does the work and concurrent callers wait for it instead of
/// racing parallel `docker compose up` invocations on a cold volume.
pub async fn ensure_stack() -> Result<()> {
    static STACK: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    STACK.get_or_try_init(do_ensure_stack).await?;
    Ok(())
}

async fn do_ensure_stack() -> Result<()> {
    let compose = harness_dir().join("compose.test.yaml");
    let status = Command::new("docker")
        .args([
            "compose".to_owned(),
            "-p".to_owned(),
            stack_id(),
            "-f".to_owned(),
            compose.to_string_lossy().into_owned(),
            "up".to_owned(),
            "-d".to_owned(),
            "--wait".to_owned(),
        ])
        .stdout(Stdio::null())
        .status()
        .await
        .context("failed to run docker compose up")?;
    if !status.success() {
        bail!("docker compose up failed with {status}");
    }

    let status = Command::new(harness_dir().join("scripts").join("provision-bots.sh"))
        .status()
        .await
        .context("failed to run provision-bots.sh")?;
    if !status.success() {
        bail!("bot provisioning failed with {status}");
    }

    ensure_appservice_registered(&compose).await?;
    keep_the_stack_bounded().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Keeping the shared stack from growing without bound (#432)
// ---------------------------------------------------------------------------

/// How often one stack is swept, at most.
///
/// The walk asks the homeserver about every account it holds — 478 of them on
/// 2026-10-04 — and a `cargo test` of one component runs nineteen binaries,
/// each calling [`ensure_stack`] once. Sweeping in all nineteen would spend
/// more time on housekeeping than on the suite, and nothing accumulates in an
/// hour that two days of grace will not cover. The marker is a file in the
/// temp directory named after the compose project, so parallel worktrees that
/// share a stack share its sweep, and one that moved its stack aside with
/// `TWALK_TEST_STACK` sweeps its own.
const SWEEP_AT_MOST_EVERY: Duration = Duration::from_secs(60 * 60);

/// Says how old the stack's data is, sweeps it if it is due, and prints both.
///
/// **Never fails a suite.** This is housekeeping: a sweep that cannot run is a
/// line on stderr and nothing more. A harness that failed here would report
/// its own chores as a defect in the code under test, which is the shape of
/// the bug this whole ticket is about.
async fn keep_the_stack_bounded() {
    eprintln!("harness: {}", stack_report().await);
    if !sweep_is_due() {
        return;
    }
    // Marked before the sweep rather than after, so that a sweep which fails
    // (or a run killed in the middle of one) is not retried by all eighteen
    // binaries behind this one.
    mark_swept();
    match crate::sweep::sweep_stack(crate::sweep::UNSEEN_FOR).await {
        Ok(swept) => eprintln!("harness: {swept}"),
        Err(error) => eprintln!(
            "harness: the shared stack could not be swept, and no test depends on it having \
             been: {error:#}"
        ),
    }
}

/// What the stack is, how old its data is, and what starts it over — the line
/// a reader of a timed-out wait needs.
///
/// A wait that gives up on a fortnight-old stack reads as a defect in the
/// component under test; it took reading `homeserver.db` by hand to find that
/// four failures in one suite were a shared homeserver holding 1 395 rooms
/// (#432). The age is no longer something to infer from `docker ps`.
pub async fn stack_report() -> String {
    let project = stack_id();
    let age = match data_age(&project).await {
        Some(age) => format!("has held data for {}", how_long(age)),
        // The volume is the thing that accumulates and the container is not:
        // every worktree's first run recreates the container (its compose file
        // binds a configuration directory at its own path), so container
        // uptime says nothing about how much the homeserver holds.
        None => {
            "has held data for an unknown time (its data volume could not be inspected)".to_owned()
        }
    };
    let swept = match swept_ago(&project) {
        Some(ago) => format!("swept {} ago", how_long(ago)),
        None => "not swept by this host yet".to_owned(),
    };
    format!(
        "the shared test stack {project} {age}, {swept}; \
         `tools/twalk-test-stacks.sh --recreate {project}` starts it over, and nothing does that \
         by itself — sibling suites share this stack (#432)"
    )
}

/// `16 day(s)`, `3 hour(s)`, `12 minute(s)` — one unit, because the reader is
/// deciding whether to suspect the stack and not measuring it.
fn how_long(age: Duration) -> String {
    let days = age.as_secs() / 86_400;
    let hours = age.as_secs() / 3_600;
    if days > 0 {
        format!("{days} day(s)")
    } else if hours > 0 {
        format!("{hours} hour(s)")
    } else {
        format!("{} minute(s)", age.as_secs() / 60)
    }
}

/// How long ago the stack's data volume was created, which is how long the
/// homeserver has been accumulating.
async fn data_age(project: &str) -> Option<Duration> {
    let output = Command::new("docker")
        .args([
            "volume",
            "inspect",
            &format!("{project}_synapse-data"),
            "--format",
            "{{.CreatedAt}}",
        ])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let created =
        chrono::DateTime::parse_from_rfc3339(String::from_utf8_lossy(&output.stdout).trim())
            .ok()?;
    // `chrono` is built here without its clock, so now comes from the standard
    // library and the comparison is in plain epoch seconds.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    u64::try_from(created.timestamp())
        .ok()
        .and_then(|created| now.checked_sub(created))
        .map(Duration::from_secs)
}

fn sweep_marker(project: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{project}.swept"))
}

fn swept_ago(project: &str) -> Option<Duration> {
    std::fs::metadata(sweep_marker(project))
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
}

fn sweep_is_due() -> bool {
    swept_ago(&stack_id()).is_none_or(|ago| ago >= SWEEP_AT_MOST_EVERY)
}

fn mark_swept() {
    // Best effort by design: a temp directory this process cannot write to
    // means the sweep runs more often than it needs to, which is the harmless
    // direction.
    let _ = std::fs::write(sweep_marker(&stack_id()), b"");
}

/// Makes sure the test Synapse has read `synapse/appservice-portals.yaml`.
///
/// The configuration is bind-mounted, and `docker compose up -d` does **not**
/// restart a running container when a bind-mounted file changes. So every
/// stack that was already running when this registration was added — and on a
/// developer's machine that is all of them — would serve a Synapse that has
/// never heard of the appservice, and the portal suite would fail with "the
/// register asked the wrong account" against perfectly correct code.
///
/// That failure would be the same shape as the defect it is testing for: a
/// symptom that points at the product when the cause is the fixture. So the
/// token is probed, the container is recreated once if the answer says the
/// registration is not loaded, and if it is still not loaded afterwards **this
/// function fails and says exactly what it probed and what it was told**. It
/// never falls through into a test: an unregistered appservice must not be
/// discovered as an assertion.
async fn ensure_appservice_registered(compose: &std::path::Path) -> Result<()> {
    // The fast path checks the identity too, not merely that something
    // answered: a registration whose sender has drifted from
    // [`PORTALS_APPSERVICE_SENDER`] would make the suite assert against an
    // account nobody meant, and recreating Synapse would not fix it.
    if appservice_whoami().await.as_deref() == Ok(PORTALS_APPSERVICE_SENDER) {
        return Ok(());
    }

    let status = Command::new("docker")
        .args([
            "compose".to_owned(),
            "-p".to_owned(),
            stack_id(),
            "-f".to_owned(),
            compose.to_string_lossy().into_owned(),
            "up".to_owned(),
            "-d".to_owned(),
            "--wait".to_owned(),
            "--force-recreate".to_owned(),
            "synapse".to_owned(),
        ])
        .stdout(Stdio::null())
        .status()
        .await
        .context("failed to recreate the test Synapse to load its appservice registration")?;
    if !status.success() {
        bail!(
            "the test Synapse could not be recreated to load {}: docker compose exited with \
             {status}",
            harness_dir()
                .join("synapse")
                .join("appservice-portals.yaml")
                .display()
        );
    }

    match appservice_whoami().await {
        Ok(user_id) if user_id == PORTALS_APPSERVICE_SENDER => Ok(()),
        Ok(user_id) => bail!(
            "the test Synapse answered /account/whoami for the portals appservice token with \
             {user_id:?}, and the registration in {} says its sender is \
             {PORTALS_APPSERVICE_SENDER:?}. The two have drifted; fix the registration or this \
             constant, and do not let a test run against the difference.",
            harness_dir()
                .join("synapse")
                .join("appservice-portals.yaml")
                .display()
        ),
        Err(detail) => bail!(
            "the test Synapse has not registered the portals appservice, and recreating it did \
             not help. This is the harness's fault and not the code under test's: a test run \
             now would fail as though the portal register asked the wrong account.\n  \
             probed: GET {}/_matrix/client/v3/account/whoami with the token in {}\n  \
             answered: {detail}\n  \
             try: docker compose -p {} -f {} down -v, then run the suite again",
            synapse_url(),
            harness_dir()
                .join("synapse")
                .join("appservice-portals.yaml")
                .display(),
            stack_id(),
            compose.display()
        ),
    }
}

/// Who the homeserver says the portals appservice token is, or why it would
/// not say. `Err` includes an unreachable homeserver, which is the same
/// diagnosis as an unregistered one for the caller's purposes.
async fn appservice_whoami() -> std::result::Result<String, String> {
    let response = reqwest::Client::new()
        .get(format!(
            "{}/_matrix/client/v3/account/whoami",
            synapse_url()
        ))
        .bearer_auth(PORTALS_APPSERVICE_AS_TOKEN)
        .send()
        .await
        .map_err(|error| error.without_url().to_string())?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("{status} {body}"));
    }
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|body| {
            body.get("user_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .ok_or_else(|| format!("{status} {body}, which names no user_id"))
}

// ---------------------------------------------------------------------------
// Holding a deploy stack, across processes (#212)
// ---------------------------------------------------------------------------

/// An exclusive hold on one compose project, released when this is dropped.
///
/// The lock is `flock(2)` on a file in the temp directory, and the kernel
/// releases it when the holding process dies — a test interrupted with ^C, a
/// `cargo test` killed on a full disk, a panic that unwinds past the guard —
/// which is why it is `flock` and not a lock file with a pid inside. There is no
/// stale lock to recognise and no lock to steal, and those are the two things a
/// hand-rolled one gets wrong.
///
/// Advisory, so it binds only the runs that ask. Every reader of a deploy stack
/// in this repository asks; nothing stops an operator's own `docker compose up`
/// on the same project, which is theirs to do.
pub struct StackHeld {
    /// Held for its file descriptor: dropping it closes the descriptor, which is
    /// what releases the lock. Never read.
    _file: std::fs::File,
    project: String,
}

impl Drop for StackHeld {
    fn drop(&mut self) {
        // Said, because a wait nobody can see is the kind of slowness that gets
        // blamed on the code under test.
        eprintln!("released the deploy stack {}", self.project);
    }
}

/// Waits until this process is the only one using the deploy stack `project`,
/// and holds it until the answer is dropped (#212).
///
/// Two deployment suites in two binaries share one compose project and one pair
/// of host ports, and that is deliberate: a second Synapse and a second NATS on
/// a host that is already memory-bound is a cost nobody asked for, and run one
/// after the other the two suites reuse one warm stack, which is what makes them
/// bearable. What they cannot do is run *at once* — each generates its own
/// environment file and `docker compose up -d` on a changed configuration
/// **recreates** the containers, so the second run pulls the stack out from under
/// the first, and the failure that reaches the developer is a timeout or a
/// `Connection reset by peer` in whatever change they happened to be testing.
///
/// `companion-gateway/tests/deployment.rs` already said in prose that the two
/// "can never be running at once". This is that sentence made true: they wait
/// for each other instead of assuming they never meet.
pub async fn hold_deploy_stack(project: &str) -> Result<StackHeld> {
    let path = std::env::temp_dir().join(format!("{project}.deploy-lock"));
    let project = project.to_owned();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open the deploy stack's lock {path:?}"))?;
        // Non-blocking first, so that a wait is *announced* rather than looking
        // like a hung test: the second run of two prints one line and then waits
        // for as long as the first one needs.
        use std::os::fd::AsRawFd;
        let fd = file.as_raw_fd();
        // SAFETY: `fd` is open for the lifetime of `file`, and `flock` touches
        // nothing but the kernel's lock table for it.
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            eprintln!(
                "another run holds the deploy stack {project}; waiting for it \
                 (the two deployment suites share one stack on purpose — #212)"
            );
            // SAFETY: as above.
            if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
                bail!(
                    "failed to take the deploy stack {project}: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        Ok(StackHeld {
            _file: file,
            project,
        })
    })
    .await
    .context("the task holding the deploy stack was cancelled")?
}

// ---------------------------------------------------------------------------
// A stack that cannot start says so, and says it is not the code (#128)
// ---------------------------------------------------------------------------

/// What Docker says when the **host** is out of something, and what frees it.
///
/// One row per exhaustion this project has actually met or can name precisely.
/// Matched on Docker's own words because those are the only stable part of the
/// answer: the exit status is always `1`, and a caller that reported only the
/// status is a caller that said nothing.
const EXHAUSTED: [(&str, &str); 5] = [
    (
        "address pools",
        "the Docker daemon has no subnet left for another network. Every compose \
         project takes one, and a host that also runs unrelated services shares \
         that pool with them.",
    ),
    (
        "already allocated",
        "a host port this stack wants is held by something else — another test \
         stack on the same default port, or a service of the host's own.",
    ),
    (
        "no space left on device",
        "the filesystem Docker builds and stores on is full.",
    ),
    (
        "cannot allocate memory",
        "the host cannot give this stack the memory it asked for.",
    ),
    (
        "Cannot connect to the Docker daemon",
        "the Docker daemon is not answering on this host.",
    ),
];

/// Turns a failed `docker compose up` into an error that says whether the host
/// or the stack is at fault, and what would free it.
///
/// A test that could not *start* used to report as a test that failed, which is
/// the defect #128 is about and not a matter of tidiness: the first reading of
/// `docker compose up failed with exit status 1` is "my change broke the
/// deployment tests", and the second — if the operator is unlucky — is to go
/// looking in the code. This project has shipped that confusion in the product
/// three times (#116, #111, and a login token refused as a wrong password); the
/// test suite does not get to add a fourth.
///
/// So the answer names the exhaustion when Docker named it, counts what is
/// holding the resource, and says the command that frees it — and when Docker
/// said something this function has never heard of, it says *that*, with the
/// whole of Docker's output, rather than inventing a diagnosis.
pub async fn compose_up_failed(
    project: &str,
    status: std::process::ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> anyhow::Error {
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let exhausted = EXHAUSTED
        .iter()
        .find(|(needle, _)| said.contains(needle))
        .map(|(_, explanation)| *explanation);
    let Some(explanation) = exhausted else {
        return anyhow::anyhow!(
            "docker compose up for {project} failed with {status}, and this harness does not \
             recognise the reason. Docker said:\n{}",
            said.trim()
        );
    };
    anyhow::anyhow!(
        "THE HOST, NOT THE CODE: the stack {project} could not be created. {explanation}\n\
         \n\
         This is an environment failure and not a test failure: nothing under test ran. \
         `tools/twalk-test-stacks.sh` lists what this host is holding and prints the command \
         that frees the stale part of it.\n\
         \n\
         {}\n\
         \n\
         Docker said:\n{}",
        host_holdings().await,
        said.trim()
    )
}

/// What this host is holding, in the two numbers an exhausted daemon is about.
///
/// Best-effort and never fatal: this runs on a path that is already failing, and
/// a diagnosis that panicked would replace a bad message with none.
async fn host_holdings() -> String {
    let networks = Command::new("docker")
        .args(["network", "ls", "--format", "{{.Name}}"])
        .output()
        .await
        .ok()
        .map(|done| String::from_utf8_lossy(&done.stdout).into_owned())
        .unwrap_or_default();
    let all = networks.lines().filter(|line| !line.is_empty()).count();
    let ours = networks
        .lines()
        .filter(|line| line.starts_with("twalk"))
        .count();
    let free = Command::new("df")
        .args(["-h", "--output=avail", "/"])
        .output()
        .await
        .ok()
        .map(|done| {
            String::from_utf8_lossy(&done.stdout)
                .lines()
                .nth(1)
                .unwrap_or("?")
                .trim()
                .to_owned()
        })
        .unwrap_or_else(|| "?".to_owned());
    format!(
        "This host holds {all} Docker networks, {ours} of them Twalk's, and has {free} free on /."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lock across a **process** boundary, which is the one it exists for.
    ///
    /// `flock(1)` from util-linux takes the same kernel lock, so another process
    /// holding it is one command — and the assertion is the one that matters:
    /// this side waits for it and then gets the stack. (Linux-only, like every
    /// suite in this repository, which needs Docker and a Synapse.)
    #[tokio::test]
    async fn another_process_holding_the_stack_is_waited_for() {
        let project = format!("twalk-harness-lock-other-{}", std::process::id());
        let path = std::env::temp_dir().join(format!("{project}.deploy-lock"));
        std::fs::write(&path, b"").expect("the lock file");

        let mut other = tokio::process::Command::new("flock")
            .arg("-x")
            .arg(&path)
            .args(["-c", "echo held; sleep 3"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("flock(1) is available: this suite is Linux-only");
        // Wait until it really holds it, rather than racing its startup.
        {
            use tokio::io::AsyncBufReadExt;
            let stdout = other.stdout.take().expect("piped");
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            let held = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
                .await
                .expect("flock started")
                .expect("its output is readable");
            assert_eq!(held.as_deref(), Some("held"));
        }

        let taken = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            hold_deploy_stack(&project),
        )
        .await;
        assert!(
            taken.is_err(),
            "another process holds this stack: this run has to wait for it"
        );

        let held = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            hold_deploy_stack(&project),
        )
        .await
        .expect("the other process lets go")
        .expect("and this run then takes the stack");
        drop(held);
        let _ = other.wait().await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn one_run_waits_for_another_and_then_gets_the_stack() {
        let project = format!(
            "twalk-harness-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock is after the epoch")
                .as_nanos()
        );

        let held = hold_deploy_stack(&project)
            .await
            .expect("the first hold succeeds");

        // A second hold cannot be granted while the first one lives. `flock` is
        // per open file description, so two holds in one process contend exactly
        // as two processes do — which is what makes this testable at all.
        let waiting = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            hold_deploy_stack(&project),
        )
        .await;
        assert!(
            waiting.is_err(),
            "a second run must wait for the first, not be handed the same stack"
        );

        drop(held);
        let after = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            hold_deploy_stack(&project),
        )
        .await
        .expect("the stack is free once the first hold is dropped")
        .expect("and taking it succeeds");
        drop(after);

        let _ = std::fs::remove_file(std::env::temp_dir().join(format!("{project}.deploy-lock")));
    }

    fn failed() -> std::process::ExitStatus {
        // The status every failed `docker compose up` has, which is why it is
        // not what this function reports on.
        std::process::Command::new("false")
            .status()
            .expect("running `false` is possible")
    }

    #[tokio::test]
    async fn an_exhausted_host_is_named_as_the_host_and_not_as_a_failure_of_the_code() {
        let said = b"Error response from daemon: all predefined address pools have been fully \
                     subnetted\nfailed to create network twalk-d112b_default\n";
        let error = compose_up_failed("twalk-d112b", failed(), b"", said).await;
        let message = error.to_string();

        assert!(
            message.starts_with("THE HOST, NOT THE CODE"),
            "the first words decide how the failure is read: {message}"
        );
        assert!(
            message.contains("no subnet left"),
            "it names what was exhausted: {message}"
        );
        assert!(
            message.contains("nothing under test ran"),
            "and says so, because that is the sentence the reader needs: {message}"
        );
        assert!(
            message.contains("tools/twalk-test-stacks.sh"),
            "and the command that frees it: {message}"
        );
        assert!(
            message.contains("Docker networks"),
            "with what this host is actually holding: {message}"
        );
        assert!(
            message.contains("all predefined address pools"),
            "and Docker's own words, never paraphrased: {message}"
        );
    }

    #[tokio::test]
    async fn every_exhaustion_this_harness_names_is_recognised_from_dockers_own_words() {
        for (needle, explanation) in EXHAUSTED {
            let said = format!("Error response from daemon: {needle} something something");
            let message = compose_up_failed("twalk-test", failed(), b"", said.as_bytes())
                .await
                .to_string();
            assert!(
                message.contains(explanation),
                "{needle:?} must be recognised and explained: {message}"
            );
        }
    }

    #[tokio::test]
    async fn a_reason_this_harness_does_not_know_says_that_rather_than_guessing() {
        let said = b"Error response from daemon: something nobody here has met\n";
        let message = compose_up_failed("twalk-test", failed(), b"", said)
            .await
            .to_string();

        assert!(
            message.contains("does not recognise the reason"),
            "an unknown reason is said to be unknown: {message}"
        );
        assert!(
            message.contains("something nobody here has met"),
            "with the whole of what Docker said: {message}"
        );
        assert!(
            !message.contains("THE HOST, NOT THE CODE"),
            "and never claims to know whose fault it is: {message}"
        );
    }
}
