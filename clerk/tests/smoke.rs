//! Smoke: the clerk's test stack is real and the clerk comes up against it
//! (ticket #265).
//!
//! Two halves, asserted separately so that a relay that will not seed and
//! a clerk that will not start are two failures rather than one. The first
//! is the seam's other side: a real Buzz relay, with restricted writes,
//! accepting the owner's signed commands — a member added, three channels
//! created with that member in them. The second is the clerk binary at its
//! process boundary: given that relay, that key and those channels, it
//! reports itself running and serves `/health` and `/metrics` with every
//! counter at zero.
//!
//! Requires Docker: the relay stack comes from `tests/compose.relay.yaml`
//! (project `TWALK_CLERK_TEST_STACK`, port `TWALK_CLERK_TEST_RELAY_PORT`)
//! and the bus from the shared test stack. Both persist across runs; the
//! relay's goes with
//! `docker compose -p twalk-clerk-test -f clerk/tests/compose.relay.yaml down -v`.

mod harness;

use anyhow::Result;
use harness::{run_id, seed, RelayStack, Run, TEST_OWNER_PUBKEY_HEX};

#[tokio::test]
async fn the_relay_is_real_and_accepts_the_owners_seed() -> Result<()> {
    let stack = RelayStack::ensure().await?;

    // The owner the harness signs as is the owner the compose file booted
    // the relay with — the seed below would be refused otherwise, but this
    // says which of the two constants moved.
    assert_eq!(stack.owner.public_key().to_hex(), TEST_OWNER_PUBKEY_HEX);

    // Restricted writes: a key that is not a member writes nothing, which
    // is the property the whole suite rests on — a clerk that could post
    // without being added would prove nothing about its key.
    let nip11 = stack.nip11().await?;
    assert_eq!(
        nip11["limitation"]["restricted_writes"].as_bool(),
        Some(true),
        "the relay's NIP-11 document: {nip11}"
    );

    let run = run_id("smoke-relay");
    let dir = std::env::temp_dir().join(&run);
    tokio::fs::create_dir_all(&dir).await?;
    let (_, _, channels) = seed(&stack, &run, &dir).await?;

    // A fresh channel is empty, and the owner can read it: what every later
    // absence search depends on.
    for channel in [&channels.approvals, &channels.activity, &channels.journal] {
        assert!(
            uuid::Uuid::parse_str(channel).is_ok(),
            "a channel id is a UUID: {channel}"
        );
        let events = stack.all_events_in(channel).await?;
        assert!(
            events.is_empty(),
            "a fresh channel holds nothing: {events:?}"
        );
    }

    tokio::fs::remove_dir_all(&dir).await?;
    Ok(())
}

#[tokio::test]
async fn the_clerk_comes_up_against_it() -> Result<()> {
    // `Run::start` is the whole setup every other suite begins with, and
    // it returns only once the clerk has said `clerk running`.
    let run = Run::start("smoke-clerk").await?;

    assert_eq!(run.clerk.health().await?, reqwest::StatusCode::OK);
    let metrics = run.clerk.metrics().await?;
    assert!(
        metrics.contains("twalk_clerk_posts_total{channel=\"approbations\"} 0\n"),
        "a clerk that has posted nothing says so:\n{metrics}"
    );

    // `shutdown` is where the clerk's exit on SIGTERM is asserted: a
    // status that is not success fails it.
    run.shutdown().await?;
    Ok(())
}
