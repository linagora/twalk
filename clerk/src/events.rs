//! Typed views of the four contract event types the clerk reads, each
//! deserialised with **no member for anything the clerk must not hold**.
//!
//! The controller's ruling for this component (ADR 0035) is that a post to
//! Buzz is `suggestion.body` plus the clerk's own sentences, never a
//! contact's own words beyond that one field, and never the room a
//! conversation lives in. So [`Suggestion`] has no `trigger`, no
//! `rationale`, no `attempt`, no `confidence` — a persona's private working,
//! not the clerk's business — and [`PostedReport`] holds nothing of the
//! reply it reports on beyond what a sentence in `journal` needs: which
//! network, whether the reply reached the contact, and which Matrix ID
//! posted it. `persona_id` is fine to keep: a persona is not a contact.
//!
//! Each struct also derives `Serialize`, not because the clerk publishes
//! any of these shapes back onto the bus, but because that is how
//! `the_view_of_a_suggestion_has_no_member_for_the_contact` proves the
//! absence — re-serialising [`Suggestion`] and asserting its exact key set
//! is a stronger claim than reading the struct definition, because it is a
//! claim the compiler checks on every run rather than one a reviewer has to
//! keep re-checking by eye.

use serde::{Deserialize, Serialize};

/// `fr.linagora.twalk.persona.suggest.produced.v1`.
pub const SUGGEST_PRODUCED: &str = "fr.linagora.twalk.persona.suggest.produced.v1";
/// `fr.linagora.twalk.persona.reply.approved.v1`. Its `.posted` sibling —
/// the report a successful post republishes this event under, with the
/// `reach` and `posted-as` headers `posted_report` reads — is this subject,
/// mapped into the deployment's bus namespace by `Config::bus_subject`,
/// with `.posted` appended.
pub const REPLY_APPROVED: &str = "fr.linagora.twalk.persona.reply.approved.v1";
/// `fr.linagora.twalk.bridge.status.changed.v1`.
pub const BRIDGE_STATUS: &str = "fr.linagora.twalk.bridge.status.changed.v1";
/// `fr.linagora.twalk.consent.state.changed.v1`.
pub const CONSENT_CHANGED: &str = "fr.linagora.twalk.consent.state.changed.v1";

/// A `persona.suggest.produced` event, as much of it as `approbations`
/// needs: which suggestion (`id`), when and on which network, which persona
/// proposed it, its body and when it expires.
///
/// `id`, `time` and `network` are the envelope's own CloudEvents attributes
/// (`id` is the event's own id, not a contact's; `network` is the top-level
/// extension, not `data.network`, which this event does not have).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Suggestion {
    pub id: String,
    pub time: String,
    pub network: String,
    pub data: SuggestionData,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SuggestionData {
    pub persona_id: String,
    pub suggestion: Content,
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// A suggestion's own words. `body` is the one field of a contact's
/// exchange the clerk is allowed to quote — it is the persona's proposed
/// reply, not the contact's message — and `format` is deliberately absent:
/// the clerk posts it as plain text regardless.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Content {
    pub body: String,
}

/// What `journal` needs about one posted reply: which network, what it
/// reached, who posted it and whether the owner edited it — never the
/// reply's own words, which stay on the bus event this report is built
/// from and never enter this struct.
///
/// Built by [`posted_report`] from the `.posted` report's JSON payload (the
/// approved reply's own `id`, `time` and `network`, and `data.edited`) and
/// its two headers (`reach`, `posted-as`), rather than derived, so a report
/// the clerk cannot fully read is a report it does not act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostedReport {
    pub approval_id: String,
    pub network: String,
    pub reach: String,
    pub posted_as: String,
    pub time: String,
    /// The approval's own `data.edited` (#284): whether what went out was
    /// the owner's text rather than the persona's. The contract makes the
    /// field optional, and a report that omits it went out as proposed.
    pub edited: bool,
}

/// Reads a `.posted` report: the `persona.reply.approved` event republished
/// unchanged, carrying the `reach` (`contact`|`nobody`) and `posted-as`
/// headers the Sensor's owner device adds on every successful post (issue
/// #123, issue #216).
///
/// Returns `None` when either header is missing or the payload is not the
/// approval shape — this event's own `type`, plus `id`, `time` and
/// `network` — because a report the clerk cannot fully read must not be
/// acted on as though it said something it didn't.
pub fn posted_report(
    payload: &[u8],
    headers: Option<&async_nats::HeaderMap>,
) -> Option<PostedReport> {
    let headers = headers?;
    let reach = headers.get("reach")?.as_str().to_owned();
    let posted_as = headers.get("posted-as")?.as_str().to_owned();

    let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    if value.get("type").and_then(|v| v.as_str()) != Some(REPLY_APPROVED) {
        return None;
    }
    let approval_id = value.get("id")?.as_str()?.to_owned();
    let time = value.get("time")?.as_str()?.to_owned();
    let network = value.get("network")?.as_str()?.to_owned();
    let edited = value
        .get("data")
        .and_then(|data| data.get("edited"))
        .and_then(|edited| edited.as_bool())
        .unwrap_or(false);

    Some(PostedReport {
        approval_id,
        network,
        reach,
        posted_as,
        time,
        edited,
    })
}

/// A `bridge.status.changed` event, as much of it as `activite` needs: which
/// bridge and its new state, and the event's own `id`, which the line it
/// becomes is tagged with so a redelivery finds it (`relay::stream_message`).
/// No room, no operator identity — a bridge transition is operational and
/// names nobody.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BridgeStatus {
    pub id: String,
    pub data: BridgeStatusData,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BridgeStatusData {
    pub bridge_id: String,
    /// The schema's own `to_state` — the state after the transition, which
    /// is the one fact a sentence in `activite` needs; `from_state` is not
    /// kept.
    #[serde(rename = "to_state")]
    pub state: String,
}

/// A `consent.state.changed` event, as much of it as `activite` needs:
/// what kind of subject the decision is about, what it became, which
/// networks it covers, and the event's own `id` for the line's tag.
/// `data.subject.id` is deliberately not kept — naming the contact in
/// `activite` is the leak this component exists not to reproduce (ADR
/// 0012's own concern, applied here) — only `subject.type`, which lets a
/// sentence say "a contact" or "a persona" without saying which one. The
/// envelope's `subject` (the same Matrix ID) has no member either.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConsentChange {
    pub id: String,
    pub data: ConsentData,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConsentData {
    pub subject: Subject,
    pub new_state: String,
    #[serde(default)]
    pub scope: Option<Scope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Subject {
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Scope {
    #[serde(default)]
    pub networks: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../contracts/cloudevents/v1/fixtures/"
        );
        std::fs::read(format!("{path}{name}")).expect("fixture file exists")
    }

    #[test]
    fn a_suggestion_is_read_from_the_contract_fixture() {
        let bytes = fixture("persona.suggest.produced.json");
        let suggestion: Suggestion = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            suggestion.id,
            "319be8ff15d5dee005c8aa27119b983da8223959987e5dbc639d81e370b5ef9b"
        );
        assert_eq!(suggestion.time, "2026-09-17T10:00:09Z");
        assert_eq!(suggestion.network, "whatsapp");
        assert_eq!(suggestion.data.persona_id, "assistant");
        assert_eq!(suggestion.data.suggestion.body, "Pas de problème, à 20h !");
        assert_eq!(
            suggestion.data.expires_at.as_deref(),
            Some("2026-09-17T11:00:00Z")
        );
    }

    #[test]
    fn the_view_of_a_suggestion_has_no_member_for_the_contact() {
        let bytes = fixture("persona.suggest.produced.json");
        let suggestion: Suggestion = serde_json::from_slice(&bytes).unwrap();
        let value = serde_json::to_value(&suggestion).unwrap();

        let top: std::collections::BTreeSet<_> =
            value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            top,
            ["id", "time", "network", "data"]
                .into_iter()
                .map(String::from)
                .collect()
        );

        let data: std::collections::BTreeSet<_> =
            value["data"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            data,
            ["persona_id", "suggestion", "expires_at"]
                .into_iter()
                .map(String::from)
                .collect()
        );

        let inner: std::collections::BTreeSet<_> = value["data"]["suggestion"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(inner, ["body"].into_iter().map(String::from).collect());
    }

    #[test]
    fn a_bridge_status_is_read_from_the_contract_fixture() {
        let bytes = fixture("bridge.status.changed.json");
        let status: BridgeStatus = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            status.id,
            "029285728dfbbe8e00996a1e7a4c9baceb984a94e8360eb4b593ff9a70d661d9"
        );
        assert_eq!(status.data.bridge_id, "bridge-gmessages-1");
        assert_eq!(status.data.state, "connected");
    }

    #[test]
    fn a_consent_change_is_read_from_the_contract_fixture() {
        let bytes = fixture("consent.state.changed.json");
        let change: ConsentChange = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            change.id,
            "3255d503fa47d8d6f7f75194d2761fb149268fda34343c2a0890d8e9a36a6034"
        );
        assert_eq!(change.data.subject.kind, "persona");
        assert_eq!(change.data.new_state, "granted");
        assert_eq!(
            change.data.scope.unwrap().networks,
            vec!["whatsapp".to_string()]
        );
    }

    #[test]
    fn a_posted_report_takes_reach_and_posted_as_from_headers() {
        let bytes = fixture("persona.reply.approved.json");
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("reach", "contact");
        headers.insert("posted-as", "@sensor:example.com");

        let report = posted_report(&bytes, Some(&headers)).expect("a full report");
        assert_eq!(
            report.approval_id,
            "57f0e4d352d1ba5e6bf0e92223634253cd852e3d7d018ea91025dc098c1a564a"
        );
        assert_eq!(report.time, "2026-09-17T10:04:37Z");
        assert_eq!(report.network, "whatsapp");
        assert_eq!(report.reach, "contact");
        assert_eq!(report.posted_as, "@sensor:example.com");
    }

    #[test]
    fn posted_report_reads_edited() {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("reach", "contact");
        headers.insert("posted-as", "@sensor:example.com");

        // The fixture's approval was edited.
        let bytes = fixture("persona.reply.approved.json");
        let report = posted_report(&bytes, Some(&headers)).expect("a full report");
        assert!(report.edited);

        // The same event with `edited: false`, and with the optional field
        // left out, which the contract allows and which means "as proposed".
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["data"]["edited"] = serde_json::Value::Bool(false);
        let report = posted_report(&serde_json::to_vec(&value).unwrap(), Some(&headers))
            .expect("a full report");
        assert!(!report.edited);
        value["data"].as_object_mut().unwrap().remove("edited");
        let report = posted_report(&serde_json::to_vec(&value).unwrap(), Some(&headers))
            .expect("a full report");
        assert!(!report.edited);
    }

    #[test]
    fn a_posted_report_is_absent_without_both_headers() {
        let bytes = fixture("persona.reply.approved.json");

        assert!(posted_report(&bytes, None).is_none());

        let mut only_reach = async_nats::HeaderMap::new();
        only_reach.insert("reach", "contact");
        assert!(posted_report(&bytes, Some(&only_reach)).is_none());

        let mut only_posted_as = async_nats::HeaderMap::new();
        only_posted_as.insert("posted-as", "@sensor:example.com");
        assert!(posted_report(&bytes, Some(&only_posted_as)).is_none());
    }

    #[test]
    fn a_posted_report_is_absent_when_the_payload_is_not_the_approval_shape() {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("reach", "contact");
        headers.insert("posted-as", "@sensor:example.com");

        let not_json = b"not json at all";
        assert!(posted_report(not_json, Some(&headers)).is_none());

        let wrong_type = fixture("persona.suggest.produced.json");
        assert!(posted_report(&wrong_type, Some(&headers)).is_none());
    }
}
