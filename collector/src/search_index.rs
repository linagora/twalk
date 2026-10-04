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

#[cfg(test)]
mod tests {
    use super::*;

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
