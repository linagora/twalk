//! The sweep: an expired suggestion's post goes, and the feed says so
//! (ticket #265, #219, ADR 0035).
//!
//! The clerk holds no timer per post and no store: every
//! `CLERK_SWEEP_SECONDS` it reads its own posts in `approbations` back off
//! the relay, and the reference line of each one — the only memory it has
//! — says whether its suggestion has expired. The suite gives a suggestion
//! three seconds to live, sees its post, and then sees the relay stop
//! answering with it, the deletion counted on `/metrics` and the one
//! line in `activite` that tells the owner a proposal went undecided.
//!
//! The bound is the brief's: with a two-second sweep, eight seconds after
//! the post is there is four sweeps late, and a post still standing then
//! is a sweep that does not delete. That a post is deleted **once** is
//! proven by ordering, not by a timer: a second short-lived suggestion is
//! posted and its own deletion waited for — sweeps have run since the
//! first deletion — and the counter and the feed then account for exactly
//! two.

mod harness;

use std::time::Duration;

use anyhow::Result;
use harness::{suggestion, Run};

/// How long after its post an expired suggestion's post may still stand:
/// three seconds of life plus two sweeps, with room.
const GONE_WITHIN: Duration = Duration::from_secs(8);

/// The fragment of the feed's expiry line
/// (`Une proposition a expiré sans décision · retirée d’approbations`).
const EXPIRED: &str = "a expiré";

#[tokio::test]
async fn an_expired_suggestions_post_is_deleted_and_the_feed_says_so() -> Result<()> {
    let run = Run::start("sweep").await?;

    let short_lived = suggestion(&run.id, 1, 3)?;
    let id = short_lived["id"].as_str().unwrap().to_owned();
    run.publish("persona.suggest.produced", &short_lived)
        .await?;
    run.wait_for_post(&id).await?;
    run.assert_metric("twalk_clerk_posts_total{channel=\"approbations\"} 1")
        .await?;

    // Gone: the relay no longer answers a query for the clerk's forum
    // posts with it — which is what the owner's client reads.
    run.wait_until_gone(&id, GONE_WITHIN).await?;
    run.assert_metric("twalk_clerk_deleted_total{why=\"expired\"} 1")
        .await?;
    let line = run.wait_for_line(&run.channels.activity, EXPIRED).await?;
    assert!(
        line.content.contains("approbations"),
        "the feed says where the post was removed from: {}",
        line.content
    );

    // Deleted once: a second short-lived suggestion, posted and swept in
    // its turn, is the proof that sweeps have run since the first
    // deletion — and the count is then two, not three.
    let another = suggestion(&run.id, 2, 3)?;
    let another_id = another["id"].as_str().unwrap().to_owned();
    run.publish("persona.suggest.produced", &another).await?;
    run.wait_for_post(&another_id).await?;
    run.wait_until_gone(&another_id, GONE_WITHIN).await?;
    run.assert_metric("twalk_clerk_deleted_total{why=\"expired\"} 2")
        .await?;
    let expiry_lines = run
        .wait_for_lines(&run.channels.activity, EXPIRED, 2)
        .await?;
    assert_eq!(
        expiry_lines.len(),
        2,
        "two expiries, two lines: {expiry_lines:?}"
    );

    run.shutdown().await
}
