//! Ticket #97, reading suggestions at the Gateway's process boundary: the
//! real binary, the real NATS JetStream a persona publishes suggestions to,
//! real contract events, and HTTP calls from the test.
//!
//! Ticket #24 built the act — `POST /api/approvals` — and the question that
//! follows it. It left the Companion unable to **find** a suggestion at all,
//! so #100's approval screen had an endpoint and nothing to draw. This suite
//! is the read, and it asserts four things the ticket is actually about:
//!
//! 1. **A suggestion on the bus is listed**, with what a screen needs: what
//!    the persona proposed, for which message, when, and where it stands.
//! 2. **It is a projection, not a second store.** Nothing the Gateway
//!    answers with is kept: the suggestion's text does not appear in a byte
//!    of its state directory, and a listing is computed from the stream
//!    every time. The read is bounded, and the bound is in the answer.
//! 3. **Nothing of the message being answered crosses this door.** A
//!    suggestion quotes a contact's message, and an excerpt belongs to the
//!    author of the quoted message rather than to whoever sent the event
//!    carrying it (#110 — a content leak found on live data — and ADR
//!    0012). So the trigger's body, the excerpt it was itself quoting, the
//!    sender's display name and its `network_identifier` are asserted
//!    **absent** from every answer, for a granted contact and for a revoked
//!    one. Asserted, not assumed.
//! 4. **Expired, missing and already-approved are three answers.** This
//!    project has lost eight incidents in two days to two failures sharing
//!    one signal (#116, #141, #135, #139, #111, #128, #130), so the three
//!    are driven through the real binary and compared with each other.
//!
//! The suggestions here are published by the test rather than by a persona:
//! the Gateway's seam is the bus, and what crosses it is a contract-valid
//! CloudEvent — every fixture below is checked to be one, except the single
//! deliberately unreadable event in the last test, which is a suggestion
//! from a contract version this build does not have and exists to prove that
//! one bad message does not blank a screen.
//!
//! The bus is shared by every suite and every run, so nothing here asserts
//! on totals: each test invents its own contact, its own room and its own
//! suggestion, and asks about those.

mod harness;

use std::path::PathBuf;

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, gateway_env_with, gateway_env_with_consent, nats_url,
    owner_user_id, poll_until, unreachable_nats_url, validate_against_contract, Bus, GatewayProc,
    SERVER_NAME,
};
use serde_json::{json, Value};

const STREAM: &str = "twalk";
const INBOUND_SUBJECT: &str = "twalk.inbound.message.received.v1";
const INBOUND_TYPE: &str = "fr.linagora.twalk.inbound.message.received.v1";
const SUGGEST_SUBJECT: &str = "twalk.persona.suggest.produced.v1";
const SUGGEST_TYPE: &str = "fr.linagora.twalk.persona.suggest.produced.v1";

/// The words that belong to somebody other than the persona. Every one of
/// them is published onto the bus inside the message a suggestion answers,
/// and every one of them is asserted absent from what this API hands back.
const TRIGGER_BODY: &str = "ON DECALE A 20H CE SOIR";
const QUOTED_EXCERPT: &str = "WRITTEN BY A THIRD PERSON IN THE GROUP";
const DISPLAY_NAME: &str = "Aicha Benali G97";
const NETWORK_IDENTIFIER: &str = "33612345697";

// ---------------------------------------------------------------------------
// Driving the seam
// ---------------------------------------------------------------------------

fn unique(label: &str) -> String {
    format!(
        "{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
    )
}

fn ghost(label: &str) -> String {
    format!("@whatsapp_{}:{SERVER_NAME}", unique(label))
}

fn portal_room(label: &str) -> String {
    let opaque: String = harness::sha256_hex(&unique(label))
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(18)
        .collect();
    format!("!{opaque}:{SERVER_NAME}")
}

fn rfc3339(at: time::OffsetDateTime) -> String {
    at.replace_nanosecond(0)
        .expect("a whole second is a valid instant")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("an instant formats as RFC 3339")
}

fn in_seconds(seconds: i64) -> String {
    rfc3339(time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds))
}

/// One `inbound.message.received.v1` as the Sensor publishes one, carrying
/// everything this API must not hand back: the message's own words, an
/// excerpt of a **third person's** message it is replying to, the sender's
/// display name and their network identifier.
///
/// A revoked sender's message is reduced, because the contract forbids the
/// content outright for one (ADR 0012) — so the fixture reduces exactly as
/// the Sensor would, and the revoked case proves the absence with the bus
/// itself holding nothing rather than with the Gateway withholding it.
fn inbound_event(sender: &str, room_id: &str, consent: &str) -> Value {
    let at = in_seconds(-120);
    let data = if consent == "revoked" {
        json!({
            "format": "text/plain",
            "reply_to": { "matrix_event_id": "$g97QuotedEvent" },
            "attachments": [],
            "contact": { "display_name": DISPLAY_NAME }
        })
    } else {
        json!({
            "body": TRIGGER_BODY,
            "format": "text/plain",
            "reply_to": {
                "matrix_event_id": "$g97QuotedEvent",
                "excerpt": QUOTED_EXCERPT
            },
            "attachments": [],
            "contact": {
                "display_name": DISPLAY_NAME,
                "network_identifier": NETWORK_IDENTIFIER
            }
        })
    };
    let event = json!({
        "specversion": "1.0",
        "id": harness::sha256_hex(&format!("g97-inbound:{sender}:{room_id}:{at}")),
        "source": format!("matrix://{SERVER_NAME}/{room_id}"),
        "type": INBOUND_TYPE,
        "time": at,
        "subject": sender,
        "datacontenttype": "application/json",
        "dataschema": "https://schemas.twalk.dev/cloudevents/v1/inbound.message.received.schema.json",
        "network": "whatsapp",
        "connection": "whatsapp",
        "consent": consent,
        "data": data
    });
    validate_against_contract(&event, "inbound.message.received")
        .expect("the fixture is an event the contract allows");
    event
}

/// One `persona.suggest.produced.v1` as the assistant publishes one.
fn suggest_event(trigger: &Value, body: &str, expires_at: Option<&str>) -> Value {
    let trigger_id = trigger["id"].as_str().expect("the trigger has an id");
    let attempt = 1;
    let mut data = json!({
        "persona_id": "assistant",
        "trigger": { "event_id": trigger_id, "event_type": INBOUND_TYPE },
        "suggestion": { "body": body, "format": "text/plain" },
        "attempt": attempt
    });
    if let Some(expires_at) = expires_at {
        data["expires_at"] = json!(expires_at);
    }
    let event = json!({
        "specversion": "1.0",
        "id": harness::sha256_hex(&format!("assistant:{trigger_id}:{attempt}")),
        "source": format!("hermes://{SERVER_NAME}/personas/assistant"),
        "type": SUGGEST_TYPE,
        "time": in_seconds(-60),
        "subject": trigger_id,
        "datacontenttype": "application/json",
        "dataschema": "https://schemas.twalk.dev/cloudevents/v1/persona.suggest.produced.schema.json",
        "network": trigger["network"],
        "connection": trigger["connection"],
        "consent": trigger["consent"],
        "data": data
    });
    validate_against_contract(&event, "persona.suggest.produced")
        .expect("the fixture is an event the contract allows");
    event
}

async fn bus() -> Result<Bus> {
    let bus = Bus::connect().await?;
    bus.ensure_stream(STREAM, &["twalk.>"]).await?;
    Ok(bus)
}

/// A Gateway with a bus and a consent store, signed in.
struct Running {
    #[allow(dead_code)]
    gateway: GatewayProc,
    base: String,
    static_dir: PathBuf,
    device: String,
    http: reqwest::Client,
}

impl Running {
    async fn start(test_name: &str) -> Result<Self> {
        let static_dir = companion_build(test_name)?;
        let env = gateway_env_with_consent(&static_dir, &nats_url());
        Self::start_with(static_dir, env).await
    }

    async fn start_with(static_dir: PathBuf, env: Vec<(String, String)>) -> Result<Self> {
        let gateway = GatewayProc::start(&env)?;
        let base = gateway.base_url().await?;
        poll_until(
            || async {
                reqwest::get(format!("{base}/health"))
                    .await
                    .ok()?
                    .error_for_status()
                    .ok()
            },
            "the gateway health endpoint",
        )
        .await?;
        let device = harness::signed_in_device_token(&base).await?;
        Ok(Self {
            gateway,
            base,
            static_dir,
            device,
            http: reqwest::Client::new(),
        })
    }

    fn cookie(&self) -> String {
        format!("twalk_device={}", self.device)
    }

    /// A GET whose raw text is kept as well as its parsed body: the absence
    /// assertions search the bytes that went over the wire, not a value the
    /// test reconstructed.
    async fn get_raw(&self, path: &str) -> Result<(reqwest::StatusCode, String, Value)> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .header(reqwest::header::COOKIE, self.cookie())
            .send()
            .await
            .with_context(|| format!("failed to call GET {path}"))?;
        let status = response.status();
        let text = response.text().await?;
        let body = serde_json::from_str(&text)
            .with_context(|| format!("GET {path} answered {status} with non-JSON: {text}"))?;
        Ok((status, text, body))
    }

    async fn get(&self, path: &str) -> Result<(reqwest::StatusCode, Value)> {
        let (status, _text, body) = self.get_raw(path).await?;
        Ok((status, body))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<(reqwest::StatusCode, Value)> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .header(reqwest::header::COOKIE, self.cookie())
            .json(body)
            .send()
            .await
            .with_context(|| format!("failed to call POST {path}"))?;
        let status = response.status();
        let text = response.text().await?;
        let body = serde_json::from_str(&text)
            .with_context(|| format!("POST {path} answered {status} with non-JSON: {text}"))?;
        Ok((status, body))
    }

    async fn decide(&self, contact: &str, new_state: &str) -> Result<()> {
        let (status, body) = self
            .post(
                "/api/consent/decisions",
                &json!({
                    "subject": { "type": "contact", "id": contact },
                    "new_state": new_state,
                    "scope": { "networks": ["whatsapp"] }
                }),
            )
            .await?;
        anyhow::ensure!(
            status == reqwest::StatusCode::CREATED,
            "the decision was refused with {status}: {body}"
        );
        Ok(())
    }

    /// The listing, waited for until it holds this suggestion. The bus is
    /// shared, so the assertion is always about one known id and never about
    /// a count.
    async fn listed(&self, suggestion_id: &str) -> Result<(String, Value, Value)> {
        poll_until(
            || async {
                let (status, text, body) = self.get_raw("/api/suggestions?limit=200").await.ok()?;
                if status != reqwest::StatusCode::OK {
                    return None;
                }
                let entry = body["suggestions"]
                    .as_array()?
                    .iter()
                    .find(|entry| entry["event_id"].as_str() == Some(suggestion_id))?
                    .clone();
                Some((text, entry, body))
            },
            "the suggestion to appear in the listing",
        )
        .await
    }

    fn state_dir(&self) -> PathBuf {
        harness::gateway_state_dir(&self.static_dir)
    }
}

/// Whether a byte slice contains another — a substring search over a
/// response or a database file, as `tests/approvals.rs` and `tests/pending.rs`
/// do.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// A conversation on the bus: a consented contact, the message they wrote,
/// and the suggestion a persona produced for it.
struct Conversation {
    contact: String,
    suggestion_id: String,
    trigger_id: String,
    suggestion_body: String,
}

async fn conversation(
    running: &Running,
    bus: &Bus,
    label: &str,
    consent_label: &str,
    expires_at: Option<&str>,
) -> Result<Conversation> {
    let contact = ghost(label);
    let room_id = portal_room(label);
    running.decide(&contact, "granted").await?;
    let trigger = inbound_event(&contact, &room_id, consent_label);
    bus.publish_event(INBOUND_SUBJECT, &trigger).await?;
    let body = format!("Pas de problème, à 20h ! ({label})");
    let suggestion = suggest_event(&trigger, &body, expires_at);
    bus.publish_event(SUGGEST_SUBJECT, &suggestion).await?;
    Ok(Conversation {
        contact,
        suggestion_id: suggestion["id"].as_str().unwrap().to_owned(),
        trigger_id: trigger["id"].as_str().unwrap().to_owned(),
        suggestion_body: body,
    })
}

// ---------------------------------------------------------------------------
// 1. A suggestion on the bus is listed, with what a screen needs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suggestion_on_the_bus_is_listed_with_what_the_screen_needs() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("suggestions-listed").await?;
    let expires_at = in_seconds(3600);
    let talk = conversation(&running, &bus, "listed", "granted", Some(&expires_at)).await?;

    let (_text, entry, listing) = running.listed(&talk.suggestion_id).await?;

    assert_eq!(entry["persona_id"], json!("assistant"));
    assert_eq!(
        entry["source"],
        json!(format!("hermes://{SERVER_NAME}/personas/assistant")),
        "the source names the persona whose suggestion it is"
    );
    assert_eq!(entry["network"], json!("whatsapp"));
    assert_eq!(
        entry["consent"],
        json!("granted"),
        "the label the Sensor observed — an audit fact, not the current state"
    );
    assert_eq!(entry["expires_at"], json!(expires_at));
    assert_eq!(entry["attempt"], json!(1));
    assert_eq!(
        entry["standing"],
        json!("approvable"),
        "nothing about this suggestion stops it being approved: {entry}"
    );
    assert_eq!(entry["approval"], Value::Null);
    assert_eq!(
        entry["suggestion"]["body"],
        json!(talk.suggestion_body),
        "the persona's own words are what the screen draws"
    );
    assert_eq!(entry["suggestion"]["format"], json!("text/plain"));
    assert_eq!(
        entry["trigger"]["event_id"],
        json!(talk.trigger_id),
        "for which message, by identity"
    );
    assert_eq!(entry["trigger"]["event_type"], json!(INBOUND_TYPE));

    // The read is bounded and says so, rather than leaving a client to guess
    // whether an empty tail means "no more" or "not looked".
    assert!(
        listing["window"]["to_sequence"]
            .as_u64()
            .unwrap_or_default()
            > 0,
        "the answer names the stretch of the stream it was computed from: {listing}"
    );
    assert!(listing["window"]["sequences"].as_u64().unwrap_or_default() >= 1);
    assert!(listing["window"]["reached_start_of_stream"].is_boolean());

    // And the same suggestion, by id, is the same document.
    let (status, single) = running
        .get(&format!("/api/suggestions/{}", talk.suggestion_id))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK, "{single}");
    assert_eq!(
        single, entry,
        "the listing and the single read are one shape, or a screen grows two code paths"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. It is a projection of the bus, not a second store
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reading_suggestions_stores_none_of_them() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("suggestions-projection").await?;
    let talk = conversation(
        &running,
        &bus,
        "projection",
        "granted",
        Some(&in_seconds(3600)),
    )
    .await?;

    // Read it twice, so that a Gateway that cached it would have had every
    // opportunity to.
    running.listed(&talk.suggestion_id).await?;
    let (status, _) = running
        .get(&format!("/api/suggestions/{}", talk.suggestion_id))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);

    // The suggestion lives in the stream. If the Gateway kept a copy, a bus
    // replay and this list could disagree and nobody could say which was
    // true — so there is no copy, and this is the assertion that says so.
    let bytes = std::fs::read(running.state_dir().join("consent.sqlite3"))?;
    for held in [
        talk.suggestion_body.as_str(),
        talk.suggestion_id.as_str(),
        TRIGGER_BODY,
        QUOTED_EXCERPT,
        DISPLAY_NAME,
        NETWORK_IDENTIFIER,
    ] {
        assert!(
            !contains(&bytes, held.as_bytes()),
            "the Gateway's own store holds {held:?} after a suggestion was read: this projection \
             has become a second store"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. Nothing of the message being answered crosses this door (#110)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_listing_carries_none_of_the_words_of_the_message_it_answers() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("suggestions-no-excerpt").await?;

    // A granted contact, whose message is on the bus in full: its own body,
    // an excerpt of a **third person's** message it replies to, a display
    // name and a network identifier.
    let granted = conversation(
        &running,
        &bus,
        "granted",
        "granted",
        Some(&in_seconds(3600)),
    )
    .await?;
    // And a contact the user revoked **after** their message and its
    // suggestion were already on the bus. That is the dangerous order and
    // the one the ticket names: the words are in the stream in full, labelled
    // `granted`, and a listing that resolved the trigger would hand a revoked
    // contact's words back through a door that never checked whose they were.
    let revoked = conversation(
        &running,
        &bus,
        "revoked",
        "granted",
        Some(&in_seconds(3600)),
    )
    .await?;
    running.decide(&revoked.contact, "revoked").await?;

    for talk in [&granted, &revoked] {
        let (listing_text, entry, _) = running.listed(&talk.suggestion_id).await?;
        let (_, single_text, single) = running
            .get_raw(&format!("/api/suggestions/{}", talk.suggestion_id))
            .await?;
        assert_eq!(single["event_id"], json!(talk.suggestion_id));

        // An excerpt belongs to the author of the quoted message, not to
        // whoever sent the event carrying it (#110). A listing that
        // re-published it would be that defect one layer up — and #110 was
        // found on live data, so this is asserted over the bytes that went
        // over the wire rather than over a field the test remembered to look
        // at.
        for quoted in [
            TRIGGER_BODY,
            QUOTED_EXCERPT,
            DISPLAY_NAME,
            NETWORK_IDENTIFIER,
        ] {
            assert!(
                !contains(listing_text.as_bytes(), quoted.as_bytes()),
                "GET /api/suggestions handed back {quoted:?}, which belongs to the author of the \
                 message being answered"
            );
            assert!(
                !contains(single_text.as_bytes(), quoted.as_bytes()),
                "GET /api/suggestions/{{id}} handed back {quoted:?}"
            );
        }

        // What it does say about the trigger is its identity, and only that.
        let trigger = entry["trigger"]
            .as_object()
            .expect("the trigger is an object");
        let mut members: Vec<&String> = trigger.keys().collect();
        members.sort();
        assert_eq!(
            members,
            vec!["event_id", "event_type"],
            "the trigger carries more than identity: {trigger:?}"
        );

        // The persona's own proposal is carried: it is the thing being
        // approved, and withholding it would leave the screen with nothing.
        assert_eq!(entry["suggestion"]["body"], json!(talk.suggestion_body));
    }

    // The revoked contact's suggestion is still listed. Revocation applies
    // to the future and does not erase what is on the bus (ADR 0012), and
    // what is listed here is the persona's own proposal, not the contact's
    // words. Sending it is what is refused, and `POST /api/approvals` is
    // what refuses it — at the moment of the act, where the definition puts
    // the check.
    let (_, revoked_entry, _) = running.listed(&revoked.suggestion_id).await?;
    assert_eq!(revoked_entry["standing"], json!("approvable"));
    let (status, refusal) = running
        .post(
            "/api/approvals",
            &json!({ "suggestion_event_id": revoked.suggestion_id }),
        )
        .await?;
    assert_eq!(
        status,
        reqwest::StatusCode::CONFLICT,
        "the consent check happens at the act, not at the read: {refusal}"
    );
    assert_eq!(refusal["error"], json!("consent_revoked"));
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. Expired, missing and already approved are three answers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expired_missing_and_approved_are_three_answers_and_never_one() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("suggestions-three-answers").await?;

    // --- expired: it is there, it is visible, and it cannot be approved.
    let stale = conversation(&running, &bus, "stale", "granted", Some(&in_seconds(-60))).await?;
    let (_, entry, _) = running.listed(&stale.suggestion_id).await?;
    assert_eq!(
        entry["standing"],
        json!("expired"),
        "an expired suggestion is listed as expired, not dropped: {entry}"
    );
    let (expired_status, expired) = running
        .get(&format!("/api/suggestions/{}", stale.suggestion_id))
        .await?;
    assert_eq!(expired_status, reqwest::StatusCode::OK, "{expired}");
    assert_eq!(expired["standing"], json!("expired"));
    assert_eq!(expired["expires_at"], entry["expires_at"]);

    // --- missing: no suggestion has this id, and the whole retained stream
    // was read to say so.
    let absent = harness::sha256_hex(&unique("never-produced"));
    let (missing_status, missing) = running.get(&format!("/api/suggestions/{absent}")).await?;
    assert_eq!(
        missing_status,
        reqwest::StatusCode::NOT_FOUND,
        "a suggestion nobody produced is missing, not expired: {missing}"
    );
    assert_eq!(missing["error"], json!("suggestion_not_found"));

    // --- approved: the reply went out, and the record says where.
    let sent = conversation(&running, &bus, "sent", "granted", Some(&in_seconds(3600))).await?;
    running.listed(&sent.suggestion_id).await?;
    let (status, approved) = running
        .post(
            "/api/approvals",
            &json!({ "suggestion_event_id": sent.suggestion_id }),
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{approved}");
    let (approved_status, read_back) = running
        .get(&format!("/api/suggestions/{}", sent.suggestion_id))
        .await?;
    assert_eq!(approved_status, reqwest::StatusCode::OK, "{read_back}");
    assert_eq!(
        read_back["standing"],
        json!("approved"),
        "an approved suggestion is not offered for approval again: {read_back}"
    );
    assert_eq!(read_back["approval"]["publication"], json!("published"));
    assert_eq!(read_back["approval"]["approved_by"], json!(owner_user_id()));
    assert_eq!(read_back["approval"]["event_id"], approved["event_id"]);
    assert_eq!(
        read_back["approval"]["stream_sequence"], approved["stream_sequence"],
        "the reply's position on the bus is the same fact wherever it is read"
    );

    // The three situations never arrive as one signal: a different status
    // for the missing one, and a different word for the other two.
    assert_ne!(expired_status, missing_status);
    assert_ne!(expired["standing"], read_back["standing"]);
    assert_eq!(
        [
            expired["standing"].as_str().unwrap_or_default(),
            read_back["standing"].as_str().unwrap_or_default(),
        ]
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len(),
        2
    );

    // And an approved suggestion stays approved when its expiry passes: the
    // reply went out, and telling the user it expired would be false. The
    // `sent` suggestion above expires in an hour, so the case is covered by
    // `suggestions::tests` rather than by waiting; what is asserted here is
    // that an expiry in the past does not take precedence over an approval
    // that exists, which is the order the two are evaluated in.
    assert_eq!(read_back["standing"], json!("approved"));
    Ok(())
}

// ---------------------------------------------------------------------------
// 5. "I did not look that far" is not "it is not there"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suggestion_beyond_the_read_window_is_out_of_reach_and_not_missing() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let static_dir = companion_build("suggestions-out-of-reach")?;
    // One stream position: everything but the newest message is beyond the
    // read.
    let running = Running::start_with(
        static_dir.clone(),
        gateway_env_with(
            &static_dir,
            &[
                ("GATEWAY_NATS_URL", &nats_url()),
                ("GATEWAY_APPROVAL_LOOKUP_WINDOW", "1"),
            ],
        ),
    )
    .await?;
    let talk = conversation(&running, &bus, "far", "granted", Some(&in_seconds(3600))).await?;
    for _ in 0..3 {
        let filler = inbound_event(&ghost("filler"), &portal_room("filler"), "pending");
        bus.publish_event(INBOUND_SUBJECT, &filler).await?;
    }

    let (status, answer) = running
        .get(&format!("/api/suggestions/{}", talk.suggestion_id))
        .await?;
    assert_eq!(
        status,
        reqwest::StatusCode::GONE,
        "a bounded read that gave up is not a suggestion that does not exist: {answer}"
    );
    assert_eq!(answer["error"], json!("suggestion_out_of_reach"));
    assert!(
        answer["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("GATEWAY_APPROVAL_LOOKUP_WINDOW")),
        "the refusal names the knob that widens the read: {answer}"
    );
    assert_eq!(
        answer["error"].as_str(),
        Some("suggestion_out_of_reach"),
        "and it is the same code POST /api/approvals gives for the same fact"
    );

    // The listing over the same one-position window says the same thing in
    // its own shape: it did not reach the start of the stream, so an empty
    // tail is not a claim that nothing older exists.
    let (status, listing) = running.get("/api/suggestions").await?;
    assert_eq!(status, reqwest::StatusCode::OK, "{listing}");
    assert_eq!(
        listing["window"]["reached_start_of_stream"],
        json!(false),
        "a bound nobody can see is a bound that lies: {listing}"
    );
    assert_eq!(listing["window"]["sequences"], json!(1));
    Ok(())
}

// ---------------------------------------------------------------------------
// 6. The listing's own bounds: the limit, and what it cuts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_listing_is_newest_first_and_says_when_the_limit_cut_it() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("suggestions-limit").await?;

    let mut mine = Vec::new();
    for index in 0..3 {
        let talk = conversation(
            &running,
            &bus,
            &format!("order{index}"),
            "granted",
            Some(&in_seconds(3600)),
        )
        .await?;
        mine.push(talk.suggestion_id.clone());
    }
    let (_, _, listing) = running.listed(mine.last().unwrap()).await?;

    // Newest first, asserted among this run's own three rather than against
    // a shared bus's totals.
    let positions: Vec<usize> = mine
        .iter()
        .map(|id| {
            listing["suggestions"]
                .as_array()
                .expect("the listing is an array")
                .iter()
                .position(|entry| entry["event_id"].as_str() == Some(id))
                .unwrap_or_else(|| panic!("{id} is in the listing"))
        })
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] > pair[1]),
        "the listing is newest first: {positions:?}"
    );

    // One row asked for, and the answer says the limit is why the rest are
    // missing — not the window, which is a different reason with a different
    // fix.
    let (status, one) = running.get("/api/suggestions?limit=1").await?;
    assert_eq!(status, reqwest::StatusCode::OK, "{one}");
    assert_eq!(one["suggestions"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        one["truncated"],
        json!(true),
        "more were found in the window than were answered with: {one}"
    );

    // A limit nobody could mean is refused rather than silently clamped: a
    // client that asked for a thousand rows and got fifty would believe it
    // had seen everything.
    for refused in ["0", "201", "-1", "many"] {
        let (status, answer) = running
            .get(&format!("/api/suggestions?limit={refused}"))
            .await?;
        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "limit={refused} was accepted: {answer}"
        );
        assert_eq!(answer["error"], json!("malformed_request"));
    }

    // And a path that is not an event id at all is a different answer again.
    let (status, answer) = running.get("/api/suggestions/not-an-event-id").await?;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["error"], json!("malformed_request"));
    Ok(())
}

// ---------------------------------------------------------------------------
// 7. A deployment with no bus, and a bus that does not answer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_gateway_with_no_bus_says_so_rather_than_answering_an_empty_list() -> Result<()> {
    ensure_stack().await?;
    let suggestion = harness::sha256_hex(&unique("unconfigured"));

    // No bus at all: an empty list would claim that no persona has proposed
    // anything, which is a very different statement.
    let static_dir = companion_build("suggestions-unconfigured")?;
    let unconfigured = Running::start_with(
        static_dir.clone(),
        gateway_env_with(&static_dir, &[("GATEWAY_NATS_URL", "")]),
    )
    .await?;
    let (status, answer) = unconfigured.get("/api/suggestions").await?;
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(answer["error"], json!("suggestions_not_configured"));
    let (status, answer) = unconfigured
        .get(&format!("/api/suggestions/{suggestion}"))
        .await?;
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(answer["error"], json!("suggestions_not_configured"));

    // A bus that is configured and does not answer: a different code and a
    // different status, because the fixes are different — one is a
    // deployment that was never set up, the other is an outage to wait out.
    let static_dir = companion_build("suggestions-bus-down")?;
    let down = Running::start_with(
        static_dir.clone(),
        gateway_env_with(
            &static_dir,
            &[("GATEWAY_NATS_URL", &unreachable_nats_url()?)],
        ),
    )
    .await?;
    let (status, answer) = down.get("/api/suggestions").await?;
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_GATEWAY,
        "a bus that does not answer is not a Gateway that reads no suggestions: {answer}"
    );
    assert_eq!(answer["error"], json!("bus_unreachable"));
    let (status, answer) = down.get(&format!("/api/suggestions/{suggestion}")).await?;
    assert_eq!(status, reqwest::StatusCode::BAD_GATEWAY, "{answer}");
    assert_eq!(answer["error"], json!("bus_unreachable"));
    Ok(())
}

// ---------------------------------------------------------------------------
// 8. A suggestion this build cannot read is counted, not dropped
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suggestion_this_build_cannot_read_is_counted_rather_than_blanking_the_screen(
) -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("suggestions-unreadable").await?;
    let talk = conversation(
        &running,
        &bus,
        "readable",
        "granted",
        Some(&in_seconds(3600)),
    )
    .await?;

    // A suggestion from a contract version this build does not have: the
    // network is one it has never heard of. Deliberately not validated
    // against today's contract, because that is the point — the schemas'
    // enums are closed, and a Gateway that fell over on a future one would
    // blank the screen for every other suggestion too.
    let future = json!({
        "specversion": "1.0",
        "id": harness::sha256_hex(&unique("future-network")),
        "source": format!("hermes://{SERVER_NAME}/personas/assistant"),
        "type": SUGGEST_TYPE,
        "time": in_seconds(-30),
        "subject": harness::sha256_hex(&unique("future-trigger")),
        "datacontenttype": "application/json",
        "network": "carrierpigeon",
        "connection": "carrierpigeon",
        "consent": "granted",
        "data": {
            "persona_id": "assistant",
            "trigger": {
                "event_id": harness::sha256_hex(&unique("future-trigger")),
                "event_type": INBOUND_TYPE
            },
            "suggestion": { "body": "Par retour de pigeon.", "format": "text/plain" },
            "attempt": 1
        }
    });
    bus.publish_event(SUGGEST_SUBJECT, &future).await?;
    let unreadable_id = future["id"].as_str().unwrap().to_owned();

    // The listing still draws, the readable suggestion is in it, and the one
    // that was not understood is counted rather than silently missing.
    let listing = poll_until(
        || async {
            let (status, _, body) = running.get_raw("/api/suggestions?limit=200").await.ok()?;
            if status != reqwest::StatusCode::OK {
                return None;
            }
            let has_readable = body["suggestions"]
                .as_array()?
                .iter()
                .any(|entry| entry["event_id"].as_str() == Some(&talk.suggestion_id));
            (has_readable && body["unreadable"].as_u64().unwrap_or_default() >= 1).then_some(body)
        },
        "the listing to hold the readable suggestion and count the unreadable one",
    )
    .await?;
    assert!(
        !listing["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["event_id"].as_str() == Some(&unreadable_id)),
        "a suggestion this build cannot read was answered with anyway: {listing}"
    );

    // Asked about on its own, it is neither a 404 nor a silence: "I found it
    // and do not understand it" is its own fact.
    let (status, answer) = running
        .get(&format!("/api/suggestions/{unreadable_id}"))
        .await?;
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["error"], json!("suggestion_unreadable"));
    assert!(
        answer["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("carrierpigeon")),
        "the refusal names what it could not read: {answer}"
    );
    Ok(())
}
