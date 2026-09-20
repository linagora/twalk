//! The clerk decides (ticket #284, ADR 0036): a ✅ by the owner on one of
//! its `approbations` posts is one `POST /api/approvals` on the Companion
//! Gateway as the owner's `Buzz` device, a reply in the thread is that
//! approval with the reply's text, a ❌ is a local refusal that reaches
//! nobody, a stranger's gesture is answered and carried nowhere, and every
//! refusal the Gateway can give is answered in the thread with the
//! Companion's own sentence for it.
//!
//! The seam is the process boundary: the real `twalk-clerk` binary, a real
//! Buzz relay the suite brings up, the real bus, and a **stub Companion
//! Gateway** (`harness/gateway.rs`) that rotates the session's tokens the
//! way the real one does and records every approval it was asked for. A
//! test publishes a suggestion, sees the post, makes the owner's gesture on
//! the relay as the owner's key — the key `CLERK_OWNER_PUBKEY` names — and
//! then reads four things: what the stub was asked, what the relay holds,
//! the clerk's own `/metrics`, and its log.
//!
//! What the suite is careful about, because #148 was: every wait is a
//! bounded poll on the relay, the stub or the log, never a sleep; "still
//! exactly one after two more ticks" is asserted by counting the loop's
//! own tick lines rather than by waiting a number of seconds; and the
//! refusal codes and their statuses are read out of
//! `companion-gateway/openapi.yaml` by the same walk `refusals.rs`'s own
//! test makes, so this suite cannot pass on a table the Gateway has moved
//! on from.
//!
//! The sentences asserted are `twalk_clerk::text`'s and
//! `twalk_clerk::refusals`'s, in French — the run's language — rather than
//! literals: the suite proves the clerk wrote *that* sentence in *that*
//! thread, and what the sentence says is `text.rs`'s own tests' business.
//!
//! Each test's stub Gateway takes a port of its own from the harness's band
//! (`harness::PORT_RANGE`, 17400–17499 — the module doc there says why that
//! hundred), `17400 + n`, so the tests of this binary run in parallel. The
//! unreachable-Gateway tests use the band's reserved last port, which
//! nothing listens on.
//!
//! The last section is #300's, the device's one **read** before a post:
//! `GET /api/suggestions/{id}`, once per suggestion, so that the post says
//! whether the reply can reach the contact in the Companion's own words
//! and a suggestion the Gateway already records as approved is not posted.
//! Those tests read the stub's record of what was read as much as the
//! relay, because "once" is the claim (ADR 0035) and a restart is how it
//! is tested.

mod harness;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use harness::{
    line_references_event, poll_until, suggestion, wait_on_relay, Answer, Run, StubGateway,
    SuggestionAnswer, CONTACT, DECISION_SECONDS, GATEWAY_SUGGESTION_BODY, OWNER_MATRIX_ID,
    SWEEP_SECONDS, UNREACHABLE_GATEWAY_URL,
};
use nostr::Event;
use twalk_clerk::gateway::{Delivery, REQUEST_TIMEOUT};
use twalk_clerk::refusals::{
    delivery_line, delivery_unread_line, known_codes, remedy, sent, Remedy, Unread,
};
use twalk_clerk::text::{
    activity_approved, activity_refused_locally, activity_suggested, thread_not_recorded,
    thread_not_the_owner, thread_refused, thread_revoked, Lang,
};

/// The suite's language: the run's, French.
const LANG: Lang = Lang::Fr;

/// The network the contract's suggestion fixture names, and therefore the
/// one every `activite` line here names.
const NETWORK: &str = "whatsapp";

/// How long a suggestion lives when the test is not about its expiry:
/// long enough that no post is within the loop's "not recorded" window
/// (one tick plus the Gateway's ten-second request timeout) while the
/// test is still asserting on it.
const LONG_LIFE_SECONDS: i64 = 300;

/// How long after the owner's gesture its post may still stand when the
/// Gateway answers at once: a tick to read the gesture, the approval's
/// round trip, the delete, and the relay's own margin. Ten seconds is ten
/// ticks; a post still standing after that is a clerk that did not act.
const GONE_WITHIN: Duration = Duration::from_secs(10);

/// One `twalk_clerk_approvals_total` sample line, as `/metrics` renders it.
fn approvals_sample(outcome: &str, total: u64) -> String {
    format!("twalk_clerk_approvals_total{{outcome=\"{outcome}\"}} {total}")
}

/// The value of the `twalk_clerk_approvals_total{outcome="…"}` line, or
/// zero when the row is not rendered yet (a code that has not occurred).
async fn approvals_value(run: &Run, outcome: &str) -> Result<u64> {
    let prefix = format!("twalk_clerk_approvals_total{{outcome=\"{outcome}\"}} ");
    let metrics = run.clerk.metrics().await?;
    Ok(metrics
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().parse::<u64>())
        .transpose()
        .context("an approvals_total value is an integer")?
        .unwrap_or(0))
}

/// Polls until `twalk_clerk_approvals_total{outcome="…"}` is at least
/// `at_least`; the failure carries the exposition.
async fn wait_for_approvals_at_least(run: &Run, outcome: &str, at_least: u64) -> Result<u64> {
    let reached = poll_until(
        || async {
            let value = approvals_value(run, outcome).await.ok()?;
            (value >= at_least).then_some(value)
        },
        &format!("twalk_clerk_approvals_total{{outcome=\"{outcome}\"}} >= {at_least}"),
    )
    .await;
    match reached {
        Ok(value) => Ok(value),
        Err(error) => {
            let metrics = run.clerk.metrics().await?;
            anyhow::bail!("{error}; /metrics was:\n{metrics}")
        }
    }
}

/// Whether `event` carries the tag the clerk writes for the gesture it
/// answers — `["r", "twalk:gesture:<id>"]` — spelled here so the suite
/// reads the relay back on its own terms.
fn answers_gesture(event: &Event, gesture_id: &str) -> bool {
    let wanted = format!("twalk:gesture:{gesture_id}");
    event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .any(|tag| tag.len() >= 2 && tag[0] == "r" && tag[1] == wanted)
}

/// Whether `event` is a direct reply to the post `post_id` — one `e` tag,
/// `["e", post_id, "", "reply"]`, `buzz-sdk`'s own shape — and not a reply
/// to the gesture or a nested one.
fn is_direct_reply_to(event: &Event, post_id: &str) -> bool {
    let e_tags: Vec<&[String]> = event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .filter(|tag| tag.len() >= 2 && tag[0] == "e")
        .collect();
    matches!(e_tags.as_slice(), [tag] if tag[1] == post_id && tag.get(3).map(String::as_str) == Some("reply"))
}

/// Publishes one long-lived suggestion as event `n` of the run and waits
/// for its post; returns the suggestion id and the post id.
async fn post_suggestion(run: &Run, n: u32, life_seconds: i64) -> Result<(String, String)> {
    let event = suggestion(&run.id, n, life_seconds)?;
    let id = event["id"].as_str().unwrap().to_owned();
    run.publish("persona.suggest.produced", &event).await?;
    let post = run.wait_for_post(&id).await?;
    Ok((id, post.id.to_hex()))
}

/// Every event the clerk wrote on any of the run's three channels.
async fn everything_the_clerk_wrote(run: &Run) -> Result<Vec<Event>> {
    let mut all = Vec::new();
    for channel in [
        &run.channels.approvals,
        &run.channels.activity,
        &run.channels.journal,
    ] {
        all.extend(
            run.all_events_in(channel)
                .await?
                .into_iter()
                .filter(|event| event.pubkey.to_hex() == run.clerk_pubkey),
        );
    }
    Ok(all)
}

/// Asserts that neither the contact the stub's `Approval` names nor the
/// owner's Matrix ID reached anything the clerk wrote on Buzz or logged:
/// the `201` carries both, and neither is the owner's to read on a relay.
async fn assert_nothing_of_the_gateways_answer_reached_buzz(run: &Run) -> Result<()> {
    for event in everything_the_clerk_wrote(run).await? {
        let serialised = serde_json::to_string(&event)?;
        for secret in [CONTACT, OWNER_MATRIX_ID] {
            assert!(
                !serialised.contains(secret),
                "the clerk wrote {secret:?} onto Buzz: {serialised}"
            );
        }
    }
    let logs = run.clerk.logs().await;
    for secret in [CONTACT, OWNER_MATRIX_ID] {
        assert!(!logs.contains(secret), "the clerk logged {secret:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_check_by_the_owner_is_one_approval_and_the_post_is_gone() -> Result<()> {
    let stub = StubGateway::start(17401).await?;
    let mut run = Run::start_with_write_half("owner-check", &stub).await?;
    let (id, post_id) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;

    run.react_as_owner(&post_id, "✅").await?;

    run.wait_until_gone(&id, GONE_WITHIN).await?;
    {
        let state = stub.state();
        assert_eq!(state.approvals.len(), 1, "{:?}", state.approvals);
        let call = &state.approvals[0];
        assert_eq!(call.suggestion_id, id);
        assert_eq!(call.final_body, None, "a ✅ sends the persona's own words");
        assert_eq!(
            Some(call.device_token.as_str()),
            state.current_device_token(),
            "the call was made as the device the refresh issued"
        );
        assert_eq!(state.refreshes, 1, "one refresh at startup, and no more");
    }
    run.wait_for_line(
        &run.channels.activity,
        &activity_approved(LANG, NETWORK, false),
    )
    .await?;
    run.assert_metric(&approvals_sample("approved", 1)).await?;
    run.assert_metric("twalk_clerk_posts_total{channel=\"approbations\"} 1")
        .await?;
    assert!(
        run.clerk_thread_of(&post_id).await?.is_empty(),
        "an approval that went out is not answered in the thread"
    );

    // A second tick and a restart find nothing to carry: the post is gone,
    // and the relay is the only memory the clerk has (ADR 0035).
    run.wait_for_ticks(2).await?;
    assert_eq!(stub.state().approvals.len(), 1);
    run.restart_clerk().await?;
    run.wait_for_ticks(2).await?;
    assert_eq!(
        stub.state().approvals.len(),
        1,
        "a restart does not re-approve"
    );
    assert_eq!(
        stub.state().refreshes,
        2,
        "a restart refreshes the session once"
    );
    run.assert_metric_now(&approvals_sample("approved", 0))
        .await
        .context("the restarted clerk's counters start at zero")?;
    assert_nothing_of_the_gateways_answer_reached_buzz(&run).await?;

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_reply_by_the_owner_approves_with_its_text() -> Result<()> {
    let stub = StubGateway::start(17402).await?;
    let run = Run::start_with_write_half("owner-reply", &stub).await?;
    let (id, post_id) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;
    let text = "Merci, à 20h alors.";

    let reply = run.reply_as_owner(&post_id, text).await?;

    run.wait_until_gone(&id, GONE_WITHIN).await?;
    {
        let state = stub.state();
        assert_eq!(state.approvals.len(), 1, "{:?}", state.approvals);
        assert_eq!(state.approvals[0].suggestion_id, id);
        assert_eq!(
            state.approvals[0].final_body.as_deref(),
            Some(text),
            "the reply's text is the final body"
        );
    }
    run.wait_for_line(
        &run.channels.activity,
        &activity_approved(LANG, NETWORK, true),
    )
    .await?;
    run.assert_metric(&approvals_sample("approved", 1)).await?;

    // The clerk deleted its post, not the owner's reply: a deleted root
    // leaves its comments as rows, and the owner's words stay theirs.
    let thread = run.thread_of(&post_id).await?;
    assert!(
        thread.iter().any(|event| event.id == reply.id),
        "the owner's reply is still on the relay: {thread:?}"
    );
    assert!(
        run.clerk_thread_of(&post_id).await?.is_empty(),
        "nothing answered in the thread: {thread:?}"
    );
    assert_nothing_of_the_gateways_answer_reached_buzz(&run).await?;

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_redelivered_reply_does_not_send_twice() -> Result<()> {
    let stub = StubGateway::start(17403).await?;
    let run = Run::start_with_write_half("redelivered-reply", &stub).await?;
    let (id, post_id) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;

    let reply = run.reply_as_owner(&post_id, "Oui, d’accord.").await?;
    // The same signed event again, before the clerk has looked: the relay
    // holds it once.
    let message = run.resubmit_as_owner(&reply).await?;
    assert!(message.starts_with("duplicate:"), "{message}");

    run.wait_until_gone(&id, GONE_WITHIN).await?;
    assert_eq!(stub.state().approvals.len(), 1);

    // And again after the post is gone — a client retrying long after.
    // The relay itself refuses a reply whose parent is deleted, so the
    // retry never even lands; two more ticks find nothing to carry.
    let refused = run
        .resubmit_as_owner(&reply)
        .await
        .expect_err("a reply to a deleted post is refused by the relay");
    assert!(
        refused.to_string().contains("parent not found"),
        "{refused:#}"
    );
    run.wait_for_ticks(2).await?;
    assert_eq!(
        stub.state().approvals.len(),
        1,
        "{:?}",
        stub.state().approvals
    );
    run.assert_metric_now(&approvals_sample("approved", 1))
        .await?;
    run.assert_metric_now(&approvals_sample("already_approved", 0))
        .await?;

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_check_by_a_stranger_makes_no_call_and_is_answered_once() -> Result<()> {
    let stub = StubGateway::start(17404).await?;
    let run = Run::start_with_write_half("stranger-check", &stub).await?;
    let (id, post_id) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;

    let check = run.react_as_stranger(&post_id, "✅").await?;
    let stranger = check.pubkey.to_hex();
    assert_ne!(stranger, run.owner_pubkey());

    let answer = run
        .wait_for_thread_line(&post_id, &thread_not_the_owner(LANG))
        .await?;
    assert_eq!(answer.content, thread_not_the_owner(LANG));
    assert!(
        answers_gesture(&answer, &check.id.to_hex()),
        "the answer is keyed on the stranger's reaction: {:?}",
        answer.tags
    );
    assert!(
        is_direct_reply_to(&answer, &post_id),
        "the answer is a direct reply to the post, under it and not under the reaction: {:?}",
        answer.tags
    );
    run.assert_metric(&approvals_sample("not_the_owner", 1))
        .await?;

    // A relay **admin** is a stranger too: every right the relay grants
    // short of ownership, and the clerk reads no role. Answered on the same
    // terms, keyed on their own reaction.
    let admins_check = run.react_as_admin(&post_id, "✅").await?;
    assert_ne!(admins_check.pubkey.to_hex(), run.owner_pubkey());
    let answers = wait_on_relay(
        || async {
            let answers = run.clerk_thread_of(&post_id).await.ok()?;
            (answers.len() >= 2).then_some(answers)
        },
        "the admin's reaction answered in the thread",
    )
    .await?;
    assert!(
        answers
            .iter()
            .any(|answer| answers_gesture(answer, &admins_check.id.to_hex())
                && answer.content == thread_not_the_owner(LANG)),
        "the admin is told the same thing, keyed on their reaction: {answers:?}"
    );
    run.assert_metric(&approvals_sample("not_the_owner", 2))
        .await?;

    // Two ticks later: still one answer each, no call, the post still there.
    run.wait_for_ticks(2).await?;
    let answers = run.clerk_thread_of(&post_id).await?;
    assert_eq!(answers.len(), 2, "each answered once: {answers:?}");
    assert!(
        stub.state().approvals.is_empty(),
        "{:?}",
        stub.state().approvals
    );
    assert_eq!(
        stub.state().unauthenticated,
        0,
        "no call was even attempted"
    );
    assert_eq!(run.posts_about(&id).await?.len(), 1, "the post stays");
    run.assert_metric_now(&approvals_sample("not_the_owner", 2))
        .await?;
    run.assert_metric_now(&approvals_sample("approved", 0))
        .await?;
    run.assert_metric_now("twalk_clerk_posts_total{channel=\"approbations\"} 1")
        .await
        .context("a thread answer is not a post")?;

    // The log names the stranger by a prefix of their key — a public key
    // is not a contact — and never the content of anything.
    let logs = run.clerk.logs().await;
    assert!(
        logs.contains(&stranger[..8]),
        "the log names the stranger's key: {logs}"
    );

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

/// The status the Gateway answers each refusal code of `POST /api/approvals`
/// with, read out of `companion-gateway/openapi.yaml` by the walk
/// `refusals.rs`'s own test makes — per status key under the operation's
/// `responses`, every `error.enum` value beneath it. A response that is a
/// `$ref` to one of `components.responses` (the two shared ones,
/// `Unauthenticated` and `ApprovalsNotConfigured`) is **resolved** rather
/// than named by hand, so no status in this suite is a number that can
/// drift from the description; and a code documented under two statuses
/// is a failure of the walk rather than whichever came last.
fn statuses_from_openapi() -> Result<BTreeMap<String, u16>> {
    let document: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(include_str!("../../companion-gateway/openapi.yaml"))
            .context("openapi.yaml parses")?;
    let paths = document
        .get("paths")
        .and_then(|paths| paths.as_mapping())
        .context("openapi.yaml has paths")?;
    let mut statuses = BTreeMap::new();
    for (_, operations) in paths {
        let Some(operations) = operations.as_mapping() else {
            continue;
        };
        for (_, operation) in operations {
            if operation.get("operationId").and_then(|id| id.as_str()) != Some("approveSuggestion")
            {
                continue;
            }
            let responses = operation
                .get("responses")
                .and_then(|responses| responses.as_mapping())
                .context("approveSuggestion has responses")?;
            for (status, response) in responses {
                let status: u16 = status
                    .as_str()
                    .context("a response is keyed by its status")?
                    .parse()
                    .context("a status is a number")?;
                let response = resolve_response(&document, response)?;
                let mut codes = Vec::new();
                collect_codes(response, &mut codes);
                for code in codes {
                    anyhow::ensure!(
                        statuses.insert(code.clone(), status).is_none(),
                        "openapi.yaml documents {code} under two statuses of approveSuggestion"
                    );
                }
            }
        }
    }
    anyhow::ensure!(
        !statuses.is_empty(),
        "the walk found no refusal code under approveSuggestion"
    );
    anyhow::ensure!(
        statuses.contains_key("approvals_not_configured"),
        "the $ref to the shared ApprovalsNotConfigured response was not resolved: {statuses:?}"
    );
    Ok(statuses)
}

/// `response` itself, or the shared response it is a `$ref` to
/// (`#/components/responses/<Name>`).
fn resolve_response<'a>(
    document: &'a serde_yaml_ng::Value,
    response: &'a serde_yaml_ng::Value,
) -> Result<&'a serde_yaml_ng::Value> {
    let Some(reference) = response.get("$ref").and_then(|r| r.as_str()) else {
        return Ok(response);
    };
    let name = reference
        .strip_prefix("#/components/responses/")
        .with_context(|| format!("a response $ref this walk does not follow: {reference}"))?;
    document
        .get("components")
        .and_then(|components| components.get("responses"))
        .and_then(|responses| responses.get(name))
        .with_context(|| format!("{reference} names no shared response"))
}

fn collect_codes(node: &serde_yaml_ng::Value, codes: &mut Vec<String>) {
    match node {
        serde_yaml_ng::Value::Sequence(items) => {
            for item in items {
                collect_codes(item, codes);
            }
        }
        serde_yaml_ng::Value::Mapping(record) => {
            if let Some(values) = record
                .get("error")
                .and_then(|error| error.get("enum"))
                .and_then(|values| values.as_sequence())
            {
                codes.extend(values.iter().filter_map(|v| v.as_str()).map(str::to_owned));
            }
            for (_, value) in record {
                collect_codes(value, codes);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn every_refusal_code_is_answered_with_the_companions_sentence() -> Result<()> {
    let statuses = statuses_from_openapi()?;
    // Every code the clerk has words for, minus the two that are not a
    // refusal answered in the thread: `unauthenticated` (the session, its
    // own test) and `already_approved` (success, its own test).
    let codes: Vec<&str> = known_codes()
        .into_iter()
        .filter(|code| *code != "unauthenticated" && *code != "already_approved")
        .collect();
    for code in &codes {
        anyhow::ensure!(
            statuses.contains_key(*code),
            "the clerk's table knows {code} and openapi.yaml documents no status for it: {statuses:?}"
        );
    }

    let stub = StubGateway::start(17405).await?;
    let run = Run::start_with_write_half("every-refusal", &stub).await?;

    // Publish everything first, then react to everything, then wait:
    // the loop reads every post on every tick, so the whole table is
    // carried in a few ticks rather than one code per round trip.
    let mut posts: Vec<Refused> = Vec::new();
    for (n, code) in codes.iter().enumerate() {
        let event = suggestion(&run.id, n as u32 + 1, LONG_LIFE_SECONDS)?;
        let id = event["id"].as_str().unwrap().to_owned();
        stub.state().answer(
            &id,
            Answer::Refuse {
                status: statuses[*code],
                code: (*code).to_owned(),
            },
        );
        run.publish("persona.suggest.produced", &event).await?;
        posts.push(Refused {
            code,
            id,
            post_id: String::new(),
            check_id: String::new(),
        });
    }
    for post in posts.iter_mut() {
        post.post_id = run.wait_for_post(&post.id).await?.id.to_hex();
    }
    for post in posts.iter_mut() {
        post.check_id = run.react_as_owner(&post.post_id, "✅").await?.id.to_hex();
    }

    // Three kinds of answer, by the Companion's own remedy table: the one
    // refusal that is not a failure went out; the two that may work next
    // time are tried again in silence; every other one is told once.
    for Refused {
        code, id, post_id, ..
    } in &posts
    {
        if sent(code) {
            run.wait_until_gone(id, GONE_WITHIN)
                .await
                .with_context(|| format!("{code}: the reply went out, so the post goes"))?;
            run.assert_metric(&approvals_sample(code, 1)).await?;
        } else if remedy(code) == Remedy::Retry {
            let attempts = wait_for_approvals_at_least(&run, code, 2).await?;
            assert!(attempts >= 2, "{code}: counted per attempt, got {attempts}");
        } else {
            let answer = run
                .wait_for_thread_line(post_id, &thread_refused(LANG, code))
                .await
                .with_context(|| code.to_string())?;
            assert_eq!(answer.content, thread_refused(LANG, code), "{code}");
            run.assert_metric(&approvals_sample(code, 1)).await?;
        }
    }
    run.wait_for_line(
        &run.channels.activity,
        &activity_approved(LANG, NETWORK, false),
    )
    .await
    .context("the reply that went out but was not recorded is in the feed as approved")?;

    // Two more ticks, and each kind is still what it was: told once, or
    // retried again and told nothing, or gone.
    // Read once for all of them — one query for every thread, one for
    // every post — because each read is a request of the owner's quota.
    run.wait_for_ticks(2).await?;
    let post_ids: Vec<String> = posts.iter().map(|post| post.post_id.clone()).collect();
    let all_answers = run.clerk_threads_of(&post_ids).await?;
    let all_standing = run.standing_posts().await?;
    let mut retried = 0;
    for Refused {
        code,
        id,
        post_id,
        check_id,
    } in &posts
    {
        let answers: Vec<&Event> = all_answers
            .iter()
            .filter(|answer| is_direct_reply_to(answer, post_id))
            .collect();
        let standing = all_standing
            .iter()
            .filter(|post| post.id.to_hex() == *post_id)
            .count();
        if sent(code) {
            assert!(answers.is_empty(), "{code}: no thread line: {answers:?}");
            assert_eq!(standing, 0, "{code}: the post is gone");
        } else if remedy(code) == Remedy::Retry {
            assert!(
                answers.is_empty(),
                "{code}: nothing in the thread while it is retried: {answers:?}"
            );
            assert_eq!(standing, 1, "{code}: the post stays");
            retried += 1;
        } else {
            assert_eq!(answers.len(), 1, "{code}: told once: {answers:?}");
            assert!(answers_gesture(answers[0], check_id));
            assert_eq!(standing, 1, "{code}: the post stays");
            run.assert_metric_now(&approvals_sample(code, 1)).await?;
        }
        let calls = stub.state().approvals_of(id);
        if remedy(code) == Remedy::Retry {
            assert!(calls.len() >= 2, "{code}: called per tick: {calls:?}");
        } else {
            assert_eq!(calls.len(), 1, "{code}: called once: {calls:?}");
        }
    }
    assert_eq!(
        retried, 2,
        "bus_unreachable and store_unavailable are the retried ones"
    );
    run.assert_metric_now(&approvals_sample("approved", 0))
        .await?;
    assert_nothing_of_the_gateways_answer_reached_buzz(&run).await?;

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

/// One suggestion of the refusal table: its code, its id, its post and
/// the owner's ✅ on it.
struct Refused {
    code: &'static str,
    id: String,
    post_id: String,
    check_id: String,
}

#[tokio::test]
async fn already_approved_is_success() -> Result<()> {
    let stub = StubGateway::start(17406).await?;
    let run = Run::start_with_write_half("already-approved", &stub).await?;
    let (id, post_id) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;
    stub.state().answer(&id, Answer::AlreadyApproved);

    run.react_as_owner(&post_id, "✅").await?;

    run.wait_until_gone(&id, GONE_WITHIN).await?;
    run.assert_metric(&approvals_sample("already_approved", 1))
        .await?;
    run.assert_metric_now(&approvals_sample("approved", 0))
        .await?;
    assert_eq!(stub.state().approvals.len(), 1);
    assert!(
        run.clerk_thread_of(&post_id).await?.is_empty(),
        "a duplicate is not a refusal, so nothing is said in the thread"
    );
    // And no feed line either: the approval the Gateway already held was
    // decided elsewhere — the approval screen, or an earlier tick whose
    // line exists — and a second "approved from Buzz" would attribute the
    // decision to this gesture. The absence is read after the deletion
    // the line would have followed and a tick more, so it is an absence
    // and not an early read.
    run.wait_for_ticks(1).await?;
    let feed = run.lines_in(&run.channels.activity).await?;
    let approved_lines: Vec<&Event> = feed
        .iter()
        .filter(|line| line.content == activity_approved(LANG, NETWORK, false))
        .collect();
    assert!(
        approved_lines.is_empty(),
        "a duplicate wrote an approval line: {approved_lines:?}"
    );

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_revoked_device_is_named_in_the_thread() -> Result<()> {
    let stub = StubGateway::start(17407).await?;
    let mut run = Run::start_with_write_half("revoked-device", &stub).await?;
    let (id, post_id) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;
    // Revoked from the dashboard after the clerk signed in.
    stub.state().revoked = true;

    let check = run.react_as_owner(&post_id, "✅").await?;

    let told = run
        .wait_for_thread_line(&post_id, &thread_revoked(LANG))
        .await?;
    assert_eq!(told.content, thread_revoked(LANG));
    assert!(answers_gesture(&told, &check.id.to_hex()));
    run.clerk
        .wait_for_log("provision-clerk-device.sh")
        .await
        .context("the log names the script that signs the device in again")?;
    run.assert_metric(&approvals_sample("unauthenticated", 1))
        .await?;
    {
        let state = stub.state();
        assert!(
            state.approvals.is_empty(),
            "nothing got past the door: {:?}",
            state.approvals
        );
        assert!(state.unauthenticated >= 1, "the call was made and refused");
    }

    // Told once, and no hot loop of refusals: two ticks later the count
    // and the thread are what they were, and the stub was asked nothing
    // more.
    let refused_so_far = stub.state().unauthenticated;
    run.wait_for_ticks(2).await?;
    assert_eq!(run.clerk_thread_of(&post_id).await?.len(), 1);
    run.assert_metric_now(&approvals_sample("unauthenticated", 1))
        .await?;
    assert_eq!(
        stub.state().unauthenticated,
        refused_so_far,
        "a dead session makes no further call"
    );
    assert_eq!(run.posts_about(&id).await?.len(), 1, "the post waits");

    // A ❌ needs no session: it deletes locally even now.
    let (refused_id, refused_post) = post_suggestion(&run, 2, LONG_LIFE_SECONDS).await?;
    run.react_as_owner(&refused_post, "❌").await?;
    run.wait_until_gone(&refused_id, GONE_WITHIN).await?;
    run.assert_metric(&approvals_sample("refused_locally", 1))
        .await?;

    // The operator signs the device in again and restarts the clerk: the
    // ✅ still on the relay is carried on the first tick.
    run.provision_device(&stub).await?;
    run.restart_clerk().await?;
    run.wait_until_gone(&id, GONE_WITHIN).await?;
    {
        let state = stub.state();
        assert_eq!(state.approvals.len(), 1, "{:?}", state.approvals);
        assert_eq!(state.approvals[0].suggestion_id, id);
        assert_eq!(state.approvals[0].final_body, None);
    }
    run.assert_metric(&approvals_sample("approved", 1)).await?;
    run.wait_for_line(
        &run.channels.activity,
        &activity_approved(LANG, NETWORK, false),
    )
    .await?;
    let thread = run.clerk_thread_of(&post_id).await?;
    assert_eq!(
        thread.len(),
        1,
        "the revoked line stays and nothing is added: {thread:?}"
    );
    assert_eq!(thread[0].content, thread_revoked(LANG));

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

/// How long the suggestion lives in the unreachable-Gateway test: well past
/// the loop's "not recorded" window (one tick plus the ten-second request
/// timeout), so that the gesture is tried at least twice **before** the
/// window opens and the thread line is written inside it — with room for
/// the owner's ✅ to land late, since a relay that is rate-limiting the
/// owner's key holds that one request for as long as it asks.
const UNREACHABLE_LIFE_SECONDS: i64 = 40;

/// The relay's own round trips around a wait: the query that finds the
/// line, the poll that observes it.
const MARGIN_SECONDS: u64 = 4;

/// The loop's "not recorded" window at this suite's tick, from the crate's
/// own arithmetic: a tick plus the Gateway request timeout.
fn not_recorded_window() -> Duration {
    twalk_clerk::consumers::not_recorded_window(Duration::from_secs(DECISION_SECONDS))
}

#[tokio::test]
async fn an_unreachable_gateway_is_retried_then_said() -> Result<()> {
    let run =
        Run::start_with_write_half_at("unreachable-gateway", UNREACHABLE_GATEWAY_URL, "R0").await?;
    // The startup refresh failed and said so, and the clerk runs anyway.
    run.clerk
        .wait_for_log("could not be reached")
        .await
        .context("the startup refresh names the outage")?;
    let published = Instant::now();
    let (id, post_id) = post_suggestion(&run, 1, UNREACHABLE_LIFE_SECONDS).await?;

    let check = run.react_as_owner(&post_id, "✅").await?;

    // The ✅ must land while there is time to retry before the window
    // opens: at least two ticks before `life − window`. A relay that is
    // rate-limiting the owner's key holds that one request for as long as
    // it asks, and a ✅ inside the window is told "not recorded" at once —
    // correct, and not what this test is about, so the failure names it.
    let landed = published.elapsed();
    let latest = Duration::from_secs(UNREACHABLE_LIFE_SECONDS as u64)
        - not_recorded_window()
        - Duration::from_secs(2 * DECISION_SECONDS)
        - Duration::from_secs(MARGIN_SECONDS);
    anyhow::ensure!(
        landed < latest,
        "the owner's ✅ landed {}s after the suggestion was published, past the {}s this test \
         needs to see it retried before the not-recorded window opens — the relay was probably \
         rate-limiting the owner's key (300 requests a minute), so the suite, not the clerk, is \
         what to look at",
        landed.as_secs(),
        latest.as_secs()
    );

    // Counted per attempt: an outage reads as a slope.
    wait_for_approvals_at_least(&run, "gateway_unreachable", 2).await?;
    assert!(
        run.clerk_thread_of(&post_id).await?.is_empty(),
        "nothing is said while there is still time to try again"
    );
    // Then, inside the window before the expiry, the thread is told once.
    // The bound is the test's own arithmetic: the line is written on the
    // first tick inside the window, which is before the expiry — `life`
    // from the publication — and at worst one tick late when the tick that
    // opens the window has already read its posts; a second tick and the
    // relay's margin on top. Not the generic relay wait, which happened to
    // exceed this by a few seconds and would lose to load (#186).
    let told_within = Duration::from_secs(UNREACHABLE_LIFE_SECONDS as u64)
        + Duration::from_secs(2 * DECISION_SECONDS)
        + Duration::from_secs(MARGIN_SECONDS)
        - published.elapsed();
    let told = run
        .wait_for_thread_line_within(&post_id, &thread_not_recorded(LANG), told_within)
        .await?;
    assert_eq!(told.content, thread_not_recorded(LANG));
    assert!(answers_gesture(&told, &check.id.to_hex()));
    // And the sweep deletes the expired post as it would any other.
    let gone_within =
        Duration::from_secs(UNREACHABLE_LIFE_SECONDS as u64 + 2 * SWEEP_SECONDS + MARGIN_SECONDS);
    run.wait_until_gone(&id, gone_within).await?;
    run.assert_metric("twalk_clerk_deleted_total{why=\"expired\"} 1")
        .await?;
    run.wait_for_ticks(2).await?;
    let thread = run.clerk_thread_of(&post_id).await?;
    assert_eq!(thread.len(), 1, "told once: {thread:?}");
    run.assert_metric_now(&approvals_sample("approved", 0))
        .await?;
    let logs = run.clerk.logs().await;
    assert!(
        logs.contains(UNREACHABLE_GATEWAY_URL),
        "the warning names the Gateway it could not reach"
    );

    run.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn a_check_present_before_the_clerk_starts_is_carried() -> Result<()> {
    let stub = StubGateway::start(17409).await?;
    let mut run = Run::start_with_write_half("check-before-start", &stub).await?;
    let (id, post_id) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;

    // The owner reacts while the clerk is away.
    run.stop_clerk().await?;
    run.react_as_owner(&post_id, "✅").await?;
    assert!(stub.state().approvals.is_empty());
    run.start_clerk().await?;

    run.wait_until_gone(&id, GONE_WITHIN).await?;
    assert_eq!(
        stub.state().approvals.len(),
        1,
        "{:?}",
        stub.state().approvals
    );
    assert_eq!(stub.state().approvals[0].suggestion_id, id);
    run.assert_metric(&approvals_sample("approved", 1)).await?;

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_cross_by_the_owner_deletes_locally() -> Result<()> {
    let stub = StubGateway::start(17410).await?;
    let run = Run::start_with_write_half("owner-cross", &stub).await?;
    let (id, post_id) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;

    run.react_as_owner(&post_id, "❌").await?;

    run.wait_until_gone(&id, GONE_WITHIN).await?;
    {
        let state = stub.state();
        assert!(state.approvals.is_empty(), "{:?}", state.approvals);
        assert_eq!(state.unauthenticated, 0, "no call at all");
    }
    run.wait_for_line(
        &run.channels.activity,
        &activity_refused_locally(LANG, NETWORK),
    )
    .await?;
    run.assert_metric(&approvals_sample("refused_locally", 1))
        .await?;
    run.assert_metric_now(&approvals_sample("approved", 0))
        .await?;
    assert!(run.clerk_thread_of(&post_id).await?.is_empty());

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

#[tokio::test]
async fn nothing_of_the_owners_reply_reaches_a_log_or_the_activity_feed() -> Result<()> {
    let stub = StubGateway::start(17411).await?;
    let run = Run::start_with_write_half("reply-is-private", &stub).await?;
    let (id, post_id) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;
    let marker = "MARQUEUR-7f3e-mot-du-proprietaire";
    let text = format!("Bien reçu, {marker}, on en reparle demain.");

    let reply = run.reply_as_owner(&post_id, &text).await?;

    run.wait_until_gone(&id, GONE_WITHIN).await?;
    // It reached the one place it was meant for.
    assert_eq!(
        stub.state().approvals[0].final_body.as_deref(),
        Some(text.as_str())
    );
    run.wait_for_line(
        &run.channels.activity,
        &activity_approved(LANG, NETWORK, true),
    )
    .await?;
    run.wait_for_ticks(1).await?;

    // And nowhere else: not the log, not a line the clerk wrote on any
    // channel. The owner's own reply is on the relay because it is the
    // owner's, and it is the only event that carries the words.
    let logs = run.clerk.logs().await;
    assert!(
        !logs.contains(marker),
        "the clerk logged the owner's words:\n{logs}"
    );
    for event in everything_the_clerk_wrote(&run).await? {
        let serialised = serde_json::to_string(&event)?;
        assert!(
            !serialised.contains(marker),
            "the clerk wrote the owner's words onto Buzz: {serialised}"
        );
    }
    let carriers: Vec<Event> = run
        .all_events_in(&run.channels.approvals)
        .await?
        .into_iter()
        .filter(|event| event.content.contains(marker))
        .collect();
    assert_eq!(carriers.len(), 1, "{carriers:?}");
    assert_eq!(carriers[0].id, reply.id, "only the owner's own reply");
    assert_nothing_of_the_gateways_answer_reached_buzz(&run).await?;

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------
// #300: the read before the post.
// ---------------------------------------------------------------------

/// The delivery line of one `approbations` post: the third line, where
/// `text::approval_post` puts it (the fixture's body is one line).
fn delivery_line_of(post: &Event) -> String {
    post.content
        .lines()
        .nth(2)
        .unwrap_or_else(|| panic!("a post has at least three lines:\n{}", post.content))
        .to_owned()
}

/// Builds suggestion `n` of the run, scripts what the stub says of it
/// **before** it is published, publishes it and waits for its post.
async fn post_scripted_suggestion(
    run: &Run,
    stub: &StubGateway,
    n: u32,
    answer: SuggestionAnswer,
) -> Result<(String, Event)> {
    let event = suggestion(&run.id, n, LONG_LIFE_SECONDS)?;
    let id = event["id"].as_str().unwrap().to_owned();
    stub.state().suggestion(&id, answer);
    run.publish("persona.suggest.produced", &event).await?;
    let post = run.wait_for_post(&id).await?;
    Ok((id, post))
}

/// The three readings the Gateway can give (`openapi.yaml`, `Delivery`),
/// each with the detail it usually comes with.
fn every_reach() -> Vec<(&'static str, &'static str)> {
    vec![
        ("can_reach", "owner_joined"),
        ("cannot_reach", "owner_invited"),
        ("unknown", "not_a_known_portal"),
    ]
}

/// The fixture's body, for asserting it is in no log line.
fn fixture_body(run: &Run) -> Result<String> {
    Ok(
        suggestion(&run.id, 1, LONG_LIFE_SECONDS)?["data"]["suggestion"]["body"]
            .as_str()
            .context("the fixture's body is a string")?
            .to_owned(),
    )
}

#[tokio::test]
async fn a_post_carries_the_companions_delivery_sentence_for_each_reach() -> Result<()> {
    let stub = StubGateway::start(17412).await?;
    let mut run = Run::start_with_write_half("delivery-line", &stub).await?;

    let mut ids = Vec::new();
    for (n, (reach, detail)) in every_reach().into_iter().enumerate() {
        let (id, post) = post_scripted_suggestion(
            &run,
            &stub,
            n as u32 + 1,
            SuggestionAnswer::new("approvable", reach, detail),
        )
        .await?;
        let expected = delivery_line(
            LANG,
            &Delivery {
                reach: reach.to_owned(),
                detail: detail.to_owned(),
            },
        );
        assert_eq!(
            delivery_line_of(&post),
            expected,
            "the post's third line is the Companion's sentence for {reach}/{detail}:\n{}",
            post.content
        );
        // The bus's body, not the Gateway's: the read is for the delivery
        // and the standing, and nothing else of the answer reaches the post.
        assert!(
            !post.content.contains(GATEWAY_SUGGESTION_BODY),
            "the post quotes the Gateway's copy of the body:\n{}",
            post.content
        );
        ids.push(id);
    }
    {
        let state = stub.state();
        for id in &ids {
            assert_eq!(state.reads_of(id), 1, "read once: {:?}", state.reads);
        }
        assert_eq!(state.reads.len(), ids.len(), "{:?}", state.reads);
    }

    // Two ticks later and after a restart, nothing has been read again:
    // the read is at posting time and never per tick, and the restarted
    // clerk finds its posts on the relay rather than reading anything to
    // know about them (ADR 0035). A fourth suggestion is the proof the
    // restarted process is consuming — and it is read once, like the rest.
    run.wait_for_ticks(2).await?;
    assert_eq!(
        stub.state().reads.len(),
        ids.len(),
        "a tick re-read a suggestion: {:?}",
        stub.state().reads
    );
    run.restart_clerk().await?;
    run.wait_for_ticks(2).await?;
    assert_eq!(
        stub.state().reads.len(),
        ids.len(),
        "a restart re-read a suggestion: {:?}",
        stub.state().reads
    );
    let (fourth, _) = post_scripted_suggestion(
        &run,
        &stub,
        4,
        SuggestionAnswer::new("approvable", "can_reach", "owner_joined"),
    )
    .await?;
    {
        let state = stub.state();
        for id in ids.iter().chain(std::iter::once(&fourth)) {
            assert_eq!(state.reads_of(id), 1, "read once: {:?}", state.reads);
        }
        assert_eq!(state.reads.len(), ids.len() + 1, "{:?}", state.reads);
    }
    run.assert_metric("twalk_clerk_posts_total{channel=\"approbations\"} 1")
        .await?;
    run.assert_metric_now("twalk_clerk_skipped_total{why=\"already_approved\"} 0")
        .await?;
    assert_nothing_of_the_gateways_answer_reached_buzz(&run).await?;

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

/// The relay's own round trips around the post: the bus delivery, the
/// own-posts query, the post itself and the poll that observes it.
const POST_MARGIN: Duration = Duration::from_secs(6);

#[tokio::test]
async fn a_gateway_that_does_not_answer_yields_the_unread_line_and_the_post_still_goes_up(
) -> Result<()> {
    let run =
        Run::start_with_write_half_at("unread-unreachable", UNREACHABLE_GATEWAY_URL, "R0").await?;
    run.clerk
        .wait_for_log("could not be reached")
        .await
        .context("the startup refresh names the outage")?;

    let published = Instant::now();
    let (id, _) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;
    let took = published.elapsed();
    let bound = REQUEST_TIMEOUT + Duration::from_secs(DECISION_SECONDS) + POST_MARGIN;
    assert!(
        took <= bound,
        "the post took {took:?}, past one tick plus the request timeout ({bound:?}): the read \
         blocked the post"
    );
    let post = run.posts_about(&id).await?.remove(0);
    assert_eq!(
        delivery_line_of(&post),
        delivery_unread_line(LANG, Unread::GatewayUnreachable),
        "{}",
        post.content
    );
    run.clerk
        .wait_for_log("no usable answer to the suggestion read")
        .await
        .context("the read's failure is a warning naming the Gateway")?;
    run.wait_for_line(&run.channels.activity, &activity_suggested(LANG, NETWORK))
        .await?;
    run.assert_metric("twalk_clerk_posts_total{channel=\"approbations\"} 1")
        .await?;
    let logs = run.clerk.logs().await;
    assert!(
        !logs.contains(&fixture_body(&run)?),
        "the warning carried the body:\n{logs}"
    );

    run.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn an_unscripted_suggestion_is_posted_as_not_found() -> Result<()> {
    let stub = StubGateway::start(17413).await?;
    let run = Run::start_with_write_half("unread-not-found", &stub).await?;

    let (id, _) = post_suggestion(&run, 1, LONG_LIFE_SECONDS).await?;

    let post = run.posts_about(&id).await?.remove(0);
    assert_eq!(
        delivery_line_of(&post),
        delivery_unread_line(LANG, Unread::NotFound),
        "{}",
        post.content
    );
    {
        let state = stub.state();
        assert_eq!(state.reads_of(&id), 1, "{:?}", state.reads);
        assert_eq!(state.unauthenticated, 0, "the read was made as the device");
    }
    run.clerk
        .wait_for_log("suggestion_not_found")
        .await
        .context("the warning names the Gateway's code")?;
    run.wait_for_line(&run.channels.activity, &activity_suggested(LANG, NETWORK))
        .await?;

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_suggestion_the_gateway_records_as_approved_is_not_posted() -> Result<()> {
    let stub = StubGateway::start(17414).await?;
    let run = Run::start_with_write_half("already-approved-read", &stub).await?;
    let activity_before = run.lines_in(&run.channels.activity).await?.len();

    let event = suggestion(&run.id, 1, LONG_LIFE_SECONDS)?;
    let id = event["id"].as_str().unwrap().to_owned();
    stub.state().suggestion(
        &id,
        SuggestionAnswer::new("approved", "can_reach", "owner_joined"),
    );
    run.publish("persona.suggest.produced", &event).await?;

    run.assert_metric("twalk_clerk_skipped_total{why=\"already_approved\"} 1")
        .await?;
    run.clerk
        .wait_for_log("already records as approved is not posted")
        .await?;
    // Two ticks and a second suggestion later — the second's post is the
    // proof the consumer acked the first and went on — still no post, no
    // line in activite, and one read.
    run.wait_for_ticks(2).await?;
    let (second, _) = post_suggestion(&run, 2, LONG_LIFE_SECONDS).await?;
    assert!(
        run.posts_about(&id).await?.is_empty(),
        "an approved suggestion was posted"
    );
    let activity = run.lines_in(&run.channels.activity).await?;
    assert!(
        !activity.iter().any(|line| line_references_event(line, &id)),
        "activite gained a line about the approved suggestion: {activity:?}"
    );
    assert_eq!(
        activity.len(),
        activity_before + 1,
        "activite changed by the second suggestion's line and nothing else: {activity:?}"
    );
    {
        let state = stub.state();
        assert_eq!(state.reads_of(&id), 1, "{:?}", state.reads);
        assert_eq!(state.reads_of(&second), 1, "{:?}", state.reads);
    }
    run.assert_metric("twalk_clerk_posts_total{channel=\"approbations\"} 1")
        .await?;
    run.assert_metric_now("twalk_clerk_skipped_total{why=\"already_approved\"} 1")
        .await?;
    // The approved answer carries the Approval record — the contact, the
    // owner — and none of it reached a log or the relay.
    assert_nothing_of_the_gateways_answer_reached_buzz(&run).await?;
    let logs = run.clerk.logs().await;
    assert!(
        !logs.contains(&fixture_body(&run)?),
        "the clerk logged the body:\n{logs}"
    );
    assert!(
        !logs.contains(GATEWAY_SUGGESTION_BODY),
        "the clerk logged the Gateway's body:\n{logs}"
    );

    run.shutdown().await?;
    stub.stop().await;
    Ok(())
}
