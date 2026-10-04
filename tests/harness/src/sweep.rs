//! The shared stack's devices, swept — the half of #432 the deadline does not
//! reach.
//!
//! Every login in these suites creates a device on the test homeserver and
//! nothing ever removed one. Measured on 2026-10-04, after sixteen days up:
//! **5 636 devices** over 477 accounts, 3 373 of them on `@bot_alpha` alone,
//! 1 553 rooms. A cold client's initial sync pays for all of it, which is what
//! made four separate waits in one suite fail on a deadline (#437 raised the
//! deadline; this stops the growth it was raised to absorb).
//!
//! **What this is not.** It is not a recreation. Parallel sessions on this host
//! share one stack, and tearing it down would kill a sibling suite in flight —
//! so the automatic half only ever deletes devices, and a recreation is an
//! operator's explicit command (`tools/twalk-test-stacks.sh --recreate`). That
//! is the decision taken on #432 against three options.
//!
//! **Why a device is safe to take.** A device unseen for [`UNSEEN_FOR`] cannot
//! belong to a suite that is running: the longest wait in the harness is
//! [`crate::DEADLINE`], two minutes. The rule is one-directional on purpose —
//! the sweep takes a device it can *prove* is old, and keeps every other,
//! including one the homeserver has never seen (see [`sweepable`]).
//!
//! It talks to [`crate::synapse_url`] as `@harness`, a Synapse administrator
//! provisioned by `provision-bots.sh` with a test-only password — so to the
//! shared stack and nothing else: a deployment suite's homeserver answers on
//! another port under another project, and those suites bring their own stacks
//! down. Pointed at a homeserver that is not a test stack, that login simply
//! fails and nothing is swept.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::{synapse_url, SERVER_NAME};

/// The administrator the harness acts as, and the password scheme
/// `provision-bots.sh` gives it. Test-only constants for a local throwaway
/// stack, like every other credential in this crate.
pub const ADMIN_LOCALPART: &str = "harness";

/// How long a device must have gone unseen before the sweep may take it.
///
/// Two days, which is three orders of magnitude more than the longest thing
/// any suite waits for ([`crate::DEADLINE`], 120 s) and two hours more than
/// the longest run measured here. A device older than this cannot be a
/// running suite's, and that is the whole safety argument: the stack is
/// shared, so the sweep may never take anything a sibling run could still be
/// holding.
pub const UNSEEN_FOR: Duration = Duration::from_secs(2 * 24 * 60 * 60);

/// A device as the admin API describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub device_id: String,
    /// When the homeserver last saw this device make an authenticated
    /// request, in milliseconds since the epoch — `None` when it never has.
    pub last_seen_ms: Option<i64>,
}

/// The devices of one account a sweep may take: those the homeserver last saw
/// before `cutoff_ms`.
///
/// **A device the homeserver has never seen is kept**, and that is the rule
/// worth reading twice. `last_seen` is set by the first *authenticated*
/// request, not by the login that created the device, so a device with no
/// timestamp is either a dead login from months ago or a sibling suite's
/// client that logged in a second ago and has not synced yet. Nothing in the
/// answer tells the two apart — so the sweep keeps it, and the 504 such
/// devices measured on 2026-10-04 (9% of the total) are the price of never
/// pulling a device out from under a running test. They are bounded in
/// practice by the thing that creates most of them logging out of its own
/// accord (`sensor/tests/harness/crypto.rs`).
pub fn sweepable(devices: &[Device], cutoff_ms: i64) -> Vec<&str> {
    devices
        .iter()
        .filter(|device| device.last_seen_ms.is_some_and(|seen| seen < cutoff_ms))
        .map(|device| device.device_id.as_str())
        .collect()
}

/// What one sweep did, and what the stack holds after it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Swept {
    /// Accounts the homeserver holds, and which the sweep walked.
    pub accounts: usize,
    /// Devices taken.
    pub deleted: usize,
    /// Devices left: seen since the cutoff, or never seen at all.
    pub kept: usize,
    /// How many of `kept` the homeserver has never seen (see [`sweepable`]).
    pub never_seen: usize,
    /// Accounts the homeserver would not answer about. Counted and skipped
    /// rather than fatal: housekeeping over 478 accounts must not be stopped
    /// by one of them, and a suite deactivating its own throwaway account is
    /// a thing that happens while this walk is in flight.
    pub skipped: usize,
    /// Rooms the homeserver holds, when the whole stack was swept — `None`
    /// when one account was. Not swept: rooms are the other half of the
    /// accumulation and the raised deadline absorbs them (#432). Counted,
    /// because a reader of a slow initial sync needs the number.
    pub rooms: Option<usize>,
    pub took: Duration,
}

impl std::fmt::Display for Swept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "swept {} device(s) unseen for {} day(s) in {:.1}s; {} account(s) and {} device(s) \
             remain ({} the homeserver has never seen, kept on purpose)",
            self.deleted,
            UNSEEN_FOR.as_secs() / 86_400,
            self.took.as_secs_f64(),
            self.accounts,
            self.kept,
            self.never_seen,
        )?;
        if let Some(rooms) = self.rooms {
            write!(f, ", and {rooms} room(s) nothing here sweeps")?;
        }
        if self.skipped > 0 {
            write!(
                f,
                "; {} account(s) the homeserver would not answer about, skipped",
                self.skipped
            )?;
        }
        Ok(())
    }
}

/// Sweeps every account the test homeserver holds.
///
/// Called by [`crate::ensure_stack`], at most once an hour per stack.
pub async fn sweep_stack(unseen_for: Duration) -> Result<Swept> {
    let started = SystemTime::now();
    let admin = Admin::login().await?;
    let cutoff = cutoff_ms(started, unseen_for)?;

    let mut swept = Swept::default();
    let accounts = admin.accounts().await?;
    for user_id in &accounts {
        swept.accounts += 1;
        if admin
            .sweep_account(user_id, cutoff, &mut swept)
            .await
            .is_err()
        {
            swept.skipped += 1;
        }
    }
    if swept.skipped == accounts.len() && !accounts.is_empty() {
        bail!(
            "not one of the {} account(s) could be read: this is the sweep's own fault and not \
             one odd account's",
            accounts.len()
        );
    }
    swept.rooms = Some(admin.rooms().await?);
    swept.took = started.elapsed().unwrap_or_default();
    admin.logout().await;
    Ok(swept)
}

/// Sweeps one account. What the sweep's own suite drives, because a test must
/// never point a cutoff of "now" at accounts a sibling run is using.
pub async fn sweep_account(user_id: &str, unseen_for: Duration) -> Result<Swept> {
    let started = SystemTime::now();
    let admin = Admin::login().await?;
    let cutoff = cutoff_ms(started, unseen_for)?;

    let mut swept = Swept {
        accounts: 1,
        ..Swept::default()
    };
    admin.sweep_account(user_id, cutoff, &mut swept).await?;
    swept.took = started.elapsed().unwrap_or_default();
    admin.logout().await;
    Ok(swept)
}

/// The instant a device must have been seen since in order to be kept.
fn cutoff_ms(now: SystemTime, unseen_for: Duration) -> Result<i64> {
    let since_epoch = now
        .checked_sub(unseen_for)
        .context("the cutoff is before the epoch")?
        .duration_since(UNIX_EPOCH)
        .context("the clock is before the epoch")?;
    Ok(since_epoch.as_millis() as i64)
}

/// A logged-in Synapse administrator on the test stack.
struct Admin {
    http: reqwest::Client,
    token: String,
}

impl Admin {
    async fn login() -> Result<Self> {
        let http = reqwest::Client::new();
        let response = http
            .post(format!("{}/_matrix/client/v3/login", synapse_url()))
            .json(&serde_json::json!({
                "type": "m.login.password",
                "identifier": { "type": "m.id.user", "user": ADMIN_LOCALPART },
                "password": format!("test-only-password-{ADMIN_LOCALPART}"),
                "initial_device_display_name": "twalk-harness-sweep",
            }))
            .send()
            .await
            .context("the test homeserver did not answer a login")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!(
                "the test homeserver refused the harness administrator's login: {status} {body}. \
                 `provision-bots.sh` creates @{ADMIN_LOCALPART}:{SERVER_NAME} with `-a`; a stack \
                 provisioned before #432 has no such account and the next run's provisioning \
                 adds it."
            );
        }
        let token = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|body| {
                body.get("access_token")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .with_context(|| format!("the login answered {body}, which names no access_token"))?;
        Ok(Self { http, token })
    }

    /// Every account the homeserver holds, deactivated ones excepted — those
    /// have no devices left to take.
    async fn accounts(&self) -> Result<Vec<String>> {
        let mut from = 0_usize;
        let mut accounts = Vec::new();
        loop {
            let page: Value = self
                .get(&format!(
                    "/_synapse/admin/v2/users?from={from}&limit=500&deactivated=false"
                ))
                .await?;
            let users = page
                .get("users")
                .and_then(Value::as_array)
                .context("the admin user list names no users")?;
            for user in users {
                if let Some(name) = user.get("name").and_then(Value::as_str) {
                    accounts.push(name.to_owned());
                }
            }
            match page.get("next_token").and_then(Value::as_str) {
                // Synapse pages by offset and answers the next one as a
                // string; it is absent on the last page.
                Some(next) => from = next.parse().context("next_token is not an offset")?,
                None => return Ok(accounts),
            }
        }
    }

    async fn rooms(&self) -> Result<usize> {
        let page: Value = self.get("/_synapse/admin/v1/rooms?limit=1").await?;
        Ok(page
            .get("total_rooms")
            .and_then(Value::as_u64)
            .unwrap_or_default() as usize)
    }

    async fn sweep_account(&self, user_id: &str, cutoff_ms: i64, into: &mut Swept) -> Result<()> {
        let listed: Value = self
            .get(&format!("/_synapse/admin/v2/users/{user_id}/devices"))
            .await
            .with_context(|| format!("the devices of {user_id} could not be listed"))?;
        let devices: Vec<Device> = listed
            .get("devices")
            .and_then(Value::as_array)
            .context("the admin device list names no devices")?
            .iter()
            .map(|device| Device {
                device_id: device
                    .get("device_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                last_seen_ms: device.get("last_seen_ts").and_then(Value::as_i64),
            })
            .collect();

        let take = sweepable(&devices, cutoff_ms);
        into.deleted += take.len();
        into.kept += devices.len() - take.len();
        into.never_seen += devices
            .iter()
            .filter(|device| device.last_seen_ms.is_none())
            .count();

        // In batches, because the first sweep of an old stack has thousands to
        // take and most of them on one account — `@bot_alpha` alone held 3 373
        // devices on 2026-10-04, of the stack's 5 636 — and one request that
        // large is a transaction nobody can see the progress of.
        for batch in take.chunks(200) {
            let response = self
                .http
                .post(format!(
                    "{}/_synapse/admin/v2/users/{user_id}/delete_devices",
                    synapse_url()
                ))
                .bearer_auth(&self.token)
                .json(&serde_json::json!({ "devices": batch }))
                .send()
                .await
                .with_context(|| format!("the devices of {user_id} could not be deleted"))?;
            if !response.status().is_success() {
                let status = response.status();
                bail!(
                    "the test homeserver refused to delete {} device(s) of {user_id}: {status} {}",
                    batch.len(),
                    response.text().await.unwrap_or_default()
                );
            }
        }
        Ok(())
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let response = self
            .http
            .get(format!("{}{path}", synapse_url()))
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("the test homeserver did not answer GET {path}"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("the test homeserver answered GET {path} with {status} {body}");
        }
        serde_json::from_str(&body).with_context(|| format!("GET {path} answered {body}"))
    }

    /// Takes the device this sweep logged in with, so that the housekeeping
    /// does not itself accumulate what it came to remove.
    async fn logout(&self) {
        let _ = self
            .http
            .post(format!("{}/_matrix/client/v3/logout", synapse_url()))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({}))
            .send()
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::{cutoff_ms, sweepable, Device, UNSEEN_FOR};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn device(id: &str, last_seen_ms: Option<i64>) -> Device {
        Device {
            device_id: id.to_owned(),
            last_seen_ms,
        }
    }

    #[test]
    fn a_device_unseen_since_before_the_cutoff_is_taken_and_a_recent_one_is_kept() {
        let devices = [
            device("OLD", Some(1_000)),
            device("RECENT", Some(3_000)),
            device("EXACTLY_AT_THE_CUTOFF", Some(2_000)),
        ];
        // At the cutoff is not before it: a device seen at the instant the
        // window opens is inside the window.
        assert_eq!(vec!["OLD"], sweepable(&devices, 2_000));
    }

    #[test]
    fn a_device_the_homeserver_has_never_seen_is_kept_whatever_the_cutoff() {
        // `last_seen` is set by the first authenticated request and not by the
        // login that created the device, so no timestamp means "a dead login,
        // or a sibling suite's client that has not synced yet" — and nothing
        // here can tell those apart. The stack is shared; the sweep keeps it.
        let devices = [device("NEVER_SEEN", None)];
        assert!(sweepable(&devices, i64::MAX).is_empty());
    }

    #[test]
    fn the_cutoff_is_the_window_before_now() {
        let now = UNIX_EPOCH + Duration::from_secs(10 * 86_400);
        assert_eq!(
            Ok(8 * 86_400 * 1_000),
            cutoff_ms(now, UNSEEN_FOR).map_err(|_| ())
        );
        // A clock before the window is a refusal rather than a cutoff in the
        // future, which would sweep everything.
        assert!(cutoff_ms(UNIX_EPOCH, UNSEEN_FOR).is_err());
        assert!(cutoff_ms(SystemTime::now(), UNSEEN_FOR).is_ok());
    }
}
