//! Le backfill de la recherche (#XXX, lot 3a) : le moteur reprend à son
//! curseur et n'indexe jamais deux fois le même mail. Pur : `run_once` prend
//! une liste de mails et un chemin de curseur. Le câblage temps réel — l'index
//! qui se remplit à chaque poll — est testé ici même, à la frontière du
//! processus (`a_polled_mail_reaches_the_index`), avec la pile.

mod support;

use anyhow::Result;
use twalk_collector::jmap::{Mail, Person};
use twalk_test_harness::ensure_stack;

fn person(name: &str, email: &str) -> Person {
    Person {
        name: Some(name.to_owned()),
        email: email.to_owned(),
    }
}

fn a_mail(id: &str, from_email: &str) -> Mail {
    Mail {
        id: id.to_owned(),
        mailbox_ids: vec!["inbox".to_owned()],
        received_at: "2026-09-01T10:00:00Z".to_owned(),
        from: person("Alice", from_email),
        to: vec![person("Owner", "michel@example.com")],
        cc: vec![],
        subject: format!("Sujet {id}"),
        body: format!("Corps {id}"),
        message_id: Some(format!("<{id}@example.org>")),
        in_reply_to: None,
        references: vec![],
        attachments: vec![],
        auto_submitted: None,
        list_id: None,
        list_unsubscribe: None,
        precedence: None,
        has_itip_part: false,
    }
}

/// Un passage indexe deux mails, un second passage sur les mêmes mails n'écrit
/// rien : le curseur a retenu. Et l'index le confirme — deux documents, pas
/// quatre (§3.3, §8.3).
#[test]
fn the_backfill_resumes_from_its_cursor() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("twalk-backfill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let index_dir = dir.join("index");
    let cursor = index_dir.join("mail-linagora.cursor.json");
    let owner = twalk_collector::owner::Owner::new("michel@example.com", Vec::<String>::new());
    let mails = vec![
        a_mail("m1", "alice@example.org"),
        a_mail("m2", "bob@example.org"),
    ];

    let first = twalk_collector::backfill::run_once(
        "mail-linagora",
        "acct",
        &owner,
        &index_dir,
        &cursor,
        &mails,
    )?;
    assert_eq!(2, first, "the first pass indexes both mails");

    let second = twalk_collector::backfill::run_once(
        "mail-linagora",
        "acct",
        &owner,
        &index_dir,
        &cursor,
        &mails,
    )?;
    assert_eq!(0, second, "a resumed pass reindexes nothing: {second}");

    let stored = twalk_collector::search_index::Stored::open_or_create(&index_dir)?;
    assert_eq!(2, stored.document_count(), "two documents, not four");

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// L'indexation temps réel : le mail que le poll lit est dans l'index, sans
/// backfill. Preuve à la frontière, par la jauge `twalk_collector_index_documents`
/// (C25) — l'index n'a pas de route avant T6 —, et par le compte que
/// `Stored::document_count` donne sur l'index réellement écrit sous le
/// répertoire d'état du run.
#[tokio::test]
async fn a_polled_mail_reaches_the_index() -> Result<()> {
    ensure_stack().await?;
    let bus = twalk_test_harness::Bus::connect().await?;
    let run = support::Run::prepare("search-realtime").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    let (collector, metrics_port) = run.start_with_gateway_and_index()?;
    collector.wait_logged("search index opened", 1).await?;

    // Un mail arrive dans l'INBOX du propriétaire, d'un tiers.
    run.deliver_mail("Alice", "alice@example.org", "Sujet temps réel");

    // Le poll l'indexe ; la jauge le dit (elle vaut 1, pas 0).
    support::wait_metric(metrics_port, "twalk_collector_index_documents", 1.0).await?;

    // Et l'index sur disque le confirme, lu par un autre `Stored` que celui du
    // processus : le document est bien commité, pas seulement compté.
    let stored = twalk_collector::search_index::Stored::open_or_create(&run.index_dir())?;
    assert_eq!(
        1,
        stored.document_count(),
        "the polled mail was not committed to the index"
    );

    let _ = collector.stop().await;
    Ok(())
}
