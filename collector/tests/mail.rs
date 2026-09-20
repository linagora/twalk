//! The owner's mailbox at the process boundary (issue #276): the collector
//! started against the fake SSO, the fake JMAP server and the test stack's
//! bus — what the INBOX already held is taken as the state and not
//! published; a mail delivered afterwards is `inbound.message.received.v1`
//! on the mail connection, field by field; a newsletter, an iTIP mail and
//! the owner's own mail are dropped and counted; a mail in Sent or Archive
//! is never read; a granted sender's mail carries `granted`, a revoked
//! sender's carries no words — and a filename reaches nothing.

mod support;

use anyhow::Result;
use serde_json::{json, Value};
use support::{sha256_hex, Run, OWNER};
use twalk_test_harness::jmap_fake::{Address, FakeMail, ACCOUNT_ID, ARCHIVE_ID, INBOX_ID, SENT_ID};
use twalk_test_harness::{ensure_stack, validate_against_contract, Bus};

const FILENAME: &str = "ordre-du-jour-confidentiel.pdf";

const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";

/// The messages this run's mail connection published, in order.
async fn messages_of(bus: &Bus, run: &Run) -> Result<Vec<Value>> {
    run.events_of(bus, MESSAGE_SUBJECT, &run.mail).await
}

async fn wait_for_messages(bus: &Bus, run: &Run, at_least: usize) -> Result<Vec<Value>> {
    run.wait_for_events(bus, MESSAGE_SUBJECT, &run.mail, at_least)
        .await
}

#[tokio::test]
async fn a_mail_delivered_after_the_start_is_the_message_and_what_was_there_before_is_not(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("mail").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    // Already there when the collector starts: the past, not published.
    run.sso.deliver(FakeMail::from_person(
        "Old Friend",
        "old@example.org",
        OWNER,
        "From last week",
        "This was here before.",
    ));
    let collector = run.start_with_gateway()?;
    collector
        .wait_logged("mailbox taken as it stands", 1)
        .await?;
    collector.wait_logged("mailbox polled", 2).await?;
    assert!(messages_of(&bus, &run).await?.is_empty());

    // Alice writes, with an attachment, in a thread.
    let mut mail = FakeMail::from_person(
        "Alice Martin",
        "Alice@Example.org",
        OWNER,
        "Re: Point hebdo",
        "Bonjour Michel,\n\nOn se voit toujours lundi ?\n\nAlice",
    )
    .attachment(FILENAME, "application/pdf", 48213);
    mail.in_reply_to = Some("<7a1e0c2b@example.com>".to_owned());
    mail.references = vec![
        "<c9d8e7f6@example.org>".to_owned(),
        "<7a1e0c2b@example.com>".to_owned(),
    ];
    let id = run.sso.deliver(mail);
    let messages = wait_for_messages(&bus, &run, 1).await?;
    let message = &messages[0];
    validate_against_contract(message, "inbound.message.received")?;
    assert_eq!(
        message["id"],
        sha256_hex(&format!("jmap:{ACCOUNT_ID}:{id}"))
    );
    assert_eq!(
        message["subject"], "mailto:alice@example.org",
        "lower-cased"
    );
    assert_eq!(
        message["source"],
        format!(
            "jmap://{}/{ACCOUNT_ID}/{INBOX_ID}",
            run.sso.issuer().trim_start_matches("http://")
        )
    );
    assert_eq!(message["network"], "email");
    assert_eq!(message["connection"], run.mail);
    assert_eq!(message["consent"], "pending", "nobody decided about Alice");
    assert_eq!(
        message["data"]["body"],
        "Bonjour Michel,\n\nOn se voit toujours lundi ?\n\nAlice"
    );
    assert_eq!(message["data"]["title"], "Re: Point hebdo");
    assert_eq!(message["data"]["audience"], "direct");
    assert_eq!(message["data"]["contact"]["display_name"], "Alice Martin");
    assert_eq!(
        message["data"]["reply_to"],
        json!({ "message_id": "<7a1e0c2b@example.com>" })
    );
    assert_eq!(message["data"]["thread_root"], "<c9d8e7f6@example.org>");
    assert_eq!(
        message["data"]["attachments"],
        json!([{ "kind": "file", "mime_type": "application/pdf", "size_bytes": 48213 }])
    );
    assert!(!message.to_string().contains(FILENAME), "{message}");

    // The frontier: a newsletter, an invitation and the owner's own mail
    // are not messages from a person, and each is counted as what it is.
    run.sso.deliver(
        FakeMail::from_person(
            "Weekly Digest",
            "news@list.example",
            OWNER,
            "This week",
            "Unsubscribe below",
        )
        .header("List-Unsubscribe", "<https://list.example/u>"),
    );
    run.sso.deliver(
        FakeMail::from_person(
            "Calendar",
            "carol@example.org",
            OWNER,
            "Invitation: sync",
            "You are invited",
        )
        .attachment("invite.ics", "text/calendar", 1200),
    );
    run.sso.deliver(FakeMail::from_person(
        "Me",
        OWNER,
        OWNER,
        "Note to self",
        "Buy milk",
    ));
    // And two mails the collector must not read at all.
    let sent = run.sso.deliver_to(
        SENT_ID,
        FakeMail::from_person(
            "Michel",
            OWNER,
            "alice@example.org",
            "Re: Re: Point hebdo",
            "Oui !",
        ),
    );
    let archived = run.sso.deliver_to(
        ARCHIVE_ID,
        FakeMail::from_person(
            "Dave",
            "dave@example.org",
            OWNER,
            "Old thread",
            "Filed away",
        ),
    );
    collector.wait_logged("\"non_human_sender\"", 1).await?;
    collector.wait_logged("\"calendar_invitation\"", 1).await?;
    collector.wait_logged("\"owner\"", 1).await?;
    // A cc'd mail is a group's.
    let mut group = FakeMail::from_person("Bob", "bob@example.org", OWNER, "Team", "All,");
    group.cc.push(Address::new(None, "alice@example.org"));
    run.sso.deliver(group);
    let messages = wait_for_messages(&bus, &run, 2).await?;
    assert_eq!(messages[1]["data"]["audience"], "group");
    assert_eq!(
        messages.len(),
        2,
        "the newsletter, the invitation, the owner's note, Sent and Archive published nothing: {messages:?}"
    );
    let read = run.sso.mails_read();
    assert!(read.contains(&id), "Alice's mail was read: {read:?}");
    assert!(
        !read.contains(&sent) && !read.contains(&archived),
        "a mail in Sent or Archive was read: {read:?}"
    );

    // Nobody's words — Alice's, the newsletter's, the note's — in the log
    // or on disk; the filename neither.
    collector
        .assert_never_logged(&[
            FILENAME,
            "On se voit toujours lundi",
            "Unsubscribe below",
            "Buy milk",
            "Point hebdo",
        ])
        .await;
    let stored = run.stored_bytes()?;
    assert!(!stored.contains(FILENAME) && !stored.contains("lundi") && !stored.contains("hebdo"));
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_granted_senders_mail_is_granted_and_a_revoked_senders_carries_no_words() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("decided").await?;
    run.authorize().await?;
    run.serve_snapshot(
        &bus,
        vec![
            run.decided_on_mail("mailto:alice@example.org", "granted"),
            run.decided_on_mail("mailto:bob@example.org", "revoked"),
        ],
    )
    .await?;
    let collector = run.start_with_gateway()?;
    collector
        .wait_logged("mailbox taken as it stands", 1)
        .await?;
    run.sso.deliver(FakeMail::from_person(
        "Alice Martin",
        "alice@example.org",
        OWNER,
        "Lundi ?",
        "On se voit lundi ?",
    ));
    run.sso.deliver(
        FakeMail::from_person(
            "Bob",
            "bob@example.org",
            OWNER,
            "Secret subject",
            "Secret words",
        )
        .attachment("secret.pdf", "application/pdf", 10),
    );
    let messages = wait_for_messages(&bus, &run, 2).await?;
    let alice = messages
        .iter()
        .find(|m| m["subject"] == "mailto:alice@example.org")
        .expect("Alice's mail");
    let bob = messages
        .iter()
        .find(|m| m["subject"] == "mailto:bob@example.org")
        .expect("Bob's mail");
    validate_against_contract(alice, "inbound.message.received")?;
    validate_against_contract(bob, "inbound.message.received")?;
    assert_eq!(alice["consent"], "granted");
    assert_eq!(alice["data"]["body"], "On se voit lundi ?");
    assert_eq!(bob["consent"], "revoked");
    assert!(bob["data"].get("body").is_none(), "{bob}");
    assert!(bob["data"].get("title").is_none(), "{bob}");
    assert_eq!(
        bob["data"]["attachments"],
        json!([{ "kind": "file", "mime_type": "application/pdf", "size_bytes": 10 }])
    );
    let text = bob.to_string();
    assert!(!text.contains("Secret"), "{bob}");
    collector
        .assert_never_logged(&["Secret words", "Secret subject", "secret.pdf"])
        .await;
    let stored = run.stored_bytes()?;
    assert!(
        !stored.contains("Secret") && !stored.contains("lundi"),
        "a mail's words reached the state directory"
    );
    collector.stop().await;
    Ok(())
}
