//! Une source d'archive indexable (#XXX, lot 3a) : l'abstraction qui fait
//! que le courrier et la messagerie entreront par la même porte (spec §3.1).
//! Ce lot-ci n'en a qu'une, le courrier ; le trait est étroit pour que la
//! seconde ne rouvre pas la première.
//!
//! `document` rend un `Document` pour une unité native — un mail — ou `None`
//! pour ce qui ne s'indexe pas : le mail de l'owner lui-même (ADR 0021), un
//! mail non humain (la frontière de `jmap::frontier`, réutilisée). `embedded`
//! est le texte extractible d'une pièce jointe : `None` tant que le lot 3d
//! n'est pas là.

use crate::jmap::{Attachment, Mail};
use crate::owner::Owner;

/// Une unité native d'une source, prête à devenir un document — ou à être
/// écartée.
pub enum Native {
    Mail(Mail),
}

/// Le document indexé (spec §3.2). Les champs sont ceux du schéma de
/// `search_index::Schema`, dans le même ordre, pour que l'écriture et la
/// lecture ne puissent pas diverger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: String,
    pub source: String,
    pub correspondent: String,
    pub mailbox: Option<String>,
    pub thread: Option<String>,
    pub date: i64,
    pub subject: String,
    pub body: String,
    pub has_attachment: bool,
}

/// Une source d'archive interrogeable.
pub trait Source {
    /// L'identifiant de connexion, tel que le registre du Gateway le nomme
    /// (`COLLECTOR_MAIL_CONNECTION`). C'est aussi la clé de consentement.
    fn id(&self) -> &str;

    /// L'identifiant du compte natif — le `accountId` JMAP — sous lequel la
    /// source range ses unités. L'id du bus en dépend, et le backfill en a
    /// besoin pour bâtir un document à partir d'un `jmap::Mail` sans passer
    /// par le nom de la connexion.
    fn account(&self) -> &str;

    /// Le document d'une unité native, ou `None` si elle ne s'indexe pas.
    fn document(&self, native: &Native) -> Option<Document>;

    /// Le texte extractible d'une pièce jointe, s'il y en a un. `None` par
    /// défaut : hors périmètre du lot 3a (spec §8.4).
    fn embedded(&self, _attachment: &Attachment) -> Option<String> {
        None
    }
}

/// Le courrier de l'owner : un `Document` par mail d'un tiers.
pub struct MailSource {
    connection: String,
    account_id: String,
    owner: Owner,
}

impl MailSource {
    pub fn new(connection: String, account_id: String, owner: Owner) -> Self {
        Self {
            connection,
            account_id,
            owner,
        }
    }
}

impl Source for MailSource {
    fn id(&self) -> &str {
        &self.connection
    }

    fn account(&self) -> &str {
        &self.account_id
    }

    fn document(&self, native: &Native) -> Option<Document> {
        let Native::Mail(mail) = native;
        // La frontière est celle du bus, une seule fois (`jmap::frontier`) :
        // elle écarte déjà le mail de l'owner (ADR 0021), l'invitation iTIP
        // et le signal non humain (`Auto-Submitted`, `List-Id`, …). L'index
        // et le bus posent donc la même frontière, et un `Err` n'est pas un
        // document.
        crate::jmap::frontier(mail, &self.owner).ok()?;
        Some(Document {
            id: mail_event_id(&self.account_id, &mail.id),
            source: self.connection.clone(),
            correspondent: mail.from.mailto(),
            mailbox: mail.mailbox_ids.first().cloned(),
            thread: mail.references.first().cloned(),
            date: parse_rfc3339_seconds(&mail.received_at).unwrap_or_default(),
            subject: mail.subject.clone(),
            body: mail.body.clone(),
            has_attachment: !mail.attachments.is_empty(),
        })
    }
}

/// L'id déterministe d'un mail — **le même que celui du bus** (§3.3), pour
/// qu'un mail vu par le push et repassé par le backfill soit un document et
/// non deux. C'est `sha256("jmap:" + accountId + ":" + emailId)` ; la
/// fonction est ici pour être appelée par le backfill comme par le push.
pub fn mail_event_id(account: &str, email_id: &str) -> String {
    crate::status::sha256_hex(&format!("jmap:{account}:{email_id}"))
}

/// La date d'un `received_at` RFC 3339 en secondes depuis l'époque.
fn parse_rfc3339_seconds(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jmap::{Mail, Person};

    fn person(name: &str, email: &str) -> Person {
        Person {
            name: Some(name.to_owned()),
            email: email.to_owned(),
        }
    }

    fn a_mail() -> Mail {
        Mail {
            id: "jmap-id-1".to_owned(),
            mailbox_ids: vec!["inbox".to_owned()],
            received_at: "2026-09-01T10:00:00Z".to_owned(),
            from: person("Alice", "alice@example.org"),
            to: vec![person("Owner", "owner@linagora.com")],
            cc: vec![],
            subject: "Point hebdo".to_owned(),
            body: "Le point de la semaine.".to_owned(),
            message_id: Some("<m1@example.org>".to_owned()),
            in_reply_to: None,
            references: vec!["<root@example.org>".to_owned()],
            attachments: vec![],
            auto_submitted: None,
            list_id: None,
            list_unsubscribe: None,
            precedence: None,
            has_itip_part: false,
        }
    }

    /// Un mail d'un tiers devient un document : l'id du bus, la source, le
    /// correspondant en `mailto:`, la date, le sujet, le corps, le fil.
    #[test]
    fn a_third_party_mail_becomes_a_document() {
        let owner = crate::owner::Owner::new("owner@linagora.com", Vec::<String>::new());
        let source = MailSource::new("mail-linagora".to_owned(), "acct".to_owned(), owner);
        let document = source
            .document(&Native::Mail(a_mail()))
            .expect("a third party's mail is indexed");
        assert_eq!(
            document.id,
            crate::source::mail_event_id("acct", "jmap-id-1"),
            "the id is the bus's own deterministic id"
        );
        assert_eq!(document.source, "mail-linagora");
        assert_eq!(document.correspondent, "mailto:alice@example.org");
        assert_eq!(document.subject, "Point hebdo");
        assert_eq!(document.body, "Le point de la semaine.");
        assert_eq!(document.thread.as_deref(), Some("<root@example.org>"));
        assert!(!document.has_attachment);
    }

    /// Le mail de l'owner lui-même n'est pas un tiers : pas de document
    /// (ADR 0021, l'owner n'est jamais un contact).
    #[test]
    fn the_owners_own_mail_is_not_indexed() {
        let owner = crate::owner::Owner::new("owner@linagora.com", Vec::<String>::new());
        let source = MailSource::new("mail-linagora".to_owned(), "acct".to_owned(), owner);
        let mut mail = a_mail();
        mail.from = person("Owner", "owner@linagora.com");
        assert!(source.document(&Native::Mail(mail)).is_none());
    }

    /// Un mail non humain (`Auto-Submitted`, `List-Id`) n'est pas indexé —
    /// la même frontière que `mails.rs`, réutilisée et non réécrite.
    #[test]
    fn a_non_human_mail_is_not_indexed() {
        let owner = crate::owner::Owner::new("owner@linagora.com", Vec::<String>::new());
        let source = MailSource::new("mail-linagora".to_owned(), "acct".to_owned(), owner);
        let mut mail = a_mail();
        mail.list_id = Some("<list.example.org>".to_owned());
        assert!(source.document(&Native::Mail(mail)).is_none());
    }

    /// Une pièce jointe ne produit aucun texte en 3a (lot 3d).
    #[test]
    fn attachments_yield_no_text_in_this_lot() {
        let owner = crate::owner::Owner::new("owner@linagora.com", Vec::<String>::new());
        let source = MailSource::new("mail-linagora".to_owned(), "acct".to_owned(), owner);
        let attachment = crate::jmap::Attachment {
            kind: "document",
            mime_type: "application/pdf".to_owned(),
            size_bytes: 1024,
        };
        assert!(source.embedded(&attachment).is_none());
    }
}
