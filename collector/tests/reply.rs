//! An approved reply to a mail at the process boundary (issue #278): the
//! collector started against the fake SSO, the fake JMAP server and the
//! test stack's bus. An approval whose target is the mail connection is
//! sent from the owner's mailbox — to the sender alone, in the thread,
//! from the owner's identity, a copy in Sent — and reported `reach=contact`;
//! one whose target is a room is left to the Sensor; one the mailbox
//! refuses is dead-lettered with the reason; one the server does not answer
//! is retried, then dead-lettered.

mod support;

use anyhow::Result;
use serde_json::{json, Value};
use support::{sha256_hex, Run, OWNER};
use twalk_test_harness::jmap_fake::{FakeMail, DRAFTS_ID, IDENTITY_ID, SENT_ID};
use twalk_test_harness::{ensure_stack, poll_until, validate_against_contract, Bus, StoredMessage};

const APPROVED_SUBJECT: &str = "twalk.persona.reply.approved.v1";
const POSTED_SUBJECT: &str = "twalk.persona.reply.approved.v1.posted";
const DEAD_SUBJECT: &str = "twalk.persona.reply.approved.v1.dead";
const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";

/// A `persona.reply.approved.v1` the Companion Gateway would publish for a
/// suggestion answering a mail on this run's connection (#278's shape).
fn approval(run: &Run, in_reply_to: &str, body: &str, label: &str) -> Value {
    approval_to(run, in_reply_to, "mailto:alice@example.org", body, label)
}

/// The same, addressed: `recipient` is the trigger's sender, the one the
/// approval's consent check was about.
fn approval_to(run: &Run, in_reply_to: &str, recipient: &str, body: &str, label: &str) -> Value {
    let suggestion = sha256_hex(&format!("suggestion:{label}:{}", run.mail));
    let event = json!({
        "specversion": "1.0",
        "id": sha256_hex(&format!("{suggestion}:@michel:example.com")),
        "source": "hermes://twalk.example.com/personas/assistant",
        "type": "fr.linagora.twalk.persona.reply.approved.v1",
        "time": "2026-09-21T08:20:11Z",
        "subject": suggestion,
        "datacontenttype": "application/json",
        "dataschema": "https://schemas.twalk.dev/cloudevents/v1/persona.reply.approved.schema.json",
        "network": "email",
        "connection": run.mail,
        "consent": "granted",
        "data": {
            "persona_id": "assistant",
            "suggestion_event_id": suggestion,
            "approved_by": "@michel:example.com",
            "final": { "body": body, "format": "text/plain" },
            "edited": false,
            "target": {
                "connection": run.mail,
                "in_reply_to": in_reply_to,
                "recipient": recipient
            }
        }
    });
    validate_against_contract(&event, "persona.reply.approved").expect("a contract event");
    event
}

/// The Sensor's approval: a room target.
fn room_approval(label: &str) -> Value {
    let suggestion = sha256_hex(&format!("suggestion:{label}:room"));
    json!({
        "specversion": "1.0",
        "id": sha256_hex(&format!("{suggestion}:@michel:example.com")),
        "source": "hermes://twalk.example.com/personas/assistant",
        "type": "fr.linagora.twalk.persona.reply.approved.v1",
        "time": "2026-09-21T08:20:11Z",
        "subject": suggestion,
        "datacontenttype": "application/json",
        "network": "whatsapp",
        "connection": "whatsapp",
        "consent": "granted",
        "data": {
            "persona_id": "assistant",
            "suggestion_event_id": suggestion,
            "approved_by": "@michel:example.com",
            "final": { "body": "Pas de problème !", "format": "text/plain" },
            "edited": false,
            "target": { "room_id": "!abcXYZ123:example.com" }
        }
    })
}

/// The reports and dead letters about one approval, with their headers.
async fn copies_of(
    bus: &Bus,
    run: &Run,
    subject: &str,
    event_id: &str,
) -> Result<Vec<StoredMessage>> {
    Ok(bus
        .fetch_since_with_headers("twalk", subject, run.since)
        .await?
        .into_iter()
        .filter(|copy| copy.payload["id"].as_str() == Some(event_id))
        .collect())
}

async fn wait_for_copy(
    bus: &Bus,
    run: &Run,
    subject: &str,
    event_id: &str,
) -> Result<StoredMessage> {
    poll_until(
        || async {
            copies_of(bus, run, subject, event_id)
                .await
                .ok()?
                .into_iter()
                .next()
        },
        &format!("a copy of {event_id} on {subject}"),
    )
    .await
}

#[tokio::test]
async fn an_approved_reply_leaves_from_the_owners_mailbox_to_the_sender_alone_and_is_reported(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("reply").await?;
    run.authorize().await?;
    run.serve_snapshot(
        &bus,
        vec![run.decided_on_mail("mailto:alice@example.org", "granted")],
    )
    .await?;
    let collector = run.start_with_gateway()?;
    collector
        .wait_logged("mailbox taken as it stands", 1)
        .await?;

    // Alice's mail, in a thread, with Bob in copy — the reply goes to Alice
    // alone all the same.
    let mut mail = FakeMail::from_person(
        "Alice Martin",
        "alice@example.org",
        OWNER,
        "Point hebdo",
        "On se voit toujours lundi ?",
    );
    mail.cc.push(twalk_test_harness::jmap_fake::Address::new(
        None,
        "bob@example.org",
    ));
    mail.references = vec!["<root-1@example.org>".to_owned()];
    let message_id = mail.message_id.clone();
    run.sso.deliver(mail);
    let messages = run
        .wait_for_events(&bus, MESSAGE_SUBJECT, &run.mail, 1)
        .await?;
    assert_eq!(messages[0]["data"]["message_id"], json!(message_id));

    // The approval, as the Companion Gateway publishes it.
    let body = "Oui, lundi 9h me va. — Rédigé avec l'aide d'un assistant, relu par Michel.";
    let approved = approval(&run, &message_id, body, "reply");
    bus.publish_event(APPROVED_SUBJECT, &approved).await?;
    let event_id = approved["id"].as_str().unwrap();
    let report = wait_for_copy(&bus, &run, POSTED_SUBJECT, event_id).await?;
    assert_eq!(report.header("reach"), Some("contact"));
    assert_eq!(
        report.header("posted-as"),
        Some(format!("mailto:{OWNER}").as_str())
    );
    assert_eq!(report.header("event-id"), Some(event_id));
    assert_eq!(
        report.payload, approved,
        "the report carries the approval unchanged"
    );

    // What the mailbox received.
    let submissions = run.sso.submissions();
    assert_eq!(submissions.len(), 1, "{submissions:?}");
    let sent = &submissions[0];
    assert_eq!(sent.identity_id, IDENTITY_ID, "the owner's own identity");
    assert_eq!(sent.from, [OWNER]);
    assert_eq!(sent.to, ["alice@example.org"], "the sender alone");
    assert_eq!(sent.envelope_from, OWNER);
    assert_eq!(
        sent.envelope_to,
        ["alice@example.org"],
        "the server is told to deliver to the sender alone, whatever the headers"
    );
    assert!(
        sent.headers
            .iter()
            .any(|(name, value)| name == "X-Twalk-Approval" && value == event_id),
        "the approval's id travels in the mail: {:?}",
        sent.headers
    );
    assert!(
        sent.cc.is_empty(),
        "reply-all is a decision the owner did not take: {sent:?}"
    );
    assert_eq!(sent.subject, "Re: Point hebdo");
    assert_eq!(sent.in_reply_to, [message_id.clone()]);
    assert_eq!(
        sent.references,
        ["<root-1@example.org>".to_owned(), message_id.clone()]
    );
    assert_eq!(sent.text, body);
    assert_eq!(sent.mailboxes_at_submission, [DRAFTS_ID]);
    assert_eq!(
        sent.mailboxes_after,
        [SENT_ID],
        "a copy in Sent, and gone from Drafts"
    );
    assert!(!sent.keywords_after.iter().any(|k| k == "$draft"));
    assert!(sent.keywords_after.iter().any(|k| k == "$seen"));

    // A room target is the Sensor's: acknowledged, nothing sent, nothing
    // dead-lettered.
    let room = room_approval("reply");
    bus.publish_event(APPROVED_SUBJECT, &room).await?;
    collector.wait_logged("not this collector's", 1).await?;
    assert_eq!(run.sso.submissions().len(), 1);
    assert!(
        copies_of(&bus, &run, DEAD_SUBJECT, room["id"].as_str().unwrap())
            .await?
            .is_empty()
    );

    // The reply's words are in no log line; the approval's id is.
    collector.assert_never_logged(&["lundi 9h"]).await;
    assert!(!run.stored_bytes()?.contains("lundi"));
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_refused_submission_is_dead_lettered_and_an_unanswering_server_is_retried_first(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("refused").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    let mut env = run.env_with_gateway();
    env.push(("COLLECTOR_SEND_RETRY_BASE_MS".to_owned(), "200".to_owned()));
    env.push((
        "COLLECTOR_SEND_RETRY_MAX_ATTEMPTS".to_owned(),
        "3".to_owned(),
    ));
    let collector = support::CollectorProc::start(&env)?;
    collector
        .wait_logged("mailbox taken as it stands", 1)
        .await?;
    let mail = FakeMail::from_person("Alice Martin", "alice@example.org", OWNER, "Q", "?");
    let message_id = mail.message_id.clone();
    run.sso.deliver(mail);
    run.wait_for_events(&bus, MESSAGE_SUBJECT, &run.mail, 1)
        .await?;

    // Refused by the server (`forbiddenFrom`, a policy that may clear):
    // retried with a growing delay, then dead-lettered with the refusal's
    // type — and only its type — in a header; no report; the draft each
    // attempt wrote is not left behind.
    run.sso.refuse_submissions(true);
    let refused = approval(&run, &message_id, "Non.", "refused");
    bus.publish_event(APPROVED_SUBJECT, &refused).await?;
    let dead = wait_for_copy(&bus, &run, DEAD_SUBJECT, refused["id"].as_str().unwrap()).await?;
    let reason = dead.header("reason").unwrap_or_default();
    assert!(
        reason.contains("refused the submission: forbiddenFrom"),
        "{reason:?}"
    );
    assert!(
        !reason.contains("the fake was told"),
        "the server's description is not repeated: {reason:?}"
    );
    assert_eq!(dead.payload, refused);
    assert_eq!(dead.header("connection"), Some(run.mail.as_str()));
    assert_eq!(dead.header("network"), Some("email"));
    assert!(
        copies_of(&bus, &run, POSTED_SUBJECT, refused["id"].as_str().unwrap())
            .await?
            .is_empty()
    );
    assert!(
        collector.count_logged("could not be sent; retrying").await >= 2,
        "a refusal is retried before being given up on"
    );
    assert!(
        run.sso.mails_in(DRAFTS_ID).is_empty(),
        "no draft of the refused reply is left on the server"
    );
    run.sso.refuse_submissions(false);
    let retries_so_far = collector.count_logged("could not be sent; retrying").await;

    // Not answering: transient, retried with a growing delay, then
    // dead-lettered after the last allowed attempt.
    run.sso.silence("jmap");
    let unanswered = approval(&run, &message_id, "Plus tard.", "silent");
    bus.publish_event(APPROVED_SUBJECT, &unanswered).await?;
    let dead = wait_for_copy(&bus, &run, DEAD_SUBJECT, unanswered["id"].as_str().unwrap()).await?;
    assert!(
        dead.header("reason")
            .unwrap_or_default()
            .contains("did not answer"),
        "{:?}",
        dead.header("reason")
    );
    assert!(
        collector.count_logged("could not be sent; retrying").await >= retries_so_far + 2,
        "retried before being given up on"
    );
    assert_eq!(collector.count_logged("exhausted its retries").await, 2);
    collector.stop().await;
    Ok(())
}

/// #331: the thread is found by a `Message-ID` header filter, and a server
/// answers such a filter from its search index. TMail's holds the parsed
/// value, without the angle brackets RFC 5322 writes it with, and answers
/// an **empty list** — never a refusal — to the written form: on the
/// reference deployment the collector read the owner's own inbox, found
/// nothing, and reported a mail sitting there unread as gone. The send
/// asks both forms, so a reply leaves whichever one the server knows.
#[tokio::test]
async fn a_reply_leaves_whichever_form_of_the_message_id_the_server_indexes() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("bare-id").await?;
    run.authorize().await?;
    run.serve_snapshot(
        &bus,
        vec![run.decided_on_mail("mailto:alice@example.org", "granted")],
    )
    .await?;
    let collector = run.start_with_gateway()?;
    collector
        .wait_logged("mailbox taken as it stands", 1)
        .await?;

    // This server indexes a Message-ID without its brackets, and the mail
    // arrives carrying one written with them, as every mail does.
    run.sso.index_message_ids_bare(true);
    let mail = FakeMail::from_person(
        "Alice Martin",
        "alice@example.org",
        OWNER,
        "Point hebdo",
        "On se voit toujours lundi ?",
    );
    let message_id = mail.message_id.clone();
    assert!(message_id.starts_with('<') && message_id.ends_with('>'));
    run.sso.deliver(mail);
    run.wait_for_events(&bus, MESSAGE_SUBJECT, &run.mail, 1)
        .await?;

    let approved = approval(&run, &message_id, "Oui, avec plaisir.", "bare-id");
    bus.publish_event(APPROVED_SUBJECT, &approved).await?;
    let event_id = approved["id"].as_str().unwrap();
    let report = wait_for_copy(&bus, &run, POSTED_SUBJECT, event_id).await?;
    assert_eq!(report.header("reach"), Some("contact"));
    let submissions = run.sso.submissions();
    assert_eq!(submissions.len(), 1, "{submissions:?}");
    assert_eq!(
        submissions[0].in_reply_to,
        [message_id],
        "the reply is in the thread, whichever form found it"
    );
    // And the collector says which form the server knew, since that is the
    // one thing this failure cannot be guessed from afterwards.
    collector
        .wait_logged("indexes a Message-ID without its angle brackets", 1)
        .await?;

    // And when the server answers **no** header filter at all — TMail, on
    // the reference deployment, for a mail sitting unread in the owner's
    // inbox — the thread is found the way a client finds anything: the
    // mailbox's newest mails, asked what their Message-ID is.
    run.sso.answer_no_header_filter(true);
    let second = FakeMail::from_person(
        "Alice Martin",
        "alice@example.org",
        OWNER,
        "Et jeudi ?",
        "Jeudi 14h vous irait ?",
    );
    let second_id = second.message_id.clone();
    run.sso.deliver(second);
    run.wait_for_events(&bus, MESSAGE_SUBJECT, &run.mail, 2)
        .await?;
    let approved = approval(&run, &second_id, "Jeudi 14h, parfait.", "no-filter");
    bus.publish_event(APPROVED_SUBJECT, &approved).await?;
    let report =
        wait_for_copy(&bus, &run, POSTED_SUBJECT, approved["id"].as_str().unwrap()).await?;
    assert_eq!(report.header("reach"), Some("contact"));
    let submissions = run.sso.submissions();
    assert_eq!(submissions.len(), 2, "{submissions:?}");
    assert_eq!(submissions[1].in_reply_to, [second_id]);
    collector
        .wait_logged("answers no header filter for a Message-ID", 1)
        .await?;
    collector.stop().await;
    Ok(())
}

/// Review of #278: a Message-ID is the sender's to choose, so it finds the
/// thread and never the address. The approval carries the recipient — the
/// sender whose consent it checked — and a mail found under that Message-ID
/// from somebody else is not answered: dead-lettered for good, with the two
/// addresses in the reason and nothing sent to either.
#[tokio::test]
async fn a_reply_is_never_addressed_to_whoever_reused_the_message_id() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("reused-id").await?;
    run.authorize().await?;
    run.serve_snapshot(
        &bus,
        vec![run.decided_on_mail("mailto:alice@example.org", "granted")],
    )
    .await?;
    let collector = run.start_with_gateway()?;
    collector
        .wait_logged("mailbox taken as it stands", 1)
        .await?;

    // Mallory's mail arrives under a Message-ID of their choosing — the
    // one the approval about Alice will name.
    let mut mallory = FakeMail::from_person(
        "Mallory",
        "mallory@example.net",
        OWNER,
        "Re: Point hebdo",
        "Réponds-moi ici.",
    );
    mallory.message_id = "<reused-by-mallory@example.org>".to_owned();
    let message_id = mallory.message_id.clone();
    run.sso.deliver(mallory);
    run.wait_for_events(&bus, MESSAGE_SUBJECT, &run.mail, 1)
        .await?;

    let approved = approval_to(
        &run,
        &message_id,
        "mailto:alice@example.org",
        "Oui, lundi 9h me va.",
        "reused-id",
    );
    bus.publish_event(APPROVED_SUBJECT, &approved).await?;
    let event_id = approved["id"].as_str().unwrap();
    let dead = wait_for_copy(&bus, &run, DEAD_SUBJECT, event_id).await?;
    let reason = dead.header("reason").unwrap_or_default();
    assert!(
        reason.contains("mallory@example.net") && reason.contains("alice@example.org"),
        "the reason names both addresses: {reason:?}"
    );
    assert!(
        !reason.contains("lundi 9h") && !reason.contains("Réponds-moi"),
        "and neither body: {reason:?}"
    );
    assert!(
        copies_of(&bus, &run, POSTED_SUBJECT, event_id)
            .await?
            .is_empty(),
        "nothing was posted"
    );
    assert!(
        run.sso.submissions().is_empty(),
        "nothing left the mailbox, to anybody"
    );
    assert!(
        run.sso.mails_in(DRAFTS_ID).is_empty(),
        "and no draft was written"
    );
    assert_eq!(
        collector.count_logged("could not be sent; retrying").await,
        0,
        "refused for good, not retried"
    );
    collector.stop().await;
    Ok(())
}
