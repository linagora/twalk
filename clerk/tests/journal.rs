//! `journal`: what went out, as which account, whether it reached anyone
//! — and never the text (ticket #265).
//!
//! The clerk journals the Sensor's report of a posted reply (#216): the
//! approval event republished unchanged on `….persona.reply.approved.v1.posted`
//! with the `reach` and `posted-as` headers. The suite plays the Sensor's
//! part, publishing that report with the headers the Sensor sets, and
//! reads the line back as the owner.
//!
//! Two things the line must carry and one it must not. The network, the
//! account the reply was posted as and the approval's id (its first twelve
//! characters, enough to find it in `GET /api/approvals/{id}`) are what
//! "did my reply go out?" needs. The reply's own text — `data.final.body`
//! — is what it must not carry: a journal is a list of what happened, not
//! a copy of what was said. And a report whose `reach` is `nobody` reads
//! as one, naming #123: a reply the bridge ignored is the failure that
//! looked like success from every other component.
//!
//! One case runs in English (`CLERK_USER_LANGUAGE=en`), so both catalogues
//! are exercised at the process boundary.

mod harness;

use anyhow::Result;
use harness::{approval, Run};

const OWNER: &str = "@owner:test.twalk";

/// Everything a journal line must and must not say about one report.
fn assert_journal_line(line: &str, approval_event: &serde_json::Value) {
    let id = approval_event["id"].as_str().unwrap();
    let body = approval_event["data"]["final"]["body"].as_str().unwrap();
    assert!(line.contains("WhatsApp"), "names the network: {line}");
    assert!(
        line.contains(OWNER),
        "names the account it was posted as: {line}"
    );
    assert!(
        line.contains(&id[..12]),
        "names the approval by its first twelve characters: {line}"
    );
    assert!(
        !line.contains(body),
        "the reply's text is not in the journal: {line}"
    );
}

#[tokio::test]
async fn a_posted_reply_is_journalled_without_its_text() -> Result<()> {
    let run = Run::start("journal-fr").await?;

    let reached = approval(&run.id, 1)?;
    run.publish_posted_report(&reached, "contact", OWNER)
        .await?;
    let line = run
        .wait_for_line(
            &run.channels.journal,
            &reached["id"].as_str().unwrap()[..12],
        )
        .await?;
    assert_journal_line(&line.content, &reached);
    assert!(
        line.content.contains("a atteint le contact"),
        "a reply that reached the contact says so, in French: {}",
        line.content
    );
    assert_eq!(line.pubkey.to_hex(), run.clerk_pubkey);

    let lines = run.lines_in(&run.channels.journal).await?;
    assert_eq!(lines.len(), 1, "one report, one line: {lines:?}");
    run.assert_metric("twalk_clerk_posts_total{channel=\"journal\"} 1")
        .await?;

    // A reply that reached nobody: its own kind of line, naming the
    // ticket that explains an account the bridge ignores.
    let unreached = approval(&run.id, 2)?;
    run.publish_posted_report(&unreached, "nobody", OWNER)
        .await?;
    let line = run
        .wait_for_line(
            &run.channels.journal,
            &unreached["id"].as_str().unwrap()[..12],
        )
        .await?;
    assert_journal_line(&line.content, &unreached);
    assert!(
        line.content.contains("Personne") && line.content.contains("#123"),
        "a reply nobody received says so and names #123: {}",
        line.content
    );

    run.shutdown().await
}

#[tokio::test]
async fn the_journal_is_written_in_english_when_asked() -> Result<()> {
    let run = Run::start_in("journal-en", "en").await?;

    let reached = approval(&run.id, 1)?;
    run.publish_posted_report(&reached, "contact", OWNER)
        .await?;
    let line = run
        .wait_for_line(
            &run.channels.journal,
            &reached["id"].as_str().unwrap()[..12],
        )
        .await?;
    assert_journal_line(&line.content, &reached);
    assert!(
        line.content.contains("reached the contact"),
        "in English: {}",
        line.content
    );

    let unreached = approval(&run.id, 2)?;
    run.publish_posted_report(&unreached, "nobody", OWNER)
        .await?;
    let line = run
        .wait_for_line(
            &run.channels.journal,
            &unreached["id"].as_str().unwrap()[..12],
        )
        .await?;
    assert_journal_line(&line.content, &unreached);
    assert!(
        line.content.contains("Nobody") && line.content.contains("#123"),
        "a reply nobody received says so and names #123: {}",
        line.content
    );

    run.shutdown().await
}
