//! `journal`: what went out, as which account, whether it reached anyone
//! — and never the text (ticket #265).
//!
//! The clerk journals the Sensor's report of a posted reply (#216): the
//! approval event republished unchanged on `….persona.reply.approved.v1.posted`
//! with the `reach` and `posted-as` headers. The suite plays the Sensor's
//! part, publishing that report with the headers the Sensor sets, and
//! reads the line back as the owner.
//!
//! Three endings, not two (#311). A reply the sender gave up on is
//! dead-lettered on `….persona.reply.approved.v1.dead` with the reason in
//! a header, and the journal carries a line for it too — "never sent",
//! with what stopped it. A channel that carried only the successes would
//! be the silence #216 was written against, one surface further out.
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
//! The same journey runs in French and in English
//! (`CLERK_USER_LANGUAGE`), so both catalogues are exercised at the
//! process boundary.
//!
//! **One report is one line, however often the bus delivers it.** The
//! clerk holds no store: it finds the line it already wrote by the `r` tag
//! every stream message carries, `twalk:event:<approval id>`, read back
//! off its own lines. The report is deliberately dated **twenty minutes in
//! the past**, because the simpler mechanism — giving the line the bus
//! event's `time` as its `created_at`, so a redelivery hashes to the same
//! Nostr id — fails exactly there: the relay refuses a `created_at` more
//! than fifteen minutes from its clock, and a redelivery after a relay
//! outage is that old. "One line" is proven by ordering, never a timer: a
//! later report's line shows the consumer went past the redelivery before
//! the first report's lines are counted.

mod harness;

use anyhow::Result;
use harness::{approval, in_seconds, line_references_event, Run};
use serde_json::json;

const OWNER: &str = "@owner:test.twalk";

/// Everything a journal line must and must not say about one report.
fn assert_journal_line(line: &str, approval_event: &serde_json::Value) {
    assert_journal_line_names_no_text(line, approval_event);
    assert!(
        line.contains(OWNER),
        "names the account it was posted as: {line}"
    );
}

/// What every journal line says, whichever way the reply ended: which
/// network, which approval — and never the reply's own words. A line about
/// a reply that never left names no account, because none posted it.
fn assert_journal_line_names_no_text(line: &str, approval_event: &serde_json::Value) {
    let id = approval_event["id"].as_str().unwrap();
    let body = approval_event["data"]["final"]["body"].as_str().unwrap();
    assert!(line.contains("WhatsApp"), "names the network: {line}");
    assert!(
        line.contains(&id[..12]),
        "names the approval by its first twelve characters: {line}"
    );
    assert!(
        !line.contains(body),
        "the reply's text is not in the journal: {line}"
    );
}

/// The journey in one language: a reply that reached the contact, then
/// one nobody received. `reached_words` and `nobody_words` are the
/// fragments of that language's two sentences.
async fn journal_in(
    language: &str,
    reached_words: &str,
    nobody_words: &str,
    never_sent_words: &str,
) -> Result<()> {
    let run = Run::start_in(&format!("journal-{language}"), language).await?;

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
        line.content.contains(reached_words),
        "a reply that reached the contact says so, in {language}: {}",
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
        line.content.contains(nobody_words) && line.content.contains("#123"),
        "a reply nobody received says so and names #123, in {language}: {}",
        line.content
    );

    // And a reply that never left at all (#311): the sender gave up, said
    // why in a header, and the journal says so rather than staying silent
    // — the silence that let an approval read as sent for ever.
    let never = approval(&run.id, 3)?;
    run.publish_dead_report(&never, Some("the JMAP server answered unknownMethod"))
        .await?;
    let line = run
        .wait_for_line(&run.channels.journal, &never["id"].as_str().unwrap()[..12])
        .await?;
    assert_journal_line_names_no_text(&line.content, &never);
    assert!(
        line.content.contains(never_sent_words)
            && line.content.contains("unknownMethod")
            && !line.content.contains(reached_words),
        "a reply that never left says so and says what stopped it, in {language}: {}",
        line.content
    );

    run.shutdown().await
}

#[tokio::test]
async fn a_posted_reply_is_journalled_without_its_text() -> Result<()> {
    journal_in("fr", "a atteint le contact", "Personne", "Non partie").await
}

#[tokio::test]
async fn the_journal_is_written_in_english_when_asked() -> Result<()> {
    journal_in("en", "reached the contact", "Nobody", "Never sent").await
}

#[tokio::test]
async fn a_redelivered_report_is_still_one_line() -> Result<()> {
    let run = Run::start("journal-redelivery").await?;

    // A report the bus delivers twice, dated past the relay's own window
    // on `created_at` (see the module docs): the line must be found, not
    // hashed.
    let mut reached = approval(&run.id, 1)?;
    reached["time"] = json!(in_seconds(-20 * 60));
    let id = reached["id"].as_str().unwrap().to_owned();
    run.publish_posted_report(&reached, "contact", OWNER)
        .await?;
    let line = run.wait_for_line(&run.channels.journal, &id[..12]).await?;
    assert_journal_line(&line.content, &reached);
    assert!(
        line_references_event(&line, &id),
        "the line is tagged with the bus event it was written for: {:?}",
        line.tags
    );

    // The same report again, then a later one: the later one's line is the
    // proof the consumer went past the redelivery.
    run.publish_posted_report_again(&reached, "contact", OWNER)
        .await?;
    let later = approval(&run.id, 2)?;
    run.publish_posted_report(&later, "contact", OWNER).await?;
    run.wait_for_line(&run.channels.journal, &later["id"].as_str().unwrap()[..12])
        .await?;

    let about_first: Vec<_> = run
        .lines_in(&run.channels.journal)
        .await?
        .into_iter()
        .filter(|line| line_references_event(line, &id))
        .collect();
    assert_eq!(
        about_first.len(),
        1,
        "a redelivered report is still one line: {about_first:?}"
    );
    run.assert_metric("twalk_clerk_skipped_total{why=\"duplicate\"} 1")
        .await?;
    run.assert_metric("twalk_clerk_posts_total{channel=\"journal\"} 2")
        .await?;

    run.shutdown().await
}
