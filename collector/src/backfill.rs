//! Le backfill de l'archive (#XXX, lot 3a) : remplit l'index depuis les
//! mails que le collecteur lit, **sans jamais écrire sur le bus** (spec §2,
//! ADR 0037). Un backfill de vingt-cinq ans qui publierait ferait déborder
//! le stream de 2 GiB et noierait tous les consumers.
//!
//! Le curseur ne bouge qu'après le commit de l'index, exactement comme le
//! curseur JMAP ne bouge qu'après que le bus a pris les événements : un
//! crash entre les deux fait réindexer, jamais perdre — et le dédoublonnage
//! par `id` (§3.3) fait de la réindexation un non-événement. Si le commit
//! échoue, le curseur n'avance **pas** : un id vu mais non commité n'est
//! jamais dit « vu », sans quoi le prochain passage le croirait indexé et le
//! perdrait (§8.3).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::owner::Owner;
use crate::search_index::Stored;
use crate::source::{MailSource, Native, Source};

/// Où le backfill en est : les ids déjà commités, pour ne pas les repasser.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Cursor {
    /// Les ids de documents déjà commités. Une liste, pas un seul id : le
    /// serveur peut ne pas rendre dans un ordre stable, et c'est l'id qui dit
    /// « déjà fait », pas une position.
    pub seen_ids: Vec<String>,
}

impl Cursor {
    pub fn read(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("the cursor {} is not readable JSON", path.display())),
            Err(_) => Ok(Self::default()),
        }
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        crate::fs::write_json_private(path, self)
    }
}

/// Un passage de backfill : indexe les mails non encore vus, commit, écrit le
/// curseur, rend combien de documents ont été écrits.
///
/// `connection` est le nom de connexion (registre, clé de consentement) ;
/// `account` est l'id de compte JMAP — deux chaînes distinctes, et l'id du bus
/// dépend de la seconde (§3.3). `owner` est passé par l'appelant : c'est le
/// `MailSource` qui pose la frontière, et l'owner n'est jamais un contact
/// (ADR 0021).
pub fn run_once(
    connection: &str,
    account: &str,
    owner: &Owner,
    index_dir: &Path,
    cursor_path: &Path,
    mails: &[crate::jmap::Mail],
) -> Result<usize> {
    let cursor = Cursor::read(cursor_path)?;
    let mut seen: BTreeSet<String> = cursor.seen_ids.iter().cloned().collect();

    let source = MailSource::new(connection.to_owned(), account.to_owned(), owner.clone());
    let stored = Stored::open_or_create(index_dir)?;
    let mut writer = stored.writer()?;

    let mut written = 0usize;
    let mut newly_seen: Vec<String> = Vec::new();
    for mail in mails {
        let Some(document) = source.document(&Native::Mail(mail.clone())) else {
            continue;
        };
        if seen.contains(&document.id) {
            continue;
        }
        // Un ajout refusé n'est pas dit « vu » : rien n'a été commité, et
        // rendre une erreur fait que le curseur n'avance pas — le prochain
        // passage reprend ce mail.
        writer.add(&document).with_context(|| {
            format!(
                "the document {} could not be added to the index; the cursor is not advanced",
                document.id
            )
        })?;
        newly_seen.push(document.id.clone());
        written += 1;
    }

    if written > 0 {
        writer.commit().context("the index could not be committed")?;
        // Le curseur n'avance **qu'après** le commit (§3.2, §8.3).
        for id in newly_seen {
            seen.insert(id);
        }
        Cursor {
            seen_ids: seen.into_iter().collect(),
        }
        .write(cursor_path)?;
    }
    Ok(written)
}

/// Indexe maintenant les mails qu'un poll vient de lire — pas de curseur : le
/// dédoublonnage par `id` (T3, `Writer::add`) rend la ré-indexation
/// inoffensive, et le temps réel n'a pas de « position » à retenir, seulement
/// des mails à écrire. Le backfill (`run_once`) reste seul à tenir un curseur.
///
/// Le `Stored` est fourni par l'appelant (C24) : celui du processus, tenu
/// ouvert pour sa vie, et non un `Stored` par poll — ouvrir un index à chaque
/// tour serait coûteux. **N'écrit rien sur le bus** : indexer est une écriture
/// locale, et ADR 0037 (rétention de 90 jours / 2 GiB) en ferait un défaut.
pub fn index_mails(
    stored: &Stored,
    connection: &str,
    account: &str,
    owner: &Owner,
    mails: &[crate::jmap::Mail],
) -> Result<usize> {
    if mails.is_empty() {
        return Ok(0);
    }
    let source = MailSource::new(connection.to_owned(), account.to_owned(), owner.clone());
    let mut writer = stored.writer()?;
    let mut written = 0usize;
    for mail in mails {
        let Some(document) = source.document(&Native::Mail(mail.clone())) else {
            continue;
        };
        writer
            .add(&document)
            .with_context(|| format!("the document {} could not be indexed", document.id))?;
        written += 1;
    }
    writer.commit().context("the index could not be committed")?;
    // Le lecteur est rechargé tout de suite : la jauge que l'appelant lit
    // après ce poll (`document_count`) doit refléter ce commit, et la
    // politique de rechargement par défaut de Tantivy a un délai de quelques
    // millisecondes.
    stored.reader_reload();
    Ok(written)
}
