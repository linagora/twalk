//! Le backfill de la recherche (#XXX, lot 3a) : le moteur reprend à son
//! curseur et n'indexe jamais deux fois le même mail. Pur : `run_once` prend
//! une liste de mails et un chemin de curseur — le wiring temps réel est
//! testé ailleurs, à la frontière du processus.

use anyhow::Result;
use twalk_collector::jmap::{Mail, Person};

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
