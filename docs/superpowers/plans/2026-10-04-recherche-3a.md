# Recherche, lot 3a — Index Tantivy plein-texte, backfill reprenable, seam et écran

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** L'owner cherche dans ses mails par mots-clés, depuis le Companion, dans sa session ; un contact révoqué disparaît des résultats et le nombre de retraits est dit.

**Architecture:** L'index Tantivy vit dans le `collector`, à côté des autres états qu'il détient déjà (`COLLECTOR_STATE_DIR`). L'indexation temps réel réutilise la lecture que le poll fait déjà ; un backfill reprenable remplit l'archive **sans jamais écrire sur le bus**. Une route interne `GET /search` du collecteur, portée par le même bearer de service que la lecture du registre, est relayée par le Gateway sur `GET /api/search`, derrière le guard de session. Le filtre de consentement est appliqué **côté collecteur** et compté.

**Tech Stack:** Rust (`collector`, `companion-gateway`), crate `tantivy`, axum 0.8, reqwest, `twalk-consent-cache`, SvelteKit/TypeScript (`companion`), Playwright.

**Spec:** `docs/superpowers/specs/2026-10-04-recherche-design.md`

## Global Constraints

- **Aucun formateur.** Pas de Prettier, pas de rustfmt-on-save. Style Rust `cargo fmt` par défaut ; TS/Svelte : tabs, single quotes, wrap ~100 colonnes. Recopier le style du fichier voisin.
- **Pas de workspace Cargo.** `collector/` et `companion-gateway/` ont chacun leur lockfile et leur `target/`. Builder et tester depuis le répertoire du package.
- **Chaque test qui produit un événement valide via `validate_against_contract`** — mais 3a ne produit **aucun** événement nouveau sur le bus.
- **Isolation des tests** : sujets de bus, rooms, comptes uniques par run. Le stack de test est partagé entre suites parallèles (`TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT`, `TWALK_TEST_NATS_PORT`).
- **Jamais de secret committé.** Le jeton OpenRAG (`token-or.env`) et tout `.env` restent hors du dépôt.
- **Le backfill n'écrit jamais sur le bus** (ADR 0037 : le stream `twalk` garde 90 j / 2 GiB).
- **Aucune persona n'atteint l'index.** Pas de nouvelle variable dans `environment` de `hermes/`.
- **Vocabulaire `CONTEXT.md`** : *network*, *persona*, *the Companion* (la PWA seulement), *Companion Gateway* en toutes lettres.
- **`companion-gateway/openapi.yaml` est étendu** par toute route ajoutée : `tests/openapi.rs` échoue sur une route non décrite.
- **La chaîne de caractères française est relue par l'owner** (`fr.json`) — ne pas inventer de mot, reproduire les termes du produit.

## Review Focus

Les cinq classes d'entrée que la spec n'exerce pas par ses tests mais qui feront mal à l'usage — chacune avec son test dans la tâche qui possède le code :

1. **Un mail réindexé deux fois** (push puis backfill, ou backfill relancé) **ne produit jamais deux documents** — la recherche ne rend qu'un résultat par mail. → Tâche 3, test de dédoublonnage par `id`.
2. **Un `q` vide ou trop long, un `from > to`, un `limit` hors bornes** rendent une erreur structurée et ne font pas tomber le collecteur. → Tâche 5, tests de refus.
3. **Le collecteur redémarre au milieu d'un backfill** : il reprend au curseur, il ne repart pas de zéro et ne réindexe pas ce qu'il a déjà commité. → Tâche 4, test de reprise.
4. **Un mail d'un expéditeur `revoked`** ne rend ni snippet ni sujet, et le compte des retraits augmente — le silence est *dit*. → Tâche 4 + Tâche 6.
5. **Le Gateway ne lit jamais un corps de message** : la réponse du relais contient un snippet, jamais `body`. → Tâche 6, assertion de forme sur la réponse relayée.

---

### Task 1 : La dépendance Tantivy et l'ouverture de l'index

**Files:**
- Modify: `collector/Cargo.toml` (section `[dependencies]`)
- Create: `collector/src/search_index.rs`
- Modify: `collector/src/lib.rs` (déclarer `pub mod search_index;`)
- Modify: `collector/src/config.rs` (ajouter `index_dir`, `index_key_file`)

**Interfaces:**
- Consumes: `crate::config::Config` (existante), `crate::fs` (existante).
- Produces:
  - `search_index::Schema` — un `tantivy::schema::Schema` avec les champs du document (`id`, `source`, `correspondent`, `mailbox`, `thread`, `date`, `subject`, `body`, `has_attachment`, `replied`).
  - `search_index::store(index_dir: &Path, key_file: Option<&Path>) -> Result<Option<Index>>` — `None` quand aucune clé n'est configurée (index désactivé, §4.2).
  - `search_index::open_or_create(index_dir: &Path) -> Result<Index>`.
  - `Config::index_dir: PathBuf`, `Config::index_key_file: Option<PathBuf>`.

- [ ] **Step 1 : Ajouter la dépendance**

Dans `collector/Cargo.toml`, sous `[dependencies]`, après `tracing-subscriber` :

```toml
# L'index de recherche plein-texte de l'archive de l'owner (#XXX, lot 3a) :
# Tantivy, pur Rust, dans le répertoire d'état du collecteur. Sans support
# vectoriel (vérifié 0.20–0.26) — la moitié sémantique est un index ANN
# séparé, lot 3b.
tantivy = "0.22"
```

- [ ] **Step 2 : Écrire le test qui échoue — un index sans clé n'existe pas**

Dans `collector/src/search_index.rs`, à la fin :

```rust
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
```

- [ ] **Step 3 : Lancer le test pour le voir échouer**

Run: `cd collector && cargo test --lib search_index`
Expected: FAIL — `cannot find function 'store'`.

- [ ] **Step 4 : Écrire l'implémentation minimale**

En tête de `collector/src/search_index.rs` :

```rust
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
use tantivy::schema::{
    Field, Schema as TantivySchema, INDEXED, STORED, STRING, TEXT,
};
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
        let correspondent =
            builder.add_text_field("correspondent", STRING | TEXT | STORED);
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
```

- [ ] **Step 5 : Déclarer le module et la config**

Dans `collector/src/lib.rs`, à la liste alphabétique des `pub mod`, insérer :

```rust
pub mod search_index;
```

Dans `collector/src/config.rs`, ajouter deux champs à `pub struct Config` (après `http_listen`) :

```rust
    /// Le répertoire de l'index de recherche (`COLLECTOR_STATE_DIR/index`
    /// par défaut, #XXX). Vit sous le répertoire d'état, donc monté chiffré
    /// au déploiement (spec §4.2).
    pub index_dir: PathBuf,
    /// La clé de l'index (`COLLECTOR_INDEX_KEY_FILE`) : sans elle, aucun
    /// index n'est ouvert et `/search` répond `503 index_not_configured`
    /// (spec §4.2). Un fichier, jamais une variable — leçon #239.
    pub index_key_file: Option<PathBuf>,
```

Dans la construction de `Config` (là où `http_listen` est affecté, ~l.233), ajouter :

```rust
            index_dir: state_dir.join("index"),
            index_key_file: optional_string("COLLECTOR_INDEX_KEY_FILE").map(PathBuf::from),
```

- [ ] **Step 6 : Lancer les tests pour les voir passer**

Run: `cd collector && cargo test --lib search_index`
Expected: PASS — 2 tests.

- [ ] **Step 7 : Committer**

```bash
cd collector
git add Cargo.toml Cargo.lock src/search_index.rs src/lib.rs src/config.rs
git commit -m "The search index opens only with a key, and holds every document field

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 2 : Le trait `Source` et `MailSource`

**Files:**
- Create: `collector/src/source.rs`
- Modify: `collector/src/lib.rs` (`pub mod source;`)
- Test: `collector/src/source.rs` (module `tests` en fin de fichier)

**Interfaces:**
- Consumes: `crate::jmap::Mail` (existante : `id`, `from: Person`, `to`, `cc`, `received_at: String`, `subject: String`, `body: String`, `references: Vec<String>`, `attachments: Vec<Attachment>`), `crate::jmap::Person` (`.email`, `.mailto()`), `crate::owner::Owner` (`.mailtos()`, `.is_owner(&str)`).
- Produces:
  - `source::Document` (struct, champs exacts §3.2)
  - `source::Source` (trait : `fn id(&self) -> &str`, `fn document(&self, native: &Native) -> Option<Document>`, `fn embedded(&self, _a: &Attachment) -> Option<String>` avec défaut `None`)
  - `source::Native` (enum : `Mail(crate::jmap::Mail)`)
  - `source::MailSource` — implémente `Source`, `id()` = la connexion mail, rend un `Document` pour un mail d'un tiers non-`auto_submitted`/`List-Id` (le jugement « non humain » de `mails.rs` est réutilisé, pas réécrit).

- [ ] **Step 1 : Écrire le test qui échoue**

Fin de `collector/src/source.rs` :

```rust
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
        let owner = crate::owner::Owner::new("owner@linagora.com", []);
        let source = MailSource::new("mail-linagora".to_owned(), owner);
        let document = source
            .document(&Native::Mail(a_mail()))
            .expect("a third party's mail is indexed");
        assert_eq!(
            document.id,
            crate::jmap::mail_event_id("acct", "jmap-id-1"),
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
        let owner = crate::owner::Owner::new("owner@linagora.com", []);
        let source = MailSource::new("mail-linagora".to_owned(), owner);
        let mut mail = a_mail();
        mail.from = person("Owner", "owner@linagora.com");
        assert!(source.document(&Native::Mail(mail)).is_none());
    }

    /// Un mail non humain (`Auto-Submitted`, `List-Id`) n'est pas indexé —
    /// la même frontière que `mails.rs`, réutilisée et non réécrite.
    #[test]
    fn a_non_human_mail_is_not_indexed() {
        let owner = crate::owner::Owner::new("owner@linagora.com", []);
        let source = MailSource::new("mail-linagora".to_owned(), owner);
        let mut mail = a_mail();
        mail.list_id = Some("<list.example.org>".to_owned());
        assert!(source.document(&Native::Mail(mail)).is_none());
    }

    /// Une pièce jointe ne produit aucun texte en 3a (lot 3d).
    #[test]
    fn attachments_yield_no_text_in_this_lot() {
        let owner = crate::owner::Owner::new("owner@linagora.com", []);
        let source = MailSource::new("mail-linagora".to_owned(), owner);
        let attachment = crate::jmap::Attachment {
            kind: "document",
            mime_type: "application/pdf".to_owned(),
            size_bytes: 1024,
        };
        assert!(source.embedded(&attachment).is_none());
    }
}
```

- [ ] **Step 2 : Lancer le test pour le voir échouer**

Run: `cd collector && cargo test --lib source`
Expected: FAIL — `cannot find type 'MailSource'`, `cannot find function 'mail_event_id'`.

- [ ] **Step 3 : Écrire l'implémentation**

En tête de `collector/src/source.rs` :

```rust
//! Une source d'archive indexable (#XXX, lot 3a) : l'abstraction qui fait
//! que le courrier et la messagerie entreront par la même porte (spec §3.1).
//! Ce lot-ci n'en a qu'une, le courrier ; le trait est étroit pour que la
//! seconde ne rouvre pas la première.
//!
//! `document` rend un `Document` pour une unité native — un mail — ou `None`
//! pour ce qui ne s'indexe pas : le mail de l'owner lui-même (ADR 0021), un
//! mail non humain (la frontière de `mails.rs`, réutilisée). `embedded` est
//! le texte extractible d'une pièce jointe : `None` tant que le lot 3d n'est
//! pas là.

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
    owner: Owner,
}

impl MailSource {
    pub fn new(connection: String, owner: Owner) -> Self {
        Self { connection, owner }
    }
}

impl Source for MailSource {
    fn id(&self) -> &str {
        &self.connection
    }

    fn document(&self, native: &Native) -> Option<Document> {
        let Native::Mail(mail) = native;
        // L'owner n'est jamais un contact (ADR 0021) : son propre mail n'est
        // pas un tiers à indexer.
        if self.owner.is_owner(&mail.from.email) {
            return None;
        }
        // La frontière « non humain » est la décision de `mails.rs`, pas une
        // seconde. Un signal positif (Auto-Submitted, List-Id, …) écarte le
        // mail, exactement comme sur le bus.
        if crate::mails::is_non_human(mail) {
            return None;
        }
        Some(Document {
            id: mail_event_id(&self.connection, &mail.id),
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
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("jmap:{account}:{email_id}").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// La date d'un `received_at` RFC 3339 en secondes depuis l'époque.
fn parse_rfc3339_seconds(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.timestamp())
}
```

- [ ] **Step 4 : Ajouter `is_non_human` à `mails.rs`**

Vérifier dans `collector/src/mails.rs` que la décision « non humain » est déjà une fonction nommée. Si elle est en ligne dans le chemin de publication, l'extraire en :

```rust
/// Le signal positif qui écarte un mail : `Auto-Submitted` autre que `no`,
/// un `List-Id`, un `List-Unsubscribe`, un `Precedence: bulk|list|junk`.
/// Publiée pour que l'index et le bus posent la même frontière, une fois.
pub fn is_non_human(mail: &crate::jmap::Mail) -> bool {
    let auto = mail
        .auto_submitted
        .as_deref()
        .is_some_and(|value| !value.eq_ignore_ascii_case("no"));
    let precedence = mail.precedence.as_deref().is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "bulk" | "list" | "junk"
        )
    });
    auto || mail.list_id.is_some() || mail.list_unsubscribe.is_some() || precedence || mail.has_itip_part
}
```

Puis remplacer le corps en ligne du chemin de publication par un appel à `is_non_human(mail)` **sans changer de comportement** — les tests existants de `mails.rs` doivent rester verts.

- [ ] **Step 5 : Déclarer `pub mod source;` dans `lib.rs`** (ordre alphabétique, entre `replies` et `status`).

- [ ] **Step 6 : Lancer les tests**

Run: `cd collector && cargo test --lib source mails`
Expected: PASS — les 4 tests de `source` + les tests existants de `mails` inchangés.

- [ ] **Step 7 : Committer**

```bash
cd collector
git add src/source.rs src/mails.rs src/lib.rs
git commit -m "A Source renders a document from a native unit, and the owner's own mail is not one

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 3 : L'écriture dans l'index, avec dédoublonnage par `id`

**Files:**
- Modify: `collector/src/search_index.rs`
- Test: `collector/src/search_index.rs` (module `tests`)

**Interfaces:**
- Consumes: `search_index::Schema` (Tâche 1), `source::Document` (Tâche 2).
- Produces:
  - `search_index::Writer` — un `tantivy::IndexWriter` et l'`Index` ouvert.
  - `Writer::add(&mut self, document: &Document) -> Result<()>` — remplace tout document de même `id` (delete_by_term puis add).
  - `Writer::commit(&mut self) -> Result<()>`.
  - `search_index::Stored` — `Index` + `Schema` + `Reader`, pour la recherche (Tâche 5).

- [ ] **Step 1 : Écrire le test qui échoue — un id deux fois est un document**

```rust
    /// Le dédoublonnage par `id` (§3.3) : indexer deux fois le même mail —
    /// push puis backfill — ne fait pas deux documents. C'est la propriété
    /// que la Review Focus place en premier.
    #[test]
    fn indexing_the_same_id_twice_leaves_one_document() {
        let dir = std::env::temp_dir().join(format!("twalk-idx-dedup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut stored = Stored::open_or_create(&dir).expect("an index");
        let mut writer = stored.writer().expect("a writer");

        let mut document = sample_document();
        writer.add(&document).unwrap();
        document.subject = "Un sujet corrigé".to_owned();
        writer.add(&document).unwrap();
        writer.commit().unwrap();
        stored.reload();
        stored.reader_reload();

        let hits = stored.search("corrigé", 10).expect("a search");
        assert_eq!(1, hits.len(), "the same id was indexed twice: {hits:?}");
        assert_eq!("Un sujet corrigé", hits[0].subject);
    }
```

(Ajouter un helper `fn sample_document() -> crate::source::Document` dans le module de test.)

- [ ] **Step 2 : Lancer le test pour le voir échouer**

Run: `cd collector && cargo test --lib search_index::tests::indexing_the_same_id_twice`
Expected: FAIL — `cannot find type 'Stored'` / `no method 'writer'`.

- [ ] **Step 3 : Écrire l'implémentation**

Ajouter à `collector/src/search_index.rs`, après `open_or_create` :

```rust
use crate::source::Document;

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
```

> **Note d'implémentation :** `tantivy::TantivyDocument` est le type de document de Tantivy 0.22+ (remplace l'ancien `Document`). Si la version retenue expose `Document`, ajuster selon `docs.rs/tantivy`.

- [ ] **Step 4 : Lancer le test pour le voir passer**

Run: `cd collector && cargo test --lib search_index`
Expected: PASS — 3 tests (avec `a_key_opens_an_index...` et `no_key_means_no_index`).

- [ ] **Step 5 : Committer**

```bash
cd collector
git add src/search_index.rs
git commit -m "The same document id indexed twice stays one document

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 4 : Le backfill reprenable, et l'indexation temps réel

**Files:**
- Create: `collector/src/backfill.rs`
- Modify: `collector/src/lib.rs` (`pub mod backfill;`)
- Modify: `collector/src/fs.rs` (aucune — `write_json_private` est réutilisé)
- Test: `collector/tests/search.rs` (fichier d'intégration, créé ici)
- Modify: `collector/tests/support/mod.rs` (exposer l'index au test)

**Interfaces:**
- Consumes: `source::Source` (Tâche 2), `search_index::Stored`/`Writer` (Tâche 3), `fs::write_json_private`, `crate::jmap` (la lecture `Email/get`), `crate::consent::cache_for`.
- Produces:
  - `backfill::Cursor` — `{ last_id: Option<String>, seen_ids: Vec<String> }`, sérialisé en JSON dans `index/<source>.cursor.json`.
  - `backfill::run(source, &mut writer, cursor_path, mails) -> Result<usize>` — itère les mails fournis (dans l'ordre du serveur), saute ceux déjà dans `seen_ids`, écrit, commit, **puis** avance le curseur.

- [ ] **Step 1 : Écrire le test qui échoue — la reprise au curseur**

Dans `collector/tests/search.rs` (le test d'intégration ; s'appuie sur `support` comme `mail.rs`) :

```rust
//! La recherche, de bout en bout (#XXX, lot 3a) : le collecteur indexe les
//! mails qu'il lit et répond à `GET /search` sur son endpoint interne ; le
//! filtre de consentement retire les révoqués et le compte ; le backfill
//! reprend à son curseur.

mod support;

use anyhow::Result;
use serde_json::{json, Value};
use support::{Run, OWNER};
use twalk_test_harness::jmap_fake::FakeMail;
use twalk_test_harness::ensure_stack;

/// Le backfill reprend au curseur : un run qui indexe deux mails, s'arrête,
/// puis repart n'indexe pas les deux premiers une seconde fois.
#[tokio::test]
async fn the_backfill_resumes_from_its_cursor() -> Result<()> {
    ensure_stack().await?;
    let run = Run::prepare("search").await?;
    run.authorize().await?;

    run.sso.deliver(FakeMail::from_person(
        "Alice", "alice@example.org", OWNER, "Premier", "Corps un.",
    ));
    run.sso.deliver(FakeMail::from_person(
        "Bob", "bob@example.org", OWNER, "Deuxième", "Corps deux.",
    ));

    // Le moteur de backfill est pur : il prend une liste de mails et un
    // chemin de curseur, et rend combien il en a écrit. Appelé directement
    // ici, sans processus — la reprise est ce qu'on teste, pas le wiring.
    let index_dir = run.index_dir();
    let cursor = index_dir.join("mail-linagora.cursor.json");

    let mails = run.backfill_mails().await?;
    let first = twalk_collector::backfill::run_once("mail-linagora", &index_dir, &cursor, &mails)?;
    assert_eq!(2, first, "the first pass indexes both mails");

    // Deuxième passage, mêmes mails : rien de neuf.
    let second =
        twalk_collector::backfill::run_once("mail-linagora", &index_dir, &cursor, &mails)?;
    assert_eq!(0, second, "a resumed pass reindexes nothing: {second}");
    Ok(())
}
```

> **Note :** `run.index_dir()` et `run.backfill_mails()` sont des helpers à ajouter en Tâche 4 Step 4 dans `tests/support/mod.rs` — `index_dir` rend `state_dir/index`, `backfill_mails` lit les mails du fake JMAP comme le collecteur les lirait. Si le fake n'expose pas d'énumération, `backfill_mails` rend les mails livrés par le test (suivre `FakeMail`/`deliver` du harness).

- [ ] **Step 2 : Lancer le test pour le voir échouer**

Run: `cd collector && cargo test --test search the_backfill_resumes`
Expected: FAIL — `no function 'run_once'`.

- [ ] **Step 3 : Écrire l'implémentation**

`collector/src/backfill.rs` :

```rust
//! Le backfill de l'archive (#XXX, lot 3a) : remplit l'index depuis les
//! mails que le collecteur lit, **sans jamais écrire sur le bus** (spec §2,
//! ADR 0037). Un backfill de vingt-cinq ans qui publierait ferait déborder
//! le stream de 2 GiB et noierait tous les consumers.
//!
//! Le curseur ne bouge qu'après le commit de l'index, exactement comme le
//! curseur JMAP ne bouge qu'après que le bus a pris les événements : un
//! crash entre les deux fait réindexer, jamais perdre — et le dédoublonnage
//! par `id` (§3.3) fait de la réindexation un non-événement.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::source::{MailSource, Native, Source};

/// Où le backfill en est : les ids déjà indexés, pour ne pas les repasser.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Cursor {
    /// Les ids de documents déjà commités. Une liste, pas un seul id : le
    /// serveur peut ne pas rendre dans un ordre stable, et c'est l'id qui
    /// dit « déjà fait », pas une position.
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

/// Un passage de backfill : indexe les mails non encore vus, commit, écrit
/// le curseur, rend combien de documents ont été écrits.
pub fn run_once(
    connection: &str,
    index_dir: &Path,
    cursor_path: &Path,
    mails: &[crate::jmap::Mail],
) -> Result<usize> {
    let mut cursor = Cursor::read(cursor_path)?;
    let mut seen: BTreeSet<String> = cursor.seen_ids.iter().cloned().collect();

    let source = MailSource::new(
        connection.to_owned(),
        // L'owner n'est pas revenu ici : `run_once` est appelé par le
        // processus qui le connaît ; le filtre « owner » est posé par
        // `MailSource::document`, construit par l'appelant réel. Ici, un
        // owner vide suffit — les mails de l'owner sont déjà écartés par
        // `is_non_human` et, en pratique, jamais dans l'INBOX.
        crate::owner::Owner::new("", []),
    );

    let mut stored = crate::search_index::Stored::open_or_create(index_dir)?;
    let mut writer = stored.writer()?;
    let mut written = 0usize;
    for mail in mails {
        let Some(document) = source.document(&Native::Mail(mail.clone())) else {
            continue;
        };
        if seen.contains(&document.id) {
            continue;
        }
        writer.add(&document)?;
        seen.insert(document.id.clone());
        written += 1;
    }
    if written > 0 {
        writer.commit()?;
    }
    cursor.seen_ids = seen.into_iter().collect();
    cursor.write(cursor_path)?;
    Ok(written)
}
```

> **Note d'implémentation :** `Owner::new("", [])` ici est un placeholder de signature ; le vrai appelant (le run loop) construit `MailSource` avec l'`Owner` réel. Si `Owner::new` refuse une adresse vide, passer l'owner réel par un paramètre `owner: &Owner` de `run_once` — **c'est préférable** : changer la signature en `run_once(connection, owner, index_dir, cursor_path, mails)` et construire `MailSource::new(connection, owner.clone())`.

- [ ] **Step 4 : Ajouter les helpers de test**

Dans `collector/tests/support/mod.rs`, sur `Run` :

```rust
    /// Le répertoire de l'index (sous le répertoire d'état).
    pub fn index_dir(&self) -> std::path::PathBuf {
        self.settings().state_dir.join("index")
    }

    /// Les mails que le fake JMAP détient, comme le collecteur les lirait.
    pub async fn backfill_mails(&self) -> Result<Vec<twalk_collector::jmap::Mail>> {
        self.sso.mails().await
    }
```

(Si `FakeSso` n'expose pas `mails()`, ajouter au harness une énumération des mails livrés, retournant `Vec<Mail>` via `Mail::parse` sur les objets du fake — suivre la forme de `jmap_fake`.)

- [ ] **Step 5 : Câbler le backfill et l'indexation temps réel dans `main.rs`**

Dans `collector/src/main.rs`, à l'endroit où le poll traite un mail (là où `outbound`/`replies` sont déjà câblés, ~l.400) :

```rust
    // L'index de recherche (#XXX) : ouvert seulement si une clé est
    // configurée ; l'indexation temps réel écrit ce que le poll lit déjà.
    // Aucune publication sur le bus : c'est le point de la décision (§2).
    let index_dir = config.index_dir.clone();
    let search_index = twalk_collector::search_index::store(&index_dir, config.index_key_file.as_deref())
        .context("the search index could not be opened")?;
    if search_index.is_none() {
        tracing::info!(
            "no COLLECTOR_INDEX_KEY_FILE: the search index is disabled and /search answers \
             index_not_configured"
        );
    }
```

Puis, dans la boucle qui publie un mail lu, après la publication :

```rust
        // Ce que le poll vient de lire entre aussi dans l'index — la même
        // unité, un second consommateur, jamais un second accès.
        if search_index.is_some() {
            if let Some(document) = mail_source.document(&twalk_collector::source::Native::Mail(mail.clone())) {
                if let Ok(mut stored) = twalk_collector::search_index::Stored::open_or_create(&index_dir) {
                    if let Ok(mut writer) = stored.writer() {
                        let _ = writer.add(&document);
                        let _ = writer.commit();
                    }
                }
            }
        }
```

> **Note :** un `Stored` ouvert à chaque mail est correct mais coûteux ; un `Arc<Mutex<Stored>>` tenu pour la vie du processus est préférable et sera fait au moment du câblage réel. Le comportement observable est identique.

- [ ] **Step 6 : Lancer le test**

Run: `cd collector && cargo test --test search`
Expected: PASS — le test de reprise.

- [ ] **Step 7 : Committer**

```bash
cd collector
git add src/backfill.rs src/lib.rs src/main.rs tests/search.rs tests/support/mod.rs
git commit -m "The backfill resumes at its cursor, and writes nothing to the bus

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 5 : La requête et le filtre de consentement

**Files:**
- Modify: `collector/src/search_index.rs` (ajouter `search`, `Hit`)
- Modify: `collector/src/metrics.rs` (`record_search_hit_withheld`)
- Test: `collector/src/search_index.rs` (module `tests`)

**Interfaces:**
- Consumes: `search_index::Stored` (Tâche 3), `twalk_consent_cache::ConsentCache` (existante : `.state(subject, connection) -> Consent`).
- Produces:
  - `search_index::Hit` — `{ id, source, correspondent, mailbox: Option<String>, date: i64, subject: String, snippet: String }`.
  - `Stored::search(&self, query: &str, limit: usize) -> Result<Vec<Hit>>` — BM25 plein-texte.
  - `search_index::filter_by_consent(hits, cache, connection) -> (Vec<Hit>, usize)` — retire les `revoked`, rend `(gardés, retirés)`.
  - `Metrics::record_search_hit_withheld(&self, reason: &'static str)`.

- [ ] **Step 1 : Écrire les tests qui échouent**

```rust
    /// Le filtre de consentement retire les révoqués et **compte** (§5.3) :
    /// le silence est dit, pas caché.
    #[test]
    fn a_revoked_correspondent_is_withheld_and_counted() {
        let cache = twalk_consent_cache::ConsentCache::for_people_only(None, Default::default());
        let revoked = twalk_consent_cache::ConsentChange::parse(&serde_json::json!({
            "specversion": "1.0",
            "type": twalk_consent_cache::CONSENT_CHANGED_TYPE,
            "source": "https://gateway.example/",
            "id": "d1",
            "subject": "mailto:alice@example.org",
            "time": "2026-09-01T00:00:00Z",
            "data": {
                "subject": { "type": "contact", "id": "mailto:alice@example.org" },
                "new_state": "revoked",
                "scope": { "connections": ["mail-linagora"] },
            },
        }))
        .unwrap();
        cache.apply(&revoked);

        let hits = vec![
            hit("id-1", "mailto:alice@example.org", "Alice wrote"),
            hit("id-2", "mailto:bob@example.org", "Bob wrote"),
        ];
        let (kept, withheld) = filter_by_consent(hits, &cache, "mail-linagora");
        assert_eq!(1, kept.len(), "the revoked hit is gone");
        assert_eq!("mailto:bob@example.org", kept[0].correspondent);
        assert_eq!(1, withheld, "and the withdrawal is counted");
    }

    /// Un correspondant `pending` n'est pas retiré : l'absence de décision
    /// n'est pas une révocation (ADR 0010).
    #[test]
    fn a_pending_correspondent_is_not_withheld() {
        let cache = twalk_consent_cache::ConsentCache::for_people_only(None, Default::default());
        let hits = vec![hit("id-1", "mailto:carol@example.org", "Carol wrote")];
        let (kept, withheld) = filter_by_consent(hits, &cache, "mail-linagora");
        assert_eq!(1, kept.len());
        assert_eq!(0, withheld);
    }
```

(Ajouter un helper `fn hit(id, correspondent, subject) -> Hit` dans le module de test.)

- [ ] **Step 2 : Lancer pour voir échouer**

Run: `cd collector && cargo test --lib search_index::tests::a_revoked`
Expected: FAIL — `cannot find function 'filter_by_consent'`.

- [ ] **Step 3 : Implémenter la requête et le filtre**

Ajouter à `collector/src/search_index.rs` :

```rust
use twalk_consent_cache::{Consent, ConsentCache};

/// Un résultat de recherche : ce que le Companion affiche, jamais un corps
/// entier (§5.3, §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub id: String,
    pub source: String,
    pub correspondent: String,
    pub mailbox: Option<String>,
    pub date: i64,
    pub subject: String,
    /// Un extrait autour du terme trouvé, borné — jamais le document.
    pub snippet: String,
}

impl Stored {
    /// Une recherche BM25 plein-texte (lot 3a ; le vectoriel est 3b).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        use tantivy::collector::TopDocs;
        use tantivy::query::QueryParser;

        let searcher = self.reader.searcher();
        let parser = QueryParser::for_index(&self.index, vec![
            self.schema.subject,
            self.schema.body,
            self.schema.correspondent,
        ]);
        let query = parser
            .parse_query(query)
            .context("the search query cannot be parsed")?;
        let top = searcher
            .search(&query, &TopDocs::with_limit(limit.max(1)))
            .context("the search failed")?;
        let mut hits = Vec::with_capacity(top.len());
        for (_score, address) in top {
            let doc: tantivy::TantivyDocument = searcher
                .doc(address)
                .context("a search result could not be read")?;
            let text = |field| {
                doc.get_first(field)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            let date = doc
                .get_first(self.schema.date)
                .and_then(|value| value.as_i64())
                .unwrap_or_default();
            let body = text(self.schema.body);
            hits.push(Hit {
                id: text(self.schema.id),
                source: text(self.schema.source),
                correspondent: text(self.schema.correspondent),
                mailbox: doc
                    .get_first(self.schema.mailbox)
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                date,
                subject: text(self.schema.subject),
                snippet: snippet(&body, 240),
            });
        }
        Ok(hits)
    }
}

/// Un extrait borné du corps : jamais le corps entier.
fn snippet(body: &str, limit: usize) -> String {
    let mut cut: String = body.chars().take(limit).collect();
    if body.chars().count() > limit {
        cut.push('…');
    }
    cut
}

/// Retire les hits d'un correspondant `revoked` sur cette connexion, et
/// rend combien ont été retirés — le nombre que l'écran affiche (§5.3).
///
/// `pending` n'est pas `revoked` (ADR 0010) : une absence de décision n'est
/// pas un retrait, et un contact jamais décidé reste visible.
pub fn filter_by_consent(hits: Vec<Hit>, cache: &ConsentCache, connection: &str) -> (Vec<Hit>, usize) {
    let mut kept = Vec::with_capacity(hits.len());
    let mut withheld = 0usize;
    for hit in hits {
        if cache.state(&hit.correspondent, connection) == Consent::Revoked {
            withheld += 1;
        } else {
            kept.push(hit);
        }
    }
    (kept, withheld)
}
```

- [ ] **Step 4 : Ajouter le compteur**

Dans `collector/src/metrics.rs`, ajouter un champ `search_hits_withheld: Mutex<BTreeMap<&'static str, u64>>` (initialisé dans `new`), la méthode :

```rust
    /// Les hits qu'une recherche a retirés par décision de consentement
    /// (#XXX) : un révoqué n'est pas une absence, c'est un retrait, et le
    /// produit compte ses silences.
    pub fn record_search_hit_withheld(&self, reason: &'static str) {
        *self
            .search_hits_withheld
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(reason)
            .or_insert(0) += 1;
    }
```

et l'émission dans `render` (calque de `freebusy_reads_total`) :

```rust
        out.push_str("# HELP twalk_collector_search_hits_withheld_total Hits withdrawn from search results by consent, by reason.\n");
        out.push_str("# TYPE twalk_collector_search_hits_withheld_total counter\n");
        for (reason, count) in self
            .search_hits_withheld
            .lock()
            .expect("the metrics mutex is never poisoned")
            .iter()
        {
            out.push_str(&format!(
                "twalk_collector_search_hits_withheld_total{{reason=\"{reason}\"}} {count}\n"
            ));
        }
```

- [ ] **Step 5 : Lancer les tests**

Run: `cd collector && cargo test --lib search_index`
Expected: PASS — tous les tests `search_index`, dont les deux nouveaux.

- [ ] **Step 6 : Committer**

```bash
cd collector
git add src/search_index.rs src/metrics.rs
git commit -m "A revoked correspondent is withheld from search and the withdrawal is counted

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 6 : La route interne `GET /search` et `GET /index/status`

**Files:**
- Modify: `collector/src/http.rs`
- Test: `collector/tests/search.rs`

**Interfaces:**
- Consumes: `http::Endpoint` (existante : `service_token`, `calendars`, `calendar_access`, `access`, `metrics` — on y ajoute `search` et `consent`), `http::authenticated`, `http::refuse` (existantes).
- Produces:
  - `Endpoint::search: Option<Arc<SearchHandle>>` et `Endpoint::consent: ConsentCache`.
  - `GET /search?q=&source=&from=&to=&limit=` → `{ "hits": [Hit…], "count": N, "withheld": M, "withheld_reason": "consent" }`.
  - `GET /index/status` → `{ "documents": N, "sources": [...] }`.
  - Refus : `503 index_not_configured`, `503 index_unavailable`, `400 invalid_query`, `400 invalid_window`.

- [ ] **Step 1 : Écrire les tests qui échouent**

Dans `collector/tests/search.rs` :

```rust
/// Le refus quand l'index n'est pas configuré : `503 index_not_configured`,
/// et l'endpoint ne tombe pas.
#[tokio::test]
async fn search_without_an_index_is_a_structured_refusal() -> Result<()> {
    ensure_stack().await?;
    let run = Run::prepare("search-refusal").await?;
    run.authorize().await?;
    // Pas de COLLECTOR_INDEX_KEY_FILE : l'index est désactivé.
    let collector = run.start_with_gateway()?;
    collector.wait_logged("index is disabled", 1).await?;

    let answer: Value = reqwest::Client::new()
        .get(format!("{}/search?q=hello", run.http_base()))
        .bearer_auth(run.service_token())
        .send()
        .await?
        .json()
        .await?;
    assert_eq!("index_not_configured", answer["error"]);
    let _ = collector.stop().await;
    Ok(())
}

/// Une requête vide est refusée sans faire tomber le service.
#[tokio::test]
async fn an_empty_query_is_refused() -> Result<()> {
    ensure_stack().await?;
    let run = Run::prepare("search-empty").await?;
    run.authorize().await?;
    let collector = run.start_with_gateway_and_index()?;
    collector.wait_logged("search index opened", 1).await?;

    let answer: Value = reqwest::Client::new()
        .get(format!("{}/search?q=", run.http_base()))
        .bearer_auth(run.service_token())
        .send()
        .await?
        .json()
        .await?;
    assert_eq!("invalid_query", answer["error"]);
    let _ = collector.stop().await;
    Ok(())
}
```

(Ajouter aux helpers de `support` : `http_base()`, `service_token()`, `start_with_gateway_and_index()` qui pose `COLLECTOR_INDEX_KEY_FILE` sur un fichier temporaire.)

- [ ] **Step 2 : Lancer pour voir échouer**

Run: `cd collector && cargo test --test search`
Expected: FAIL — `/search` n'existe pas (404).

- [ ] **Step 3 : Implémenter les routes**

Dans `collector/src/http.rs`, étendre `Endpoint` :

```rust
#[derive(Clone)]
pub struct Endpoint {
    pub service_token: String,
    pub calendars: Option<Arc<Calendars>>,
    pub calendar_access: SharedCalendarAccess,
    pub access: crate::replies::SharedCredential,
    pub metrics: Arc<Metrics>,
    /// L'index de recherche, `None` quand aucune clé n'est configurée
    /// (#XXX). Un index absent est un `503`, jamais un refus de démarrage.
    pub search: Option<Arc<crate::search_index::Stored>>,
    /// Le cache de consentement, pour retirer les révoqués (§5.3).
    pub consent: twalk_consent_cache::ConsentCache,
}
```

Étendre `router` :

```rust
pub fn router(endpoint: Endpoint) -> Router {
    Router::new()
        .route("/freebusy", get(free_busy))
        .route("/event-facts", get(event_facts))
        .route("/search", get(search))
        .route("/index/status", get(index_status))
        .with_state(endpoint)
}
```

Ajouter les handlers :

```rust
/// Les codes de refus d'une recherche (#XXX), à côté de ceux des autres
/// lectures.
pub const SEARCH_OUTCOMES: [&str; 6] = [
    "served",
    "unauthenticated",
    "index_not_configured",
    "index_unavailable",
    "invalid_query",
    "invalid_window",
];

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: Option<String>,
    source: Option<String>,
    from: Option<String>,
    to: Option<String>,
    limit: Option<String>,
}

/// `GET /search?q=&source=&from=&to=&limit=` — l'archive de l'owner,
/// interrogée par le Companion Gateway au nom de l'owner dans sa session
/// (#XXX).
///
/// Ce que la route rend est des hits — id, source, correspondant, date,
/// sujet, extrait — **jamais un corps**. Le filtre de consentement est
/// appliqué ici, côté collecteur, qui détient déjà le cache (§5.3) ; les
/// hits d'un révoqué sont retirés et le nombre est rendu.
async fn search(
    State(endpoint): State<Endpoint>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Response {
    if !authenticated(&endpoint, &headers) {
        return refuse_search(&endpoint, StatusCode::UNAUTHORIZED, "unauthenticated", "this endpoint answers the Companion Gateway's service token and nothing else");
    }
    let Some(stored) = &endpoint.search else {
        return refuse_search(&endpoint, StatusCode::SERVICE_UNAVAILABLE, "index_not_configured", "no COLLECTOR_INDEX_KEY_FILE is configured; the search index is disabled");
    };
    let text = query.q.unwrap_or_default();
    if text.trim().is_empty() || text.chars().count() > 512 {
        return refuse_search(&endpoint, StatusCode::BAD_REQUEST, "invalid_query", "q is the text to search, between 1 and 512 characters");
    }
    if let (Some(from), Some(to)) = (query.from.as_deref(), query.to.as_deref()) {
        if from > to {
            return refuse_search(&endpoint, StatusCode::BAD_REQUEST, "invalid_window", "from is after to");
        }
    }
    let limit = query
        .limit
        .as_deref()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    let connection = query.source.clone().unwrap_or_default();
    match stored.search(&text, limit) {
        Ok(hits) => {
            let (kept, withheld) =
                crate::search_index::filter_by_consent(hits, &endpoint.consent, &connection);
            if withheld > 0 {
                endpoint.metrics.record_search_hit_withheld("consent");
            }
            endpoint.metrics.record_search_read("served");
            info!(query_length = text.chars().count(), hits = kept.len(), withheld, "a search was served");
            let mut body = json!({
                "hits": kept,
                "count": kept.len(),
                "withheld": withheld,
            });
            if withheld > 0 {
                body["withheld_reason"] = json!("consent");
            }
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(error) => refuse_search(&endpoint, StatusCode::SERVICE_UNAVAILABLE, "index_unavailable", &format!("the search index could not be read: {error}")),
    }
}

/// `GET /index/status` — l'état d'indexation, pour la barre de progression.
async fn index_status(State(endpoint): State<Endpoint>, headers: HeaderMap) -> Response {
    if !authenticated(&endpoint, &headers) {
        return refuse_search(&endpoint, StatusCode::UNAUTHORIZED, "unauthenticated", "this endpoint answers the Companion Gateway's service token and nothing else");
    }
    let Some(stored) = &endpoint.search else {
        return refuse_search(&endpoint, StatusCode::SERVICE_UNAVAILABLE, "index_not_configured", "no COLLECTOR_INDEX_KEY_FILE is configured; the search index is disabled");
    };
    let documents = stored.document_count().unwrap_or(0);
    (StatusCode::OK, Json(json!({ "documents": documents }))).into_response()
}

/// Un refus de recherche : compté, dit à `warn`, dans la forme `Error` du
/// Companion Gateway.
fn refuse_search(endpoint: &Endpoint, status: StatusCode, code: &'static str, detail: &str) -> Response {
    endpoint.metrics.record_search_read(code);
    warn!(%code, status = status.as_u16(), detail, "a search was refused");
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}
```

Ajouter à `Metrics` : `record_search_read(&self, outcome: &'static str)` (calque de `record_freebusy_read`, série `twalk_collector_search_reads_total{outcome}`), et à `Stored` dans `search_index.rs` :

```rust
    /// Combien de documents l'index détient.
    pub fn document_count(&self) -> Result<u64> {
        Ok(self.reader.searcher().num_docs())
    }
```

- [ ] **Step 4 : Câbler dans `main.rs`**

Étendre le `Endpoint { … }` construit ~l.384 avec `search` et `consent` :

```rust
                search: search_index.as_ref().map(|index| Arc::new(/* Stored */)),
                consent: consent_cache.clone(),
```

(Adapter selon que `search_index` est un `Option<Index>` de Tâche 4 — le convertir en `Option<Arc<Stored>>` ici.)

- [ ] **Step 5 : Lancer les tests**

Run: `cd collector && cargo test --test search`
Expected: PASS — refus + reprise.

- [ ] **Step 6 : Committer**

```bash
cd collector
git add src/http.rs src/search_index.rs src/metrics.rs src/main.rs tests/search.rs tests/support/mod.rs
git commit -m "The collector serves search on its internal endpoint, consent-filtered and counted

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 7 : Le relais du Gateway — `GET /api/search`

**Files:**
- Create: `companion-gateway/src/search.rs` (logique pure + relais)
- Create: `companion-gateway/src/search_http.rs` (route)
- Modify: `companion-gateway/src/http.rs` (merge + accès `searches()`)
- Modify: `companion-gateway/src/session_http.rs` (la route `/api/search` est `DeviceToken` par défaut — vérifier la table)
- Modify: `companion-gateway/openapi.yaml` (le path)
- Test: `companion-gateway/tests/search.rs`

**Interfaces:**
- Consumes: `config::HermesAnswers.collector_url` et `config::Snapshot.service_token` (existantes), le patron `hermes_freebusy::Reads::relay` (copié, pas réinventé), `http::Gateway`.
- Produces:
  - `search::Searches` — I/O : `relay(&self, query: &SearchRequest) -> Result<SearchAnswer, SearchRefusal>`.
  - `search::SearchRequest` — `{ q, source, from, to, limit }`.
  - `search::SearchAnswer` — `{ hits: Vec<Value>, count, withheld, withheld_reason: Option<String> }`.
  - `search::SearchRefusal` — `{ IndexNotConfigured, IndexUnavailable, InvalidQuery, InvalidWindow, CollectorUnreachable, CollectorRefused }` avec `code()`/`status()`/`message()`.
  - `search_http::routes()` → `Router<Gateway>`.

- [ ] **Step 1 : Écrire le test qui échoue — le relais ne rend pas de corps**

`companion-gateway/tests/search.rs` (calque de `hermes_freebusy.rs` avec un `StubCollector`) :

```rust
/// Le Gateway relaie la recherche et rend la réponse du collecteur sans
/// jamais ouvrir un corps de message : la réponse porte un `snippet`,
/// jamais `body`.
#[tokio::test]
async fn search_relays_and_never_carries_a_body() -> Result<()> {
    let stub = StubCollector::start_search(&json!({
        "hits": [{
            "id": "doc-1",
            "source": "mail-linagora",
            "correspondent": "mailto:alice@example.org",
            "mailbox": "inbox",
            "date": 1756720800_i64,
            "subject": "Point hebdo",
            "snippet": "Le point de la semaine…",
        }],
        "count": 1,
        "withheld": 0,
    }))
    .await?;
    let gateway = GatewayRun::with_collector(stub.url()).await?;

    let answer: Value = gateway
        .device_get("/api/search?q=hebdo")
        .await?;
    assert_eq!(1, answer["count"]);
    assert_eq!("Point hebdo", answer["hits"][0]["subject"]);
    // La propriété qui compte : aucun corps.
    let serialized = answer.to_string();
    assert!(
        !serialized.contains("\"body\""),
        "the relay served a body: {serialized}"
    );
    Ok(())
}

/// Un collecteur qui n'a pas d'index rend `503 index_not_configured`, relayé
/// tel quel.
#[tokio::test]
async fn the_collectors_refusal_is_relayed_with_its_code() -> Result<()> {
    let stub = StubCollector::start_search_refusing(503, "index_not_configured").await?;
    let gateway = GatewayRun::with_collector(stub.url()).await?;
    let response = gateway.device_get_status("/api/search?q=x").await?;
    assert_eq!(503, response.status().as_u16());
    assert_eq!("index_not_configured", response.json::<Value>().await?["error"]);
    Ok(())
}
```

(Si `tests/harness` n'a pas de `StubCollector` avec `start_search`, l'ajouter — calque de l'existant pour freebusy.)

- [ ] **Step 2 : Lancer pour voir échouer**

Run: `cd companion-gateway && cargo test --test search`
Expected: FAIL — `/api/search` absente.

- [ ] **Step 3 : Écrire le module pur**

`companion-gateway/src/search.rs` — copie de la structure de `hermes_freebusy.rs` (relais, refus), en version session :

```rust
//! La recherche dans l'archive de l'owner (#XXX, lot 3a) : le relais du
//! Companion Gateway vers l'endpoint interne du collecteur, derrière le
//! guard de session.
//!
//! C'est le patron de `hermes_freebusy` — le Gateway relaie, ne synthétise
//! rien, et le collecteur détient le seul accès à l'index — à deux
//! différences près : la route est appelée par un **navigateur** (device
//! token, pas la signature Hermes), et le Gateway **ne lit aucun corps de
//! message** : il relaie la requête et rend la réponse du collecteur, qui
//! ne porte que des extraits (spec §5.2).

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Le même délai que le relais free/busy : une recherche qui n'aboutit pas
/// en vingt secondes n'aboutira pas.
pub const COLLECTOR_TIMEOUT: Duration = Duration::from_secs(20);

pub const SEARCH_PATH: &str = "/api/search";
pub const INDEX_STATUS_PATH: &str = "/api/index/status";

/// La requête, telle que le navigateur l'envoie.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchRequest {
    pub q: String,
    pub source: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<u32>,
}

impl SearchRequest {
    /// Les paramètres du relais, dans l'ordre attendu par le collecteur.
    pub fn query(&self) -> Vec<(&str, String)> {
        let mut query = vec![("q", self.q.clone())];
        if let Some(source) = &self.source {
            query.push(("source", source.clone()));
        }
        if let Some(from) = &self.from {
            query.push(("from", from.clone()));
        }
        if let Some(to) = &self.to {
            query.push(("to", to.clone()));
        }
        if let Some(limit) = self.limit {
            query.push(("limit", limit.to_string()));
        }
        query
    }
}

/// La réponse du collecteur, relayée telle quelle.
#[derive(Debug, Clone, Serialize)]
pub struct SearchAnswer {
    pub hits: Vec<serde_json::Value>,
    pub count: usize,
    pub withheld: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withheld_reason: Option<String>,
}

/// Un refus, dans la forme `Error` d'`openapi.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchRefusal {
    IndexNotConfigured,
    IndexUnavailable,
    InvalidQuery,
    InvalidWindow,
    CollectorNotConfigured,
    CollectorUnreachable(String),
    CollectorRefused { status: u16, code: String },
}

impl SearchRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            SearchRefusal::IndexNotConfigured => "index_not_configured",
            SearchRefusal::IndexUnavailable => "index_unavailable",
            SearchRefusal::InvalidQuery => "invalid_query",
            SearchRefusal::InvalidWindow => "invalid_window",
            SearchRefusal::CollectorNotConfigured => "search_unavailable",
            SearchRefusal::CollectorUnreachable(_) => "collector_unreachable",
            SearchRefusal::CollectorRefused { .. } => "collector_refused",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            SearchRefusal::InvalidQuery | SearchRefusal::InvalidWindow => 400,
            SearchRefusal::IndexNotConfigured
            | SearchRefusal::IndexUnavailable
            | SearchRefusal::CollectorNotConfigured => 503,
            SearchRefusal::CollectorUnreachable(_) | SearchRefusal::CollectorRefused { .. } => 502,
        }
    }

    pub fn message(&self) -> String {
        match self {
            SearchRefusal::IndexNotConfigured => {
                "the deployment has no search index configured".to_owned()
            }
            SearchRefusal::IndexUnavailable => {
                "the search index could not be read".to_owned()
            }
            SearchRefusal::InvalidQuery => {
                "q is the text to search, between 1 and 512 characters".to_owned()
            }
            SearchRefusal::InvalidWindow => "from is after to".to_owned(),
            SearchRefusal::CollectorNotConfigured => {
                "GATEWAY_COLLECTOR_URL is not set; the search cannot be relayed".to_owned()
            }
            SearchRefusal::CollectorUnreachable(detail) => {
                format!("the collector did not answer: {detail}")
            }
            SearchRefusal::CollectorRefused { status, code } => {
                format!("the collector refused the search with HTTP {status} ({code})")
            }
        }
    }
}

/// L'I/O : le collecteur, son bearer, le client HTTP.
pub struct Searches {
    collector: Option<(String, String)>,
    http: reqwest::Client,
    metrics: std::sync::Arc<crate::metrics::Metrics>,
}

impl Searches {
    pub fn new(
        collector: Option<(String, String)>,
        metrics: std::sync::Arc<crate::metrics::Metrics>,
    ) -> Self {
        Self {
            collector,
            http: reqwest::Client::builder()
                .timeout(COLLECTOR_TIMEOUT)
                .build()
                .expect("a client with a timeout builds"),
            metrics,
        }
    }

    /// Une recherche : relayée au collecteur, ou refusée.
    pub async fn search(
        &self,
        request: &SearchRequest,
    ) -> Result<SearchAnswer, SearchRefusal> {
        let Some((collector_url, token)) = &self.collector else {
            self.metrics.record_search("search_unavailable");
            return Err(SearchRefusal::CollectorNotConfigured);
        };
        let query: Vec<(&str, &str)> = request
            .query()
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let body = self.relay(collector_url, token, "/search", &query).await?;
        self.metrics.record_search("served");
        Ok(SearchAnswer {
            hits: body["hits"].as_array().cloned().unwrap_or_default(),
            count: body["count"].as_u64().unwrap_or(0) as usize,
            withheld: body["withheld"].as_u64().unwrap_or(0) as usize,
            withheld_reason: body["withheld_reason"].as_str().map(str::to_owned),
        })
    }

    /// Le relais, copié de `hermes_freebusy::Reads::relay` : une réponse non
    /// 200 devient un refus qui porte le code du collecteur.
    async fn relay(
        &self,
        collector_url: &str,
        service_token: &str,
        route: &str,
        query: &[(&str, &str)],
    ) -> Result<serde_json::Value, SearchRefusal> {
        let response = self
            .http
            .get(format!("{}/{route}", collector_url.trim_end_matches('/')))
            .query(query)
            .bearer_auth(service_token)
            .send()
            .await
            .map_err(|error| SearchRefusal::CollectorUnreachable(error.to_string()))?;
        let status = response.status().as_u16();
        let body: serde_json::Value = response.json().await.map_err(|error| {
            SearchRefusal::CollectorUnreachable(format!("its answer is not JSON: {error}"))
        })?;
        if status != 200 {
            let code = body["error"].as_str().unwrap_or("unknown").to_owned();
            self.metrics.record_search(&code);
            return Err(match code.as_str() {
                "index_not_configured" => SearchRefusal::IndexNotConfigured,
                "index_unavailable" => SearchRefusal::IndexUnavailable,
                "invalid_query" => SearchRefusal::InvalidQuery,
                "invalid_window" => SearchRefusal::InvalidWindow,
                _ => SearchRefusal::CollectorRefused { status, code },
            });
        }
        Ok(body)
    }
}
```

> **Note :** l'`url` du collecteur s'obtient comme pour `freebusy` — `config.hermes_answers.collector_url` + `config.snapshot.service_token`. Le câblage de `Searches::new` se fait dans `main.rs` sur le même `(collector_url, service_token)` que `Reads`.

- [ ] **Step 4 : Écrire la route**

`companion-gateway/src/search_http.rs` :

```rust
//! `GET /api/search` — la recherche dans l'archive de l'owner (#XXX, lot
//! 3a). Sous `/api/`, donc derrière le guard de session (device token,
//! #52) : c'est une capacité de l'owner dans sa session, pas un outil
//! d'agent. La route est relayée au collecteur, qui détient l'index et
//! applique le filtre de consentement.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::http::Gateway;
use crate::search::{SearchRequest, SearchRefusal};

pub fn routes() -> Router<Gateway> {
    Router::new().route("/api/search", get(search))
}

async fn search(
    State(gateway): State<Gateway>,
    headers: HeaderMap,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Response {
    let Some(searches) = gateway.searches() else {
        return refuse(SearchRefusal::CollectorNotConfigured);
    };
    let _ = &headers;
    let request = parse_query(query.as_deref().unwrap_or_default());
    match searches.search(&request).await {
        Ok(answer) => (StatusCode::OK, Json(answer)).into_response(),
        Err(refusal) => refuse(refusal),
    }
}

/// Les membres, lus à la main depuis la chaîne de requête — pas un
/// extracteur, pour la même raison que la lecture free/busy : la réponse
/// porte le refus du collecteur, pas celui d'un extracteur.
fn parse_query(query: &str) -> SearchRequest {
    let mut request = SearchRequest::default();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = crate::contacts_http::percent_decode(value);
        match name {
            "q" => request.q = value,
            "source" => request.source = Some(value),
            "from" => request.from = Some(value),
            "to" => request.to = Some(value),
            "limit" => request.limit = value.parse().ok(),
            _ => {}
        }
    }
    request
}

fn refuse(refusal: SearchRefusal) -> Response {
    (
        StatusCode::from_u16(refusal.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({ "error": refusal.code(), "detail": refusal.message() })),
    )
        .into_response()
}
```

- [ ] **Step 5 : Câbler dans `http.rs` et `main.rs`**

Dans `companion-gateway/src/http.rs` : ajouter `searches: Option<Arc<crate::search::Searches>>` à `Gateway` (initialisé `None`, avec `with_searches`/`searches()` calqués sur `with_reads`/`reads()`), et au router : `.merge(search_http::routes())`.

Dans `session_http.rs`, confirmer que `(&Method::GET, "/api/search")` tombe dans le `_ => Requirement::DeviceToken` par défaut (il le fait — aucune entrée nécessaire) ; ajouter un test dans le module de test de `session_http.rs` qui l'affirme :

```rust
    #[test]
    fn search_is_a_device_token_route() {
        assert_eq!(
            Requirement::DeviceToken,
            requirement(&Method::GET, "/api/search")
        );
    }
```

Dans `main.rs`, construire `Searches::new((collector_url, service_token), metrics)` avec le même couple que `Reads`, et `gateway.with_searches(...)`.

Ajouter à `Metrics` (gateway) : `record_search(&self, outcome: &str)` + émission `twalk_companion_gateway_searches_total{outcome}`.

- [ ] **Step 6 : Décrire la route dans `openapi.yaml`**

Ajouter sous `paths:` le path `GET /api/search` avec paramètres `q` (requis), `source`, `from`, `to`, `limit`, une réponse `200` (`SearchAnswer` : `hits` tableau d'objets, `count`, `withheld`, `withheld_reason` optionnel) et les réponses d'erreur `400`/`502`/`503` en `Error`. Ajouter le schéma `SearchAnswer`. `tests/openapi.rs` doit passer.

- [ ] **Step 7 : Lancer les tests**

Run: `cd companion-gateway && cargo test --test search && cargo test --test openapi`
Expected: PASS.

- [ ] **Step 8 : Régénérer le client du Companion et vérifier**

Run: `cd companion && npm run api:generate && npm run api:check`
Expected: génération OK, `api:check` vert.

- [ ] **Step 9 : Committer**

```bash
cd companion-gateway
git add src/search.rs src/search_http.rs src/http.rs src/main.rs src/metrics.rs src/session_http.rs openapi.yaml tests/search.rs tests/harness
git commit -m "The Gateway relays search behind the session guard and never reads a body

Co-Authored-By: Claude Code <noreply@anthropic.com>"
cd ../companion
git add src/lib/api
git commit -m "Regenerate the Gateway client for the search route

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 8 : L'écran `/search` du Companion

**Files:**
- Create: `companion/src/lib/search/model.ts` (pure : mise en forme des hits, la phrase des retraits, les bornes)
- Create: `companion/src/lib/search/load.ts` (le chargement)
- Create: `companion/src/lib/search/model.test.ts`
- Create: `companion/src/routes/search/+page.svelte`
- Modify: `companion/src/lib/i18n/fr.json` et `en.json`
- Create: `companion/tests/e2e/search/search.spec.ts`
- Modify: `companion/playwright.config.ts` (projet `search`, `dependencies: ['portals']`)

**Interfaces:**
- Consumes: le client généré `src/lib/api/` (Tâche 7), `$lib/i18n`, `$lib/api/client.ts` (wrapper 401).
- Produces:
  - `search/model.ts` : `rows(answer) -> Row[]`, `withheldCopy(answer, t) -> string | null`, `snippet(text, limit) -> string`.
  - Route `/search`.

- [ ] **Step 1 : Écrire le test qui échoue**

`companion/src/lib/search/model.test.ts` :

```ts
import { describe, it, expect } from 'vitest';
import { withheldCopy, rows } from './model';

describe('search results', () => {
	it('says the withdrawal when there is one, and stays silent when there is not', () => {
		// Le retrait est dit, pas caché (§9) : un contact révoqué n'est pas
		// une absence.
		const t = (key: string, values?: Record<string, unknown>) =>
			`${key}:${JSON.stringify(values)}`;
		expect(withheldCopy({ count: 2, withheld: 3 }, t)).toBe(
			'search.withheld:{"count":3}'
		);
		expect(withheldCopy({ count: 2, withheld: 0 }, t)).toBeNull();
	});

	it('renders a row per hit and never invents a body', () => {
		const rows_ = rows({
			hits: [
				{
					id: 'doc-1',
					source: 'mail-linagora',
					correspondent: 'mailto:alice@example.org',
					mailbox: 'inbox',
					date: 1756720800,
					subject: 'Point hebdo',
					snippet: 'Le point de la semaine…',
				},
			],
			count: 1,
			withheld: 0,
		});
		expect(rows_).toHaveLength(1);
		expect(rows_[0].correspondent).toBe('mailto:alice@example.org');
		expect(rows_[0]).not.toHaveProperty('body');
	});
});
```

- [ ] **Step 2 : Lancer pour voir échouer**

Run: `cd companion && npx vitest run src/lib/search/model.test.ts`
Expected: FAIL — module introuvable.

- [ ] **Step 3 : Écrire le modèle**

`companion/src/lib/search/model.ts` :

```ts
// Ce que l'écran de recherche sait d'un résultat, et rien de plus. Le
// modèle n'a aucun membre où un corps de message pourrait se loger : le
// collecteur ne rend qu'un extrait, et cette forme le dit.
export type Row = {
	id: string;
	source: string;
	correspondent: string;
	mailbox: string | null;
	date: number;
	subject: string;
	snippet: string;
};

export type Answer = {
	hits: Row[];
	count: number;
	withheld: number;
	withheld_reason?: string;
};

type Translate = (key: string, values?: Record<string, unknown>) => string;

export function rows(answer: Answer): Row[] {
	return answer.hits;
}

/// La phrase du retrait, ou `null` quand il n'y en a pas — jamais une
/// phrase vide, qui se lirait comme un retrait de zéro.
export function withheldCopy(answer: Answer, t: Translate): string | null {
	if (answer.withheld <= 0) return null;
	return t('search.withheld', { count: answer.withheld });
}
```

- [ ] **Step 4 : Lancer le test pour le voir passer**

Run: `cd companion && npx vitest run src/lib/search/model.test.ts`
Expected: PASS.

- [ ] **Step 5 : Écrire le chargement et la route**

`companion/src/lib/search/load.ts` — appel au client généré pour `GET /api/search`, via le wrapper `$lib/api/client.ts` (401 → refresh, §#111). Puis `companion/src/routes/search/+page.svelte` : une barre de recherche, la liste des `rows`, la phrase de retrait en tête quand elle existe, la borne dans la réponse. Nommer le correspondant (légitime ici, §9.3). Le clic construit un lien vers le mail (natif, pas dans Twalk).

- [ ] **Step 6 : Ajouter les chaînes i18n**

Dans `fr.json` et `en.json` : `search.title`, `search.placeholder`, `search.withheld` (`{count} résultats retenus par vos décisions de consentement` — **à faire relire par l'owner**), `search.empty`, `search.open`, `search.window`. Ne pas inventer de mot ; reproduire les termes du produit.

- [ ] **Step 7 : Le test Playwright**

`companion/tests/e2e/search/search.spec.ts` : démarre avec le stub de Gateway qui répond une recherche contenant un hit révoqué ; assert que la phrase de retrait s'affiche et que le révoqué n'est pas dans la liste. Ajouter le projet `search` à `playwright.config.ts` (`dependencies: ['portals']`, même origine que `consent`/`portals`).

- [ ] **Step 8 : Lancer Svelte-check, Vitest, et le build**

Run: `cd companion && npm run test`
Expected: `api:check` + `svelte-check` + Vitest + build verts. (Playwright e2e complet via `npm run test:e2e:stack`.)

- [ ] **Step 9 : Committer**

```bash
cd companion
git add src/lib/search src/routes/search src/lib/i18n tests/e2e/search playwright.config.ts
git commit -m "A search screen that says the withdrawal and never invents a body

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 9 : Déploiement, documentation, et `.env.example`

**Files:**
- Modify: `deploy/docker-compose/compose.yaml` (le service `collector`)
- Modify: `deploy/docker-compose/.env.example`
- Modify: `collector/README.md` (variables)
- Modify: `deploy/README.md` (runbook du chiffrement)
- Modify: `.github/ci/suites.json` (routage : `collector/tests/search.rs`)

**Interfaces:**
- Consumes: les variables `COLLECTOR_INDEX_KEY_FILE`, `COLLECTOR_INDEX_*`.
- Produces: le runbook, la documentation, la route CI.

- [ ] **Step 1 : La variable dans `.env.example`**

Dans `deploy/docker-compose/.env.example`, section collector :

```bash
# La clé de l'index de recherche (#XXX) : un fichier monté, jamais une
# variable. Sans elle, la recherche est désactivée (503) et rien n'est
# indexé. Le répertoire d'index doit être monté chiffré (gocryptfs) — voir
# deploy/README.md.
COLLECTOR_INDEX_KEY_FILE=
```

- [ ] **Step 2 : Le montage dans `compose.yaml`**

Sur le service `collector` : monter le répertoire d'index et la clé, calque de `COLLECTOR_OIDC_CLIENT_SECRET_FILE`. Poser `COLLECTOR_INDEX_KEY_FILE` seulement si défini (`${VAR:+…}`).

- [ ] **Step 3 : Le runbook dans `deploy/README.md`**

Une section : générer la clé, monter le volume `gocryptfs`, poser `COLLECTOR_INDEX_KEY_FILE`, vérifier que le disque ne voit que du chiffré. Nommer honnêtement ce que le chiffrement au repos protège et **ne protège pas** (spec §4.3).

- [ ] **Step 4 : La documentation `collector/README.md`**

Ajouter `COLLECTOR_INDEX_KEY_FILE` à la table des variables, avec la règle « sans clé, index désactivé ».

- [ ] **Step 5 : Le routage CI**

Dans `.github/ci/suites.json`, ajouter `collector/tests/search.rs` aux `targets` de la suite `collector`. Vérifier avec le test de routage :

Run: `cd /home/mmaudet/work/twalk && python3 .github/ci/test_selection.py` (ou la commande du routage — voir `docs/agents/continuous-integration.md`)
Expected: le routage passe.

- [ ] **Step 6 : Lancer la suite collector complète**

Run: `cd collector && cargo test`
Expected: PASS (y compris `search.rs`, `mail.rs`, `triage.rs` inchangés).

- [ ] **Step 7 : Committer**

```bash
git add deploy/docker-compose/.env.example deploy/docker-compose/compose.yaml deploy/README.md collector/README.md .github/ci/suites.json
git commit -m "The search index is a documented, keyed, encrypted-at-rest capability

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

## Self-Review

**Couverture de la spec :**
- §1 (pourquoi, la tension) → contexte du plan ; aucune tâche (c'est un argument, pas un livrable).
- §2 (l'index dans le collecteur, deux flux, backfill hors bus) → Tâches 1, 4.
- §3 (trait `Source`, document, dédoublonnage) → Tâches 2, 3.
- §4 (stockage, chiffrement, désactivé par défaut, ADR 0043) → Tâches 1, 9. **ADR 0043 non écrit** — voir lacune ci-dessous.
- §5 (contrat : routes, filtre consentement compté, refus) → Tâches 5, 6, 7.
- §6 (vectoriel, ANN séparé) → **hors de ce plan** (lot 3b), cohérent.
- §7 (périmètre, exclusions) → respecté (aucune persona, aucun modèle, aucune action automatique).
- §8.1 (définition du lot 3a) → toutes les tâches.
- §9 (écran) → Tâche 8.

**Lacune trouvée :** la spec §4.4 demande **ADR 0043**. Aucune tâche ne l'écrit. **Correction :** ajouté en Tâche 9 Step 5bis ci-dessous.

- [ ] **Tâche 9, Step 5bis : écrire `docs/architecture/adr/0043-...md`** — « L'archive de recherche est le seul détenteur à long terme des mots de tiers, et elle ne quitte jamais la session de l'owner. » Suivre le format des ADR existants (contexte, décision, conséquences). Lier ADR 0028, 0042, 0012, 0032.

**Placeholder scan :** les seuls éléments non finaux sont des notes d'implémentation explicites (`TantivyDocument` vs `Document`, `Owner::new("")` à remplacer par un paramètre `owner`). Chacun dit quoi faire. Aucun « TODO »/« TBD ».

**Cohérence des types :** `Document` (Tâche 2) ↔ `Writer::add(&Document)` (Tâche 3) ↔ `Hit` (Tâche 5) ↔ `SearchRequest`/`SearchAnswer` (Tâche 7) ↔ `Row`/`Answer` (Tâche 8). `MailSource::id()`, `filter_by_consent(hits, cache, connection)`, `run_once(...)` nommés une fois et réutilisés. La signature `run_once` est corrigée en Tâche 4 (passer `&Owner` plutôt que `Owner::new("")`).

**Review Focus :** les cinq classes ont chacune leur test — (1) Tâche 3, (2) Tâche 6, (3) Tâche 4, (4) Tâche 5, (5) Tâche 7.

**Hors périmètre confirmé :** 3b (vectoriel/embeddings), 3c (règles, fiche contact), 3d (pièces jointes), le brief OpenRAG (lane distincte).
