//! The sweep: an expired suggestion's post goes, and the feed says so
//! (ticket #265, #219, ADR 0035).
//!
//! The clerk holds no timer per post and no store: every
//! `CLERK_SWEEP_SECONDS` it reads its own posts in `approbations` back off
//! the relay, and the reference line of each one — the only memory it has
//! — says whether its suggestion has expired. The suite gives a suggestion
//! [`LIFE_SECONDS`] to live, sees its post, and then sees the relay stop
//! answering with it, the deletion counted on `/metrics` and the one
//! line in `activite` that tells the owner a proposal went undecided.
//!
//! The one timer in this suite is derived, not chosen (#148): the life is
//! long enough that the bus, the clerk's own-posts query and the post
//! itself fit inside it on a loaded host — a suggestion that expired
//! before the clerk posted it would be correctly skipped, and the test
//! would then fail as though the sweep were broken — and the bound on the
//! deletion is the life plus two sweeps plus a margin ([`GONE_WITHIN`]).
//! That a post is deleted **once** is proven by ordering, not by a timer:
//! a second short-lived suggestion is posted and its own deletion waited
//! for — sweeps have run since the first deletion — and the counter and
//! the feed then account for exactly two.

mod harness;

use std::time::Duration;

use anyhow::Result;
use harness::{suggestion, Run, SWEEP_SECONDS};

/// How long each suggestion lives: ten seconds, so that the clerk's post
/// — the bus delivery, its own-posts query and the post itself — lands
/// well inside the life on a host this project has seen at load 25–30
/// while linking Rust, and the skip of an already-expired suggestion,
/// which is correct behaviour, cannot masquerade as a sweep that does not
/// delete. Three seconds, the previous value, was one such failure away.
const LIFE_SECONDS: i64 = 10;

/// A margin for the relay's own round trips: the query that finds the
/// expired post, the delete, and the poll that observes it gone.
const MARGIN_SECONDS: u64 = 4;

/// How long after its post an expired suggestion's post may still stand:
/// `life + 2 × sweep + margin`. The life, because the post cannot go
/// before its suggestion expires; two sweeps, because the sweep that
/// starts a moment before the expiry leaves it standing and the next one
/// is a whole interval away; and the margin above.
const GONE_WITHIN: Duration =
    Duration::from_secs(LIFE_SECONDS as u64 + 2 * SWEEP_SECONDS + MARGIN_SECONDS);

/// The fragment of the feed's expiry line
/// (`Une proposition a expiré sans décision · retirée d’approbations`).
const EXPIRED: &str = "a expiré";

#[tokio::test]
async fn an_expired_suggestions_post_is_deleted_and_the_feed_says_so() -> Result<()> {
    let run = Run::start("sweep").await?;

    let short_lived = suggestion(&run.id, 1, LIFE_SECONDS)?;
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
    let another = suggestion(&run.id, 2, LIFE_SECONDS)?;
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
