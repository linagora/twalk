//! Smoke: the clerk's test stack is real and the clerk comes up against it
//! (ticket #265).
//!
//! Three halves, asserted separately so that a relay that will not seed and
//! a clerk that will not start are two failures rather than one. The first
//! is the seam's other side: a real Buzz relay, with restricted writes,
//! accepting the owner's signed commands — a member added, three channels
//! created with that member in them. The second is the clerk binary at its
//! process boundary: given that relay, that key and those channels, it
//! reports itself running and serves `/health` and `/metrics` with every
//! counter at zero. The third (ticket #284) is the fact the write half of
//! the loop rests on: the relay accepts the owner's reaction and thread
//! reply on a forum post, indexes both by `#e`, and answers a query for
//! them made under the clerk's own key — and accepts the clerk's answer in
//! the post's thread.
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

/// The relay indexes a reaction (kind 7) and a thread reply (kind 45003)
/// by the post they name in an `e` tag, and a member that is not their
/// author reads both back with one `#e` filter: what `Relay::gestures_on`
/// rests on (ticket #284). The clerk's client is the one under test here
/// rather than the harness's twin, because the question is whether *this*
/// filter, sent by *this* key, gets the owner's gestures back — and then
/// whether the reply that key writes in the post's thread is taken, and
/// found again as its own.
#[tokio::test]
async fn the_relay_accepts_a_thread_reply_and_a_reaction_and_queries_them_by_e() -> Result<()> {
    use nostr::{EventBuilder, Kind, Tag};
    use twalk_clerk::relay::{
        answered_gesture, is_direct_reply, load_keys, targets, Relay, KIND_FORUM_COMMENT,
        KIND_REACTION,
    };

    let stack = RelayStack::ensure().await?;
    let run = run_id("smoke-gestures");
    let dir = std::env::temp_dir().join(&run);
    tokio::fs::create_dir_all(&dir).await?;
    let (key_file, _, channels) = seed(&stack, &run, &dir).await?;
    let channel = channels.approvals.as_str();
    let clerk = Relay::new(&stack.url, load_keys(&key_file)?)?;

    // The owner posts, reacts to the post and replies under it — the
    // three shapes Buzz's own builders write (`build_forum_post`,
    // `build_reaction`, `build_forum_comment` with root == parent).
    let owner_event = |kind: u16, tags: Vec<Vec<&str>>, content: &str| -> Result<nostr::Event> {
        let tags = tags
            .into_iter()
            .map(Tag::parse)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(EventBuilder::new(Kind::Custom(kind), content)
            .tags(tags)
            .sign_with_keys(&stack.owner)?)
    };
    let post = owner_event(
        45001,
        vec![vec!["h", channel]],
        "A post of the owner's own.",
    )?;
    stack.submit(&post).await?;
    let post_id = post.id.to_hex();
    let reaction = owner_event(KIND_REACTION, vec![vec!["e", &post_id]], "✅")?;
    stack.submit(&reaction).await?;
    let reply = owner_event(
        KIND_FORUM_COMMENT,
        vec![vec!["h", channel], vec!["e", &post_id, "", "reply"]],
        "Envoie-le.",
    )?;
    stack.submit(&reply).await?;

    // Both, by `#e`, under the clerk's key: the relay pushes `#e` to its
    // index for either kind, and membership of the channel is enough to
    // read a reaction that carries no `h` of its own.
    let gestures = clerk
        .gestures_on(std::slice::from_ref(&post_id), 100)
        .await?;
    let ids: Vec<String> = gestures.iter().map(|e| e.id.to_hex()).collect();
    assert_eq!(gestures.len(), 2, "{ids:?}");
    assert!(ids.contains(&reaction.id.to_hex()), "{ids:?}");
    assert!(ids.contains(&reply.id.to_hex()), "{ids:?}");
    for gesture in &gestures {
        assert!(targets(gesture, &post_id), "{gesture:?}");
        assert_eq!(gesture.pubkey, stack.owner.public_key());
    }
    let found_reply = gestures.iter().find(|e| e.id == reply.id).unwrap();
    assert!(is_direct_reply(found_reply, &post_id), "{found_reply:?}");
    assert_eq!(found_reply.content, "Envoie-le.");
    let found_reaction = gestures.iter().find(|e| e.id == reaction.id).unwrap();
    assert!(!is_direct_reply(found_reaction, &post_id));
    assert_eq!(found_reaction.content, "✅");

    // Nothing the clerk wrote yet; then the clerk answers the reaction in
    // the post's thread, and its own memory of that is the relay.
    assert!(clerk
        .own_comments_on(std::slice::from_ref(&post_id), 100)
        .await?
        .is_empty());
    let answer = clerk
        .comment(channel, &post_id, &reaction.id.to_hex(), "Envoyé.")
        .await?;
    assert!(answer.accepted, "{answer:?}");
    let own = clerk
        .own_comments_on(std::slice::from_ref(&post_id), 100)
        .await?;
    assert_eq!(own.len(), 1, "{own:?}");
    assert_eq!(own[0].id.to_hex(), answer.event_id);
    assert_eq!(own[0].pubkey.to_hex(), clerk.public_key_hex());
    assert!(is_direct_reply(&own[0], &post_id), "{:?}", own[0]);
    assert_eq!(
        answered_gesture(&own[0]),
        Some(reaction.id.to_hex().as_str())
    );

    // The clerk's own answer is a gesture on the post too, by anyone's
    // count; the owner's reads as the owner reads it, in the channel.
    assert_eq!(
        clerk
            .gestures_on(std::slice::from_ref(&post_id), 100)
            .await?
            .len(),
        3
    );
    let in_channel = stack.events_in(channel, &[KIND_FORUM_COMMENT]).await?;
    assert_eq!(in_channel.len(), 2, "{in_channel:?}");

    tokio::fs::remove_dir_all(&dir).await?;
    Ok(())
}
