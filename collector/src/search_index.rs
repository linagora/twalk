//! L'index de recherche de l'archive de l'owner (#XXX, lot 3a) : un index
//! Tantivy plein-texte, dans le répertoire d'état du collecteur, à côté des
//! curseurs JMAP et du grant OIDC. Ce que cet index détient est le plus long
//! détenteur de mots de tiers de tout Twalk — vingt-cinq ans de mails —
//! l'inverse de la règle du bus, qui en garde sept jours (ADR 0028). C'est
//! pourquoi il n'existe qu'avec une clé : sans `COLLECTOR_INDEX_KEY_FILE`,
//! `store` rend `None` et rien n'est écrit (spec §4.2).
//!
//! Tantivy n'a pas de support vectoriel (vérifié 0.20–0.26). La recherche
//! sémantique est un index ANN séparé, joint par l'`id` du document — lot 3b.

use std::path::Path;

use anyhow::{Context, Result};
use tantivy::schema::{Field, Schema as TantivySchema, INDEXED, STORED, STRING, TEXT};
use tantivy::Index;

use crate::source::Document;

/// Le schéma d'un document indexé (spec §3.2). Chaque champ est construit
/// ici et nulle part ailleurs : une facette ajoutée plus tard l'est à un
/// endroit, et `search_index::Schema` la voit.
pub struct Schema {
    pub schema: TantivySchema,
    pub id: Field,
    pub source: Field,
    pub correspondent: Field,
    pub mailbox: Field,
    pub thread: Field,
    pub date: Field,
    pub subject: Field,
    pub body: Field,
    pub has_attachment: Field,
    pub replied: Field,
}

impl Schema {
    pub fn build() -> Self {
        let mut builder = TantivySchema::builder();
        // `id` est STRING (mot exact, non tokenisé) : c'est la clé de
        // dédoublonnage et la jointure avec l'index vectoriel (§3.3, §6.2).
        let id = builder.add_text_field("id", STRING | STORED);
        // `source` est une facette : filtrer par réseau.
        let source = builder.add_text_field("source", STRING | STORED);
        // `correspondent` est les deux : mot exact (filtre « de Dupont ») ET
        // plein-texte (« mails mentionnant Dupont »). Jamais un nom
        // d'affichage (#110, ADR 0012).
        let correspondent = builder.add_text_field("correspondent", STRING | TEXT | STORED);
        let mailbox = builder.add_text_field("mailbox", STRING | STORED);
        let thread = builder.add_text_field("thread", STRING | STORED);
        // `date` en secondes : tri et plage.
        let date = builder.add_i64_field("date", INDEXED | STORED);
        let subject = builder.add_text_field("subject", TEXT | STORED);
        let body = builder.add_text_field("body", TEXT | STORED);
        let has_attachment = builder.add_bool_field("has_attachment", INDEXED | STORED);
        let replied = builder.add_bool_field("replied", INDEXED | STORED);
        Self {
            schema: builder.build(),
            id,
            source,
            correspondent,
            mailbox,
            thread,
            date,
            subject,
            body,
            has_attachment,
            replied,
        }
    }
}

/// L'index, s'il y en a un. `None` sans clé : l'index est une capacité
/// optionnelle, et son absence n'est pas une erreur — c'est un `503
/// index_not_configured` à la lecture (§4.2), jamais un refus de démarrage.
pub fn store(index_dir: &Path, key_file: Option<&Path>) -> Result<Option<Index>> {
    let Some(key_file) = key_file else {
        return Ok(None);
    };
    // La clé est lue pour prouver qu'elle existe et est lisible ; le
    // chiffrement au repos lui-même est le montage `gocryptfs` du
    // déploiement (spec §4.2–4.3), pas ce processus.
    std::fs::read(key_file)
        .with_context(|| format!("the index key {} cannot be read", key_file.display()))?;
    Ok(Some(open_or_create(index_dir)?))
}

/// Ouvre l'index s'il existe, le crée sinon — avec le schéma ci-dessus.
pub fn open_or_create(index_dir: &Path) -> Result<Index> {
    std::fs::create_dir_all(index_dir)
        .with_context(|| format!("failed to create {}", index_dir.display()))?;
    let schema = Schema::build();
    match Index::open_in_dir(index_dir) {
        Ok(index) => Ok(index),
        Err(_) => Index::create_in_dir(index_dir, schema.schema)
            .context("failed to create the search index"),
    }
}

/// L'index ouvert, son schéma et un lecteur : la moitié qu'on interroge.
pub struct Stored {
    index: Index,
    schema: Schema,
    reader: tantivy::IndexReader,
}

impl Stored {
    pub fn open_or_create(index_dir: &Path) -> Result<Self> {
        let index = open_or_create(index_dir)?;
        let schema = Schema::build();
        let reader = index
            .reader()
            .context("failed to open a reader on the index")?;
        Ok(Self {
            index,
            schema,
            reader,
        })
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Le nombre de documents vivants — ce que T6 sert sur `/index/status`.
    pub fn document_count(&self) -> usize {
        self.reader.searcher().num_docs() as usize
    }

    /// Un écrivain pour cette génération de l'index.
    pub fn writer(&self) -> Result<Writer<'_>> {
        let writer = self
            .index
            .writer(50_000_000)
            .context("failed to open an index writer")?;
        Ok(Writer {
            writer,
            schema: &self.schema,
        })
    }

    /// Recharge le lecteur après un commit (le `IndexReader` voit les
    /// nouveaux segments après `reload`).
    pub fn reader_reload(&self) {
        let _ = self.reader.reload();
    }
}

/// Écrit des documents, en remplaçant tout document de même `id`.
pub struct Writer<'a> {
    writer: tantivy::IndexWriter,
    schema: &'a Schema,
}

impl Writer<'_> {
    pub fn add(&mut self, document: &Document) -> Result<()> {
        // Tantivy n'a pas d'upsert : on supprime par `id` puis on ajoute,
        // dans le même commit. C'est ce qui fait qu'un mail vu deux fois
        // reste un document (§3.3).
        self.writer
            .delete_term(tantivy::Term::from_field_text(self.schema.id, &document.id));
        let mut doc = tantivy::TantivyDocument::default();
        doc.add_text(self.schema.id, &document.id);
        doc.add_text(self.schema.source, &document.source);
        doc.add_text(self.schema.correspondent, &document.correspondent);
        if let Some(mailbox) = &document.mailbox {
            doc.add_text(self.schema.mailbox, mailbox);
        }
        if let Some(thread) = &document.thread {
            doc.add_text(self.schema.thread, thread);
        }
        doc.add_i64(self.schema.date, document.date);
        doc.add_text(self.schema.subject, &document.subject);
        doc.add_text(self.schema.body, &document.body);
        doc.add_bool(self.schema.has_attachment, document.has_attachment);
        doc.add_bool(self.schema.replied, false);
        self.writer
            .add_document(doc)
            .context("failed to add a document to the index")?;
        Ok(())
    }

    pub fn commit(&mut self) -> Result<()> {
        self.writer
            .commit()
            .context("failed to commit the index")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_document() -> crate::source::Document {
        crate::source::Document {
            id: "jmap:acct:1".to_owned(),
            source: "mail-linagora".to_owned(),
            correspondent: "mailto:alice@example.org".to_owned(),
            mailbox: Some("inbox".to_owned()),
            thread: Some("<root@example.org>".to_owned()),
            date: 1_756_700_000,
            subject: "Point hebdo".to_owned(),
            body: "Le point de la semaine, déjà corrigé.".to_owned(),
            has_attachment: false,
        }
    }

    /// Le dédoublonnage par `id` (§3.3) : indexer deux fois le même mail —
    /// push puis backfill — ne fait pas deux documents. C'est la propriété
    /// que la Review Focus place en premier.
    ///
    /// L'assertion « le document survivant porte le sujet corrigé » (la
    /// propriété *replace*) vit dans T5, qui possède `search` (ruling C13) :
    /// ici on ne peut affirmer que le **compte**.
    #[test]
    fn indexing_the_same_id_twice_leaves_one_document() {
        let dir = std::env::temp_dir().join(format!("twalk-idx-dedup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let stored = Stored::open_or_create(&dir).expect("an index");
        let mut writer = stored.writer().expect("a writer");

        let mut document = sample_document();
        writer.add(&document).unwrap();
        document.subject = "Un sujet corrigé".to_owned();
        writer.add(&document).unwrap();
        writer.commit().unwrap();
        stored.reader_reload();

        assert_eq!(
            1,
            stored.document_count(),
            "the same id was indexed twice"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sans clé, il n'y a pas d'index : `store` rend `None`, aucun
    /// répertoire n'est créé. C'est la règle « désactivé par défaut » (spec
    /// §4.2) lue comme un test plutôt que comme un commentaire.
    #[test]
    fn no_key_means_no_index() {
        let dir = std::env::temp_dir().join(format!("twalk-idx-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = store(&dir, None).expect("a keyless store is not an error");
        assert!(store.is_none(), "an index was opened without a key");
        assert!(!dir.exists(), "a keyless store created a directory");
    }

    /// Avec une clé (ici un fichier présent), l'index s'ouvre ou se crée et
    /// les champs du document sont dans le schéma.
    #[test]
    fn a_key_opens_an_index_whose_schema_has_every_document_field() {
        let dir = std::env::temp_dir().join(format!("twalk-idx-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("index.key");
        std::fs::write(&key, b"test-only-key").unwrap();

        let store = store(&dir.join("index"), Some(&key))
            .expect("a keyed store opens")
            .expect("a keyed store is Some");
        let schema = store.schema();
        for field in [
            "id",
            "source",
            "correspondent",
            "mailbox",
            "thread",
            "date",
            "subject",
            "body",
            "has_attachment",
            "replied",
        ] {
            assert!(
                schema.get_field(field).is_ok(),
                "the schema has no {field} field"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
