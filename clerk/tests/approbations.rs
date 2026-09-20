//! `approbations`: one suggestion is one post, and nothing of the contact
//! reaches the relay (ticket #265, ADR 0035).
//!
//! Three tests at the clerk's process boundary — the bus on one side, a
//! real Buzz relay read as its owner on the other, the clerk's own
//! `/metrics` beside them — and each one is a promise the clerk makes
//! that no unit test of its modules can keep.
//!
//! **One post and it stays one.** A suggestion redelivered by the bus, and
//! a clerk restarted on the same stream, must not produce a second post:
//! the clerk holds no store and recognises its own posts by their
//! reference line. "No second post" is proven by **ordering**, never by a
//! timer: a later suggestion is published and its post waited for, which
//! shows the consumer went past the redelivery; only then is the first
//! suggestion's count read.
//!
//! **Nothing of the contact.** The post carries the suggestion's body and
//! the clerk's own lines; what it must not carry is anything from the
//! message the suggestion answers — the contact's Matrix ID, display name,
//! network identifier, their message, a quoted excerpt, the room — nor the
//! persona's rationale, which quotes that message in its own words. The
//! test publishes the inbound message on the run's own stream with every
//! one of those as a marker, waits for the post, then searches **every
//! byte** the clerk could have written on all three channels: every
//! event's content and every tag value.
//!
//! **Expired is skipped, and counted.** A suggestion already past its
//! `expires_at` is not posted — and because a silence is the failure this
//! project ships most, it is a counted skip on `/metrics`.

mod harness;

use anyhow::Result;
use harness::{inbound_message, suggestion, Run, CONTACT_MATRIX_ID};
use serde_json::json;

#[tokio::test]
async fn a_suggestion_becomes_one_post_and_stays_one() -> Result<()> {
    let mut run = Run::start("approbations-one").await?;

    // One suggestion, an hour to live: one post, whose content is the
    // body between the clerk's own lines and the reference line last.
    let first = suggestion(&run.id, 1, 3600)?;
    let first_id = first["id"].as_str().unwrap().to_owned();
    let body = first["data"]["suggestion"]["body"].as_str().unwrap();
    let expires_at = first["data"]["expires_at"].as_str().unwrap();
    run.publish("persona.suggest.produced", &first).await?;

    let post = run.wait_for_post(&first_id).await?;
    assert!(
        post.content.contains(body),
        "the post carries the suggestion's body verbatim:\n{}",
        post.content
    );
    let reference = format!("twalk:suggestion:{first_id} expires {expires_at}");
    assert_eq!(
        post.content.lines().last(),
        Some(reference.as_str()),
        "the reference line is the post's last line:\n{}",
        post.content
    );
    assert_eq!(
        post.pubkey.to_hex(),
        run.clerk_pubkey,
        "signed by the clerk's own key"
    );
    run.assert_metric("twalk_clerk_posts_total{channel=\"approbations\"} 1")
        .await?;

    // The same event again — the bus deduplicates on `Nats-Msg-Id` for
    // two minutes, so this is the same CloudEvent id under a new message
    // id, which is what a redelivery is to the clerk. Then a second
    // suggestion: its post proves the consumer went past the redelivery.
    run.publish_again("persona.suggest.produced", &first)
        .await?;
    let second = suggestion(&run.id, 2, 3600)?;
    run.publish("persona.suggest.produced", &second).await?;
    run.wait_for_post(second["id"].as_str().unwrap()).await?;

    let posts = run.posts_about(&first_id).await?;
    assert_eq!(
        posts.len(),
        1,
        "a redelivered suggestion is still one post: {posts:?}"
    );
    run.assert_metric("twalk_clerk_skipped_total{why=\"duplicate\"} 1")
        .await?;
    run.assert_metric("twalk_clerk_posts_total{channel=\"approbations\"} 2")
        .await?;

    // A warm restart on the same key, channels and stream: the durable
    // consumer resumes at its ack floor, so the first two suggestions are
    // not read again — a third one's post is the proof the new process is
    // consuming, and the duplicate counter of the new process staying at
    // zero is the proof it resumed rather than re-read the stream.
    run.restart_clerk().await?;
    let third = suggestion(&run.id, 3, 3600)?;
    run.publish("persona.suggest.produced", &third).await?;
    run.wait_for_post(third["id"].as_str().unwrap()).await?;

    let posts = run.posts_about(&first_id).await?;
    assert_eq!(
        posts.len(),
        1,
        "a restarted clerk does not post the first suggestion again: {posts:?}"
    );
    run.assert_metric_now("twalk_clerk_skipped_total{why=\"duplicate\"} 0")
        .await?;
    run.assert_metric("twalk_clerk_posts_total{channel=\"approbations\"} 1")
        .await?;

    run.shutdown().await
}

#[tokio::test]
async fn nothing_of_the_contact_reaches_the_relay() -> Result<()> {
    let run = Run::start("approbations-nothing").await?;

    // The message the suggestion answers, on this run's own stream, with
    // a marker in every field that is the contact's: their words, their
    // name, their number, the excerpt they quoted.
    let mut inbound = inbound_message(&run.id, 1)?;
    inbound["data"]["body"] = json!("MARKER-INBOUND");
    inbound["data"]["contact"]["display_name"] = json!("MARKER-NAME");
    inbound["data"]["contact"]["network_identifier"] = json!("MARKER-NUMBER");
    inbound["data"]["reply_to"] = json!({
        "matrix_event_id": "$MARKER-quoted-event",
        "excerpt": "MARKER-EXCERPT",
    });
    let contact = inbound["subject"].as_str().unwrap().to_owned();
    assert_eq!(contact, CONTACT_MATRIX_ID);
    let room = inbound["source"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned();
    assert!(
        room.starts_with('!'),
        "the source ends with the room id: {room}"
    );
    run.publish("inbound.message.received", &inbound).await?;

    // The suggestion that answers it: a marker in the body (which the post
    // must carry, once) and one in the rationale (which it must not).
    let body_marker = format!("MARKER-BODY-{}", run.id);
    let rationale_marker = format!("MARKER-RATIONALE-{}", run.id);
    let mut event = suggestion(&run.id, 2, 3600)?;
    event["data"]["trigger"]["event_id"] = inbound["id"].clone();
    event["data"]["suggestion"]["body"] = json!(format!("D'accord pour 20h ! {body_marker}"));
    event["data"]["rationale"] = json!(rationale_marker);
    let id = event["id"].as_str().unwrap().to_owned();
    run.publish("persona.suggest.produced", &event).await?;
    run.wait_for_post(&id).await?;
    // The activity line that follows the post is written after it; wait
    // for it too, so the search below reads everything the suggestion
    // caused and not only the first thing.
    run.wait_for_line(&run.channels.activity, "approbations")
        .await?;

    // Every byte the clerk could have written, on all three channels:
    // every event's content and every value of every tag.
    let mut written = String::new();
    for channel in [
        &run.channels.approvals,
        &run.channels.activity,
        &run.channels.journal,
    ] {
        for event in run.stack.all_events_in(channel).await? {
            written.push_str(&event.content);
            written.push('\n');
            for tag in event.tags.iter() {
                for value in tag.as_slice() {
                    written.push_str(value);
                    written.push('\n');
                }
            }
        }
    }

    assert_eq!(
        written.matches(&body_marker).count(),
        1,
        "the suggestion's body is on the relay exactly once, in the post:\n{written}"
    );
    for absent in [
        rationale_marker.as_str(),
        "MARKER-RATIONALE",
        "MARKER-INBOUND",
        "MARKER-NAME",
        "MARKER-NUMBER",
        "MARKER-EXCERPT",
        contact.as_str(),
        room.as_str(),
    ] {
        assert!(
            !written.contains(absent),
            "{absent:?} reached the relay; everything written was:\n{written}"
        );
    }

    run.shutdown().await
}

#[tokio::test]
async fn an_expired_suggestion_is_not_posted_and_is_counted() -> Result<()> {
    let run = Run::start("approbations-expired").await?;

    // Expired a second ago; then a live one, whose post proves the
    // consumer read past the expired one.
    let expired = suggestion(&run.id, 1, -1)?;
    let expired_id = expired["id"].as_str().unwrap().to_owned();
    run.publish("persona.suggest.produced", &expired).await?;
    let live = suggestion(&run.id, 2, 3600)?;
    run.publish("persona.suggest.produced", &live).await?;
    run.wait_for_post(live["id"].as_str().unwrap()).await?;

    let posts = run.posts_about(&expired_id).await?;
    assert!(
        posts.is_empty(),
        "an expired suggestion is not posted: {posts:?}"
    );
    run.assert_metric("twalk_clerk_skipped_total{why=\"expired\"} 1")
        .await?;
    run.assert_metric("twalk_clerk_posts_total{channel=\"approbations\"} 1")
        .await?;

    run.shutdown().await
}
