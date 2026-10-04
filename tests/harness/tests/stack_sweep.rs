//! The device sweep, against the real test homeserver (#432).
//!
//! The harness's other tests need no Docker; these do, because the thing under
//! test is what a homeserver does with a device — and the two properties that
//! matter cannot be observed anywhere else. One: a device the sweep took is
//! *gone*, which is its token answering `M_UNKNOWN_TOKEN` and not merely its
//! absence from a list the sweep itself read. Two: a device the homeserver has
//! never seen survives a cutoff that would take everything, because on a stack
//! several sessions share, a login that has not synced yet may be a sibling
//! suite's.
//!
//! **Every account here is this run's own**, registered with a unique localpart
//! and deactivated at the end. A test may not point a cutoff of "now" at the
//! shared bots: that would delete the devices of whatever is running beside it,
//! which is the one thing #432's decision forbids.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use twalk_test_harness::{
    ensure_stack, poll_until, stack_report, sweep_account, synapse_url, ADMIN_LOCALPART, UNSEEN_FOR,
};

/// One logged-in device of a throwaway account.
struct Session {
    device_id: String,
    token: String,
}

/// An account of this run's own, with devices it can afford to lose.
struct Throwaway {
    localpart: String,
    user_id: String,
    http: reqwest::Client,
}

impl Throwaway {
    async fn register() -> Result<Self> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let localpart = format!("sweep_subject_{}_{unique}", std::process::id());
        let http = reqwest::Client::new();
        let response = http
            .post(format!("{}/_matrix/client/v3/register", synapse_url()))
            .json(&serde_json::json!({
                "username": localpart,
                "password": format!("test-only-password-{localpart}"),
                "auth": { "type": "m.login.dummy" },
                "inhibit_login": true,
            }))
            .send()
            .await?;
        if !response.status().is_success() {
            bail!(
                "the test homeserver refused to register {localpart}: {} {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }
        let user_id = format!("@{localpart}:test.twalk");
        Ok(Self {
            localpart,
            user_id,
            http,
        })
    }

    /// Logs in a device and returns its id and access token.
    async fn log_in(&self) -> Result<Session> {
        let response = self
            .http
            .post(format!("{}/_matrix/client/v3/login", synapse_url()))
            .json(&serde_json::json!({
                "type": "m.login.password",
                "identifier": { "type": "m.id.user", "user": self.localpart },
                "password": format!("test-only-password-{}", self.localpart),
            }))
            .send()
            .await?
            .error_for_status()?;
        let body: Value = response.json().await?;
        Ok(Session {
            device_id: body
                .get("device_id")
                .and_then(Value::as_str)
                .context("the login answered no device_id")?
                .to_owned(),
            token: body
                .get("access_token")
                .and_then(Value::as_str)
                .context("the login answered no access_token")?
                .to_owned(),
        })
    }

    /// Makes the homeserver *see* a device, and waits until it admits it has.
    ///
    /// `last_seen` is set by the first authenticated request rather than by the
    /// login that created the device — and Synapse writes it in batches, so it
    /// is still null for a few seconds after that request succeeds. Measured
    /// here: the sweep read three devices as never-seen immediately after two
    /// of them had made an authenticated call. A test that asserted on it
    /// straight away would have called that a defect in the sweep.
    async fn seen(&self, session: &Session) -> Result<()> {
        if self.whoami(&session.token).await? != 200 {
            bail!("a token the homeserver just minted was refused");
        }
        let device_id = session.device_id.clone();
        poll_until(
            || async {
                self.own_devices(&session.token)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|(id, last_seen)| *id == device_id && last_seen.is_some())
            },
            &format!(
                "the homeserver to record that it has seen {}",
                session.device_id
            ),
        )
        .await?;
        Ok(())
    }

    /// The account's own devices, each with the last time it was seen.
    async fn own_devices(&self, token: &str) -> Result<Vec<(String, Option<i64>)>> {
        let body: Value = self
            .http
            .get(format!("{}/_matrix/client/v3/devices", synapse_url()))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(body
            .get("devices")
            .and_then(Value::as_array)
            .context("the device list names no devices")?
            .iter()
            .map(|device| {
                (
                    device
                        .get("device_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    device.get("last_seen_ts").and_then(Value::as_i64),
                )
            })
            .collect())
    }

    async fn whoami(&self, token: &str) -> Result<u16> {
        Ok(self
            .http
            .get(format!(
                "{}/_matrix/client/v3/account/whoami",
                synapse_url()
            ))
            .bearer_auth(token)
            .send()
            .await?
            .status()
            .as_u16())
    }

    /// Deactivates the account, so that a suite written against the
    /// accumulation does not become a source of it.
    async fn deactivate(&self) {
        let _ = self
            .http
            .post(format!(
                "{}/_matrix/client/v3/account/deactivate",
                synapse_url()
            ))
            .json(&serde_json::json!({
                "auth": {
                    "type": "m.login.password",
                    "identifier": { "type": "m.id.user", "user": self.localpart },
                    "password": format!("test-only-password-{}", self.localpart),
                },
                "erase": true,
            }))
            .send()
            .await;
    }
}

#[tokio::test]
async fn a_device_the_homeserver_has_seen_is_taken_and_one_it_has_never_seen_is_kept() -> Result<()>
{
    ensure_stack().await?;
    let account = Throwaway::register().await?;

    let seen_one = account.log_in().await?;
    let seen_two = account.log_in().await?;
    let never_seen = account.log_in().await?;
    account.seen(&seen_one).await?;
    account.seen(&seen_two).await?;

    // A cutoff of "now": every device the homeserver has ever seen is older
    // than it. Safe here and only here, because the account is this run's own.
    let swept = sweep_account(&account.user_id, Duration::ZERO).await?;

    let outcome = async {
        assert_eq!(2, swept.deleted, "the two seen devices: {swept:?}");
        assert_eq!(1, swept.kept, "the never-seen one: {swept:?}");
        assert_eq!(1, swept.never_seen, "and it is kept for that reason");

        // The property no device list can give: the token is refused, so the
        // device is gone from the homeserver and not merely from the answer
        // the sweep read.
        assert_eq!(401, account.whoami(&seen_one.token).await?);
        assert_eq!(401, account.whoami(&seen_two.token).await?);
        assert_eq!(
            200,
            account.whoami(&never_seen.token).await?,
            "a login that has not synced yet may be a sibling suite's"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;

    account.deactivate().await;
    outcome
}

#[tokio::test]
async fn a_device_seen_inside_the_window_is_left_alone() -> Result<()> {
    ensure_stack().await?;
    let account = Throwaway::register().await?;

    let session = account.log_in().await?;
    account.seen(&session).await?;

    // The window the stack is actually swept with. A device seen a moment ago
    // is inside it, and the sweep that runs in `ensure_stack` on every suite
    // must not be able to take it.
    let swept = sweep_account(&account.user_id, UNSEEN_FOR).await?;

    let outcome = async {
        assert_eq!(0, swept.deleted, "nothing is old enough yet: {swept:?}");
        assert_eq!(1, swept.kept);
        assert_eq!(0, swept.never_seen, "this one has been seen");
        assert_eq!(200, account.whoami(&session.token).await?);
        Ok::<(), anyhow::Error>(())
    }
    .await;

    account.deactivate().await;
    outcome
}

/// How many devices the harness administrator holds — counted from its own
/// account, with a login this function also takes away again, so that two
/// calls either side of a sweep are comparable.
async fn devices_of_the_administrator() -> Result<usize> {
    let http = reqwest::Client::new();
    let body: Value = http
        .post(format!("{}/_matrix/client/v3/login", synapse_url()))
        .json(&serde_json::json!({
            "type": "m.login.password",
            "identifier": { "type": "m.id.user", "user": ADMIN_LOCALPART },
            "password": format!("test-only-password-{ADMIN_LOCALPART}"),
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = body
        .get("access_token")
        .and_then(Value::as_str)
        .context("the administrator's login answered no access_token")?;

    let devices: Value = http
        .get(format!("{}/_matrix/client/v3/devices", synapse_url()))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let count = devices
        .get("devices")
        .and_then(Value::as_array)
        .context("the device list names no devices")?
        .len();

    let _ = http
        .post(format!("{}/_matrix/client/v3/logout", synapse_url()))
        .bearer_auth(token)
        .json(&serde_json::json!({}))
        .send()
        .await;
    Ok(count)
}

#[tokio::test]
async fn the_sweep_leaves_no_device_of_its_own_behind() -> Result<()> {
    ensure_stack().await?;
    let account = Throwaway::register().await?;

    // The sweep logs in as the harness administrator to read the admin API,
    // and a login is a device: housekeeping that accumulated what it came to
    // remove would be the defect it is fixing, one layer up. It logs out.
    //
    // Nothing else sweeps while this runs: `ensure_stack` above has just left
    // the stack's sweep marker fresh, and that is what keeps another suite
    // starting beside this one from sweeping for the next hour.
    let before = devices_of_the_administrator().await?;
    sweep_account(&account.user_id, UNSEEN_FOR).await?;
    let after = devices_of_the_administrator().await?;

    account.deactivate().await;
    assert_eq!(
        before, after,
        "the sweep's own login is still on the homeserver"
    );
    Ok(())
}

#[tokio::test]
async fn the_stack_says_how_old_its_data_is_and_what_recreates_it() -> Result<()> {
    ensure_stack().await?;

    // The line a reader of a failing wait needs, and the reason it exists: a
    // twenty-second timeout on a fortnight-old stack reads as a defect in the
    // component under test (#432). It is printed by `ensure_stack`, so it is
    // in the captured output of whichever test failed.
    let report = stack_report().await;
    assert!(
        report.contains("has held data for"),
        "the report says nothing about the stack's age: {report}"
    );
    assert!(
        report.contains("--recreate"),
        "the report does not say what starts the stack over: {report}"
    );
    Ok(())
}
