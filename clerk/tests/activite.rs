//! `activite`: the operational feed, and what it must not say (ticket
//! #265).
//!
//! The clerk writes one line per bridge transition and per consent
//! decision, and nothing for a persona's thinking. What makes the feed
//! safe to read over anyone's shoulder is the rule the Companion's
//! dashboard already keeps (`companion/src/lib/dashboard/model.ts`): a
//! consent line says that *a contact* was granted on WhatsApp, and never
//! which one — the subject's Matrix ID is a sender identity, and the line
//! is searched for the contract's own pattern of one.
//!
//! The absence is proven by ordering, not by a timer: after the thinking
//! event, a second bridge transition is published and its line waited
//! for, and the feed is then asserted to hold exactly the lines the two
//! transitions and the one decision account for.

mod harness;

use anyhow::Result;
use harness::{
    bridge_status, consent_change_about_a_contact, thinking, Run, CONTACT_MATRIX_ID,
    MATRIX_USER_ID_SUBJECT_PATTERN,
};
use serde_json::json;

/// Whether one whitespace-delimited word is shaped like a Matrix user ID —
/// the contract's own `MATRIX_USER_ID_SUBJECT_PATTERN`,
/// `^@[a-zA-Z0-9._=/+-]+:[^/]+$`, read by hand rather than through a
/// regex crate this package does not otherwise need.
fn looks_like_a_matrix_user_id(word: &str) -> bool {
    let Some(rest) = word.strip_prefix('@') else {
        return false;
    };
    let Some((localpart, server)) = rest.split_once(':') else {
        return false;
    };
    !localpart.is_empty()
        && localpart
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._=/+-".contains(&b))
        && !server.is_empty()
        && !server.contains('/')
}

#[test]
fn the_hand_written_matcher_agrees_with_the_contracts_pattern() {
    // Pinned to the pattern it transcribes: a change there fails here.
    assert_eq!(
        MATRIX_USER_ID_SUBJECT_PATTERN,
        "^@[a-zA-Z0-9._=/+-]+:[^/]+$"
    );
    assert!(looks_like_a_matrix_user_id(CONTACT_MATRIX_ID));
    assert!(looks_like_a_matrix_user_id("@owner:test.twalk"));
    assert!(!looks_like_a_matrix_user_id("un"));
    assert!(!looks_like_a_matrix_user_id("contact"));
    assert!(!looks_like_a_matrix_user_id("@"));
    assert!(!looks_like_a_matrix_user_id("@nocolon"));
    assert!(!looks_like_a_matrix_user_id("@a:b/c"));
}

#[tokio::test]
async fn bridge_transitions_and_consent_decisions_are_lines_and_thinking_is_not() -> Result<()> {
    let run = Run::start("activite").await?;

    // A bridge transition: the bridge's id and its new state.
    let status = bridge_status(&run.id, 1)?;
    let bridge_id = status["data"]["bridge_id"].as_str().unwrap().to_owned();
    assert_eq!(status["data"]["to_state"], "connected");
    run.publish("bridge.status.changed", &status).await?;
    let line = run
        .wait_for_line(&run.channels.activity, &bridge_id)
        .await?;
    assert!(
        line.content.contains("connecté"),
        "names the state the bridge reached: {}",
        line.content
    );
    assert_eq!(line.pubkey.to_hex(), run.clerk_pubkey);

    // A consent decision about a contact: the state and the networks, and
    // no Matrix user ID anywhere in the line.
    let decision = consent_change_about_a_contact(&run.id, 2)?;
    assert_eq!(decision["data"]["subject"]["id"], CONTACT_MATRIX_ID);
    assert_eq!(decision["data"]["new_state"], "granted");
    run.publish("consent.state.changed", &decision).await?;
    let line = run
        .wait_for_line(&run.channels.activity, "Consentement")
        .await?;
    assert!(
        line.content.contains("accordé") && line.content.contains("WhatsApp"),
        "names the new state and the network: {}",
        line.content
    );
    assert!(
        line.content.contains("un contact"),
        "says what kind of subject it was about: {}",
        line.content
    );
    assert!(
        !line.content.contains(CONTACT_MATRIX_ID),
        "the contact's Matrix ID is not in the feed: {}",
        line.content
    );
    assert!(
        !line
            .content
            .split_whitespace()
            .any(looks_like_a_matrix_user_id),
        "nothing shaped like a Matrix user ID is in the feed: {}",
        line.content
    );

    // A persona's thinking: nothing. A second bridge transition after it,
    // whose line proves the feed is still being written, and then the
    // count: two transitions and one decision are three lines.
    run.publish("persona.thinking.emitted", &thinking(&run.id, 3)?)
        .await?;
    let mut later = bridge_status(&run.id, 4)?;
    later["data"]["from_state"] = json!("connected");
    later["data"]["to_state"] = json!("disconnected");
    run.publish("bridge.status.changed", &later).await?;
    run.wait_for_line(&run.channels.activity, "déconnecté")
        .await?;

    let lines = run.lines_in(&run.channels.activity).await?;
    assert_eq!(
        lines.len(),
        3,
        "two bridge transitions and one consent decision, and nothing for the thinking: {:#?}",
        lines.iter().map(|line| &line.content).collect::<Vec<_>>()
    );
    run.assert_metric("twalk_clerk_posts_total{channel=\"activite\"} 3")
        .await?;
    run.assert_metric_now("twalk_clerk_skipped_total{why=\"unreadable\"} 0")
        .await?;

    run.shutdown().await
}
