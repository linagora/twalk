# Design — La recherche dans Twalk : archive personnelle indexée par Tantivy

Session de spécification, 2026-10-04. Statut : **proposé**, en attente de revue de l'owner.

Ce document conçoit une capacité nouvelle de Twalk : **une recherche plein-texte et
sémantique sur l'archive personnelle de l'owner** — ses mails, puis ses messageries —
indexée par Tantivy, interrogée depuis le Companion, et gouvernée par les mêmes ADR qui
gouvernent tout le reste. Il part d'une lecture critique de la pré-spec **Berger v2**
(produit distinct, `~/work/berger/docs/berger-v2-pre-specification.pdf`) et la traduit en
enrichissements de Twalk plutôt qu'en un second produit.

---

## 1. Pourquoi, et ce que Berger v2 apporte

### 1.1 Le constat

Twalk sait **recevoir** un message, le normaliser en CloudEvent et le faire rédiger par une
persona. Il sait **décider** (consentement, observation, approbation). Il ne sait pas
**retrouver**. Une fois qu'un message est passé sur le bus, ADR 0037 le garde quatre-vingt-dix
jours et deux GiB, puis le jette au plus ancien. Un mail d'il y a trois ans, un fil WhatsApp
d'il y a six mois, une pièce jointe reçue l'an dernier : rien de tout cela n'est interrogeable.
L'owner doit ouvrir son client mail, son client WhatsApp, un par un, et se souvenir de qui a dit
quoi.

La pré-spec Berger v2 (lot « Recherche », lot « Règles ») pose exactement cette lacune pour le
seul courrier : **un index local, plein-texte puis sémantique, sur une archive de vingt-cinq
ans** ; et, plus loin, des **règles statistiques** dérivées de cette archive (« quand X, classer
en Y ») au lieu de règles écrites à la main. Les deux idées sont bonnes et concernent Twalk au
premier chef.

### 1.2 La tension à trancher — et comment elle se tranche

Le `brief-reprise-v2.md` identifie la question qui bloque tout : Berger v2 place un
**classifieur LightGBM et un LLM local au cœur de la décision** (`spam → boîte junk`), alors
qu'**ADR 0042 pose qu'aucun modèle n'est dans le chemin d'exécution du tri** : seules des
règles décident, un modèle peut tout au plus *proposer* une règle, que l'owner approuve.

La lecture de réconciliation que ce design retient, et qui vaut pour la recherche aussi :

> **Le modèle lit ; la règle, ou l'owner, agit. La recherche n'est pas une décision.**

Un index n'écrit rien sur le bus, ne classe rien, ne déplace rien. Il rend des documents *à la
demande de l'owner*, dans sa session. Il n'y a donc aucune tension entre la recherche et
ADR 0042 : chercher n'est pas décider. La tension ne renaîtrait que si l'on faisait de l'index
la source d'une **action automatique** (auto-filing, auto-suppression) — ce que ce design
**exclut explicitement** (§7, hors-périmètre). Le lot « Règles » de Berger, qui *agit*, sera un
chantier séparé et devra passer par ADR 0042 (proposition → approbation), pas par l'index.

### 1.3 Ce que « enrichir Twalk » veut dire ici

Trois enrichissements, dans l'ordre où ils paient :

1. **L'archive interrogeable** — la recherche elle-même, mails d'abord (§4, §5, §6).
2. **La fiche contact dérivée** — un contact n'est plus seulement `(subject, network,
   consentement)`, c'est aussi « combien de messages échangés, sur quels fils, depuis quand »,
   calculé comme facettes de l'index (§8.3). Twalk ne dit aujourd'hui rien de tel.
3. **La découverte de règles par requête** — l'owner cherche « tout ce qui vient de
   `noreply@…` », l'interface lui propose d'en faire une règle de tri ; la proposition suit le
   chemin ADR 0042 déjà en place (§8.3). C'est l'idée de Berger, mais l'owner reste l'auteur de
   la règle.

---

## 2. Où vit l'index — le collecteur grandit

**Décision : l'index Tantivy vit dans le `collector`, sur `COLLECTOR_STATE_DIR`.**

Le collecteur (`collector/`) est déjà le composant qui **lit les comptes propres de l'owner**
(ADR 0033) : un mailbox par JMAP, des calendriers par CalDAV, une connexion OIDC par compte
(ADR 0038). C'est le seul composant de Twalk qui détient déjà un moyen d'accès durable au
courrier de l'owner, qui sait ce qu'est une connexion, et qui possède un répertoire d'état
(`COLLECTOR_STATE_DIR`) où il écrit déjà des curseurs (`jmap/<connection>.json`,
`caldav/…`, `oidc/grant.json`) avec le patron `write_json_private` de `collector/src/fs.rs`
(JSON, mode 0600, écrit par renommage). L'index est **un artefact d'état de plus**, avec les
mêmes garanties de fichier.

Trois gestes fondent ce choix :

- **Le collecteur indexe ce qu'il lit déjà.** Il a déjà, dans `jmap.rs`, la lecture d'un mail
  (frontière, HTML réduit, pièces jointes) et dans `push.rs` un flux temps réel. Indexer ne
  demande pas un nouvel accès — cela demande d'écrire ce qu'il lit déjà dans Tantivy.
- **Le `Source` est une abstraction, pas un cas particulier.** Mails et messageries entrent par
  la même interface (§3), donc le lot 1 (courrier) et le lot 2 (messagerie) ne divergent pas.
- **Le collecteur ne publie rien de neuf sur le bus pour l'index.** L'indexation temps réel
  réutilise la lecture que `push.rs` fait déjà ; le backfill (§3.2) **n'écrit jamais sur le
  bus** — un backfill de vingt-cinq ans de mails qui publierait sur le `twalk` stream le ferait
  déborder (deux GiB, ADR 0037) et noierait tous les consumers.

### 2.1 Les deux flux

```
                      ┌──────────────────────────────────────────────┐
                      │  collector                                   │
   JMAP push  ───────▶│  push.rs ──▶ jmap.rs (lit le mail)           │
   (temps réel)       │                 │                            │
                      │                 ▼                            │
                      │            indexer.rs ──▶ Tantivy (state_dir)│
                      │                 ▲                            │
   backfill   ───────▶│  backfill.rs ───┘   (jamais sur le bus)      │
   (à la demande)     │                                              │
                      │  http.rs :  GET /search, GET /index/status   │
                      └──────────────────────────────────────────────┘
                                     ▲
                                     │  bearer = COLLECTOR_GATEWAY_SERVICE_TOKEN
                                     │
                      ┌──────────────┴───────────────────────────────┐
                      │  companion-gateway                           │
                      │  GET /api/search  (device token, session)    │
                      │  → relais vers collector, ne lit pas les corps│
                      └──────────────────────────────────────────────┘
                                     ▲
                                     │  device token
                      ┌──────────────┴───────────────────────────────┐
                      │  companion  /search                          │
                      └──────────────────────────────────────────────┘
```

Le chemin de lecture copie le patron déjà en place pour `freebusy` (#281) : la route interne
`GET /search` du collecteur est portée par le **même bearer** que la lecture du registre
(`COLLECTOR_GATEWAY_SERVICE_TOKEN`, `collector/src/http.rs`), et le Gateway la relaie par
`GATEWAY_COLLECTOR_URL` sans jamais ouvrir un corps de message. La différence avec `freebusy`
est qu'ici la route est appelée par un **navigateur** (l'owner), pas par Hermes : la route
publique du Gateway est donc derrière le guard de session (device token, #52), pas derrière la
signature Hermes.

---

## 3. Le modèle de source et le document indexé

### 3.1 L'abstraction `Source`

Une unité d'archive interrogeable est une **source**. Deux existent à terme — le courrier, la
messagerie — et l'interface est étroite pour que la seconde ne rouvre pas la première :

```rust
/// Une unité d'archive indexable : un compte que l'owner possède.
trait Source {
    /// L'identifiant de connexion, tel que le registre du Gateway le nomme
    /// (`email`, `whatsapp`, …). C'est aussi la clé de consentement du bus.
    fn id(&self) -> &str;

    /// Le document à indexer pour une unité native (un mail, un message).
    /// Rend `None` pour ce qui ne s'indexe pas (un message du owner lui-même,
    /// une pièce jointe sans texte extractible, un message non humain).
    fn document(&self, native: &Native) -> Option<Document>;

    /// Le texte extractible d'une pièce jointe, si la source sait le faire.
    /// Hors périmètre du lot 3a (§7).
    fn embedded(&self, _attachment: &Attachment) -> Option<String> {
        None
    }
}
```

Le backfill est **par source**, mais son moteur est commun : `backfill::run(source, depuis)`
parcourt la source, appelle `document()`, écrit dans l'index, avance un **curseur de backfill**
dans `state_dir` (`index/<source>.cursor.json`, même patron que `jmap/<connection>.json`) — le
curseur ne bouge qu'après que Tantivy a *commit*é, exactement comme le curseur JMAP ne bouge
qu'après que le bus a pris les événements.

### 3.2 Le document indexé

```rust
struct Document {
    /// L'id du bus quand il existe (`inbound.message.received.v1`), sinon
    /// l'id natif préfixé par la source (`jmap:…`, `matrix:…`). C'est la clé
    /// de jointure avec l'index vectoriel (§6).
    id: String,
    /// `email`, `whatsapp`, `signal`, `sms`, `telegram`, `discord`, `matrix`.
    source: String,
    /// Le correspondant, tel que le bus le nomme déjà : `mailto:` pour un
    /// mail, l'id Matrix pour une messagerie. **Jamais un nom d'affichage.**
    correspondent: Field,      // keyword + texte (les deux)
    /// La boîte/le dossier d'origine (INBOX, Sent exclu par construction, …).
    mailbox: Option<Field>,
    /// Le fil (References/JMAPA `threadId`, ou l'id de salon Matrix).
    thread: Option<Field>,
    /// Date d'envoi, en secondes — tri et plage.
    date: i64,
    /// Le sujet, plein-texte.
    subject: Field,
    /// Le corps, plein-texte.
    body: Field,
    /// Pièce jointe présente ? — facette.
    has_attachment: bool,
    /// L'owner a répondu ? — facette. Renseigné par la jointure bus, plus tard.
    replied: bool,
}
```

Deux règles de schéma, portées par le type et non par un commentaire :

- **Le correspondant est un `keyword` (facette, filtre exact) *et* un champ plein-texte**
  (pour « tous les mails de Dupont » et « mails mentionnant Dupont »). Il n'est **jamais** un
  nom d'affichage : le nom d'affichage d'un expéditeur est composé par lui (#110, ADR 0012).
- **Le document ne porte rien que le bus ne porte déjà** : pas de contenu décrypté d'une pièce
  jointe hors périmètre, pas de métadonnée OpenRAG, pas d'information dérivée d'un autre
  message. L'index est une projection **du même corpus** que le bus, pas un nouveau corpus.

### 3.3 Indexation temps réel vs backfill : une seule vérité par document

Une même unité native peut être vue deux fois — le push la pousse, le backfill la repasse. Le
**dédoublonnage est par `id` de document** : Tantivy indexe par `id` et un `update` sur le même
id remplace (Tantivy n'a pas d'`upsert` natif, mais `delete_term(id)` puis `add` dans la même
opération de commit suffit, et c'est ce que fera `indexer.rs`). L'`id` déterministe est déjà la
règle du produit (§ l'`id` de `inbound.message.received.v1` est `sha256("jmap:" + accountId +
":" + emailId)`) : il n'y a donc **jamais deux documents pour un même mail**, que l'ordre
d'arrivée soit push-puis-backfill ou l'inverse.

---

## 4. Stockage, rétention, sécurité

### 4.1 Le problème, dit sans détour

C'est le point le plus sensible de tout ce design.

- ADR 0028 est explicite : **le bus est le seul endroit du déploiement où le texte en clair des
  messages d'autres personnes existe au repos**, et il le garde sept jours (identité quatre-vingt-
  dix jours, ADR 0027).
- Un index de vingt-cinq ans de mails est **le plus long détenteur de mots de tiers de tout
  Twalk** — l'inverse exact de cette règle. C'est un fait, pas une objection : l'archive
  appartient à l'owner et c'est lui qui la veut interrogeable. Mais il faut que ce soit un fait
  *nommé* et *gardé*, pas un effet de bord.

### 4.2 Les garde-fous

**Décision : l'index est chiffré au repos par un volume monté, la clé hors du serveur,
l'index désactivé par défaut sans clé.**

1. **Chiffrement au repos par `gocryptfs`.** Le répertoire d'index est un point de montage
   `gocryptfs` ; le conteneur ne voit que le clair monté, le disque ne voit que du chiffré. La
   clé est `COLLECTOR_INDEX_KEY_FILE` (un fichier monté, mode 0600, jamais une variable —
   leçon #239), monté par `collector-entrypoint.sh` comme le secret OIDC
   (`COLLECTOR_OIDC_CLIENT_SECRET_FILE`).
2. **Désactivé par défaut.** Sans `COLLECTOR_INDEX_KEY_FILE`, **aucun index n'est ouvert**, la
   route `/search` répond `503 index_not_configured`, et rien n'est écrit. Une capacité qui
   détient vingt-cinq ans de mots de tiers ne s'active pas par oubli.
3. **Jamais atteignable par une persona.** L'index n'est pas sur le bus, n'est pas dans
   `HERMES_PERSONAS`, et la route `/search` du collecteur exige le bearer de service — qu'aucune
   persona ne détient (`environment` ne l'injecte pas, #184). ADR 0032 : Twalk gouverne ce qu'un
   agent extérieur peut voir ; la persona ne voit pas l'index.
4. **Le Companion ne sert que des extraits, jamais des corps entiers.** (§5.3.)

### 4.3 Ce que le chiffrement ne protège pas — dit honnêtement

Le chiffrement au repos protège un disque volé, une sauvegarde, un snapshot. **Il ne protège
pas un processus compromis en cours d'exécution** : tant que le conteneur tourne, le clair est
monté. Twalk **nomme l'exigence** (le répertoire d'index doit être chiffré) mais **ne peut pas
la garantir** : c'est le déploiement qui monte `gocryptfs`. Le runbook de déploiement (§9) et
un ADR (§5.4) disent cela sans le maquiller.

### 4.4 ADR 0043 — à écrire

Ce design justifie un ADR propre, dans la lignée d'ADR 0028 mais pour l'archive :

> **ADR 0043 — L'archive de recherche est le seul détenteur à long terme des mots de tiers, et
> elle ne quitte jamais la session de l'owner.** L'index est chiffré au repos, sa clé vit hors
> du serveur, il n'est jamais lisible par une persona ni publié sur le bus, et le Companion
> n'en sert que des extraits bornés. Il n'agit sur rien : chercher n'est pas décider (ADR 0042).

---

## 5. Le contrat : de la route interne à l'écran

### 5.1 Route interne du collecteur

```
GET /search?q=<texte>&source=<optionnel>&from=<optionnel>&to=<optionnel>&limit=<défaut 20>
```

- Émise par le Gateway, **bearer = `COLLECTOR_GATEWAY_SERVICE_TOKEN`** (le même que la lecture
  du registre) — c'est le patron `collector/src/http.rs` existant.
- Rend une liste de **hits** : `{ id, source, correspondent, mailbox, date, subject_snippet,
  score }`. **Jamais le corps.**

`GET /index/status` rend l'état d'indexation par source (documents indexés, curseur de
backfill, en cours ou non) — pour le backfill (§6, écran de progression).

### 5.2 Route publique du Gateway

```
GET /api/search?q=…&source=…&from=…&to=…&limit=…
```

- Derrière le **guard de session** (device token, #52), comme toute route `/api`.
- **Le Gateway ne lit aucun corps de message** : il relaie la requête au collecteur et rend la
  réponse telle quelle. C'est le patron `hermes_freebusy_http.rs` (relais), à la différence près
  du guard.
- Décrite dans `companion-gateway/openapi.yaml` (obligatoire : `tests/openapi.rs` échoue sur une
  route non décrite).

### 5.3 Le filtre de consentement — appliqué côté collecteur, compté

**Décision (reco retenue) : le filtre de consentement est appliqué dans le collecteur, et les
hits retirés sont comptés.**

Le collecteur partage déjà `consent-cache` (`collector/src/consent.rs`, lot 2 de #251) : il lit
le snapshot du Gateway puis suit `consent.state.changed`. Il peut donc, pour chaque hit, savoir
si le correspondant est `granted`, `pending` ou `revoked` sur la source concernée — sans
nouvelle dépendance.

- Un correspondant **`revoked`** : ses hits sont **retirés du résultat** (pas de snippet, pas
  de sujet — le message réduit d'ADR 0012 dit que le texte d'un contact révoqué n'est pas
  publié, et l'archive suit la même règle).
- Le retrait est **compté** : `twalk_collector_search_hits_withheld_total{reason}`. C'est la
  règle du produit — *une silence est le seul échec que ce produit a livré plusieurs fois sans
  le voir* — appliquée à la recherche : l'owner qui cherche et ne voit pas un contact révoqué
  doit pouvoir lire, s'il le demande, que ce n'est pas une absence mais un retrait.
- Le Gateway **relaie le compte** et l'écran l'affiche (§6.3 : « 3 résultats retenus par vos
  décisions de consentement »).

Ce qu'**une persona ne peut pas faire** : appeler `/search`. Le bearer de service n'est pas
dans son environnement. La recherche est **une capacité de l'owner dans sa session**, pas un
outil d'agent.

### 5.4 Vocabulaire de refus

Aligné sur le patron existant (`freebusy`, `suggestions`) :

| Code | Statut | Sens |
|---|---|---|
| `index_not_configured` | 503 | pas de clé, pas d'index (§4.2) |
| `index_unavailable` | 503 | l'index existe mais ne s'ouvre pas |
| `invalid_query` | 400 | `q` vide ou trop long |
| `invalid_window` | 400 | `from > to`, ou fenêtre hors bornes |

Le patron `window.reached_start_of_stream` du listing des suggestions (la borne **dans la
réponse**, jamais seulement dans la config) s'applique ici aussi : une recherche bornée le dit.

---

## 6. Le vectoriel : un index ANN séparé

### 6.1 Le fait qui commande la conception

**Tantivy n'a pas de recherche vectorielle.** Vérifié en 0.20–0.26 : pas de `VectorField`, pas
de `VectorQuery`, pas de feature `vector` ; la demande amont (#815) est classée « Not planned ».
Seul le fork ParadeDB (`pg_search`) l'ajoute.

Conséquence : le vectoriel **ne peut pas** être un champ Tantivy. Il est un **second index**,
une base ANN, joint au premier par l'`id` du document.

### 6.2 La forme

- **Index ANN** : `hnsw_rs` (HNSW, pur Rust) ou `usearch`. Recommandation : `hnsw_rs`, plus
  simple, sans dépendance C++. Stocké dans `state_dir` à côté de Tantivy, même chiffrement.
- **Jointure par `id`** : la recherche ANN rend des `id` ; on va chercher ces `id` dans
  Tantivy pour le texte, le correspondant, la date — **une seule vérité de document**.
- **Fusion BM25 + vecteur** : somme pondérée des scores (paramètre de déploiement), ou
  reciprocal-rank-fusion. Recommandation : RRF, qui ne demande pas de normaliser des scores
  d'échelles différentes.
- **Où sont produits les embeddings** : question ouverte — soit le déploiement fournit un
  endpoint d'embeddings, soit c'est un modèle local. **Décision reportée au lot 3b** (§8.2), et
  la configuration suit le patron du modèle de chat (ADR 0015) : **un réglage séparé**, comme
  le chat, pas un champ ajouté à la config chat.

### 6.3 Ce que 3a fait sans vectoriel

3a indexe et cherche en **plein-texte Tantivy (BM25) seul**. Le vectoriel est 3b. C'est la
séquence validée : « 3a et 3b séquencés ». Un 3a livré est une recherche par mots-clés
parfaitement utilisable ; 3b ajoute « trouver sans connaître les mots ».

---

## 7. Périmètre et exclusions

**Dans le périmètre** : index Tantivy, backfill reprenable, route interne + relais Gateway,
écran `/search`, filtre de consentement compté, chiffrement au repos, enrichissements Twalk
(§8.3), pièces jointes (§8.4).

**Hors du périmètre, explicitement** :

- **Aucune action automatique depuis l'index.** Pas d'auto-filing, pas d'auto-suppression, pas
  de « règle qui se déclenche ». Chercher n'est pas décider (§1.2) ; le lot « Règles » de
  Berger, qui agit, est un chantier séparé soumis à ADR 0042.
- **Aucun modèle dans le chemin de la recherche.** Pas de LLM qui reformule la requête, pas de
  LLM qui résume les résultats. La recherche rend des documents ; l'owner lit. (Le vectoriel
  utilise un *modèle d'embedding*, pas un LLM générateur — c'est une transformation, pas un
  jugement.)
- **Aucune configuration d'embeddings dans ce lot.** Elle arrive en 3b, avec son propre réglage.
- **Aucun accès par une persona.** (§4.2, §5.3.)
- **Aucune lecture OpenRAG ici.** Voir §10 : c'est une autre lane.

---

## 8. Découpage en lots

### 8.1 — 3a : Tantivy plein-texte, backfill, seam, écran

- `collector/src/indexer.rs` — ouverture/écriture Tantivy, `Document`, dédoublonnage par `id`,
  commit.
- `collector/src/backfill.rs` — moteur reprenable par source, curseur, **jamais sur le bus**.
- `collector/src/search_index.rs` — la requête BM25, les facettes.
- `Source` trait + `MailSource` (le seul `Source` de 3a).
- `collector/src/http.rs` — `GET /search`, `GET /index/status` (patron existant).
- `companion-gateway/src/search_http.rs` + `search.rs` — relais + refus, `openapi.yaml`.
- `companion/src/lib/search/` + route `/search` — écran, facettes, extraits, mention des
  retraits, bornes.
- Tests : `collector/tests/search.rs` (index + backfill + filtre consentement compté),
  `companion-gateway/tests/search.rs` (relais, refus), `companion/tests/e2e/search/`.

**Livrable** : l'owner cherche dans ses mails par mots-clés, depuis le Companion, dans sa
session ; un contact révoqué disparaît et le compte est dit.

### 8.2 — 3b : hybride

- **Réglage d'embeddings séparé** — route Gateway `/api/settings/embeddings` (calque du réglage
  du chat, `settings.rs`), l'endpoint et le modèle d'embedding, write-only pour le credential.
- Index ANN (`hnsw_rs`) + jointure par `id` + fusion RRF.
- Backfill des embeddings (reprise du backfill texte, même curseur étendu).
- L'écran gagne un mode « approchant » (sémantique) en plus du mode « exact ».

**Livrable** : « trouver le mail qui parlait du problème de facturation » sans connaître les
mots.

### 8.3 — 3c : les enrichissements Twalk

- **Découverte de règles par requête** : une recherche peut être convertie en **proposition de
  règle** (ADR 0042) — l'owner cherche `from:noreply@… AND older_than:90d`, l'interface propose
  « classer les mails de cet expéditeur après 90 jours » ; la proposition suit le chemin déjà en
  place (`POST /_twalk/hermes/mail-rule-proposals`), l'owner approuve.
- **Fiche contact dérivée** : les facettes de l'index (`correspondent` × `source` × années)
  alimentent une vue « ce contact dans l'archive » — combien, quand, quels fils. Calculé à la
  requête, jamais stocké ailleurs.

**Livrable** : la recherche nourrit les règles, et une fiche contact existe.

### 8.4 — 3d : pièces jointes

- Extraction du texte des pièces jointes (PDF, texte, office) et indexation du texte extrait
  comme champ séparé (recherche `in:attachment`), jamais du binaire.
- C'est `Source::embedded()` qui se remplit ici ; 3a et 3b le laissent à `None`.

**Livrable** : chercher dans le contenu des pièces jointes.

### 8.5 — 4 : sécurité, déploiement

- `gocryptfs`, `COLLECTOR_INDEX_KEY_FILE`, montage dans `collector-entrypoint.sh`. Sans clé, le
  collecteur **démarre quand même** avec l'index désactivé (`503 index_not_configured`, §4.2) —
  jamais un refus de démarrage, qui ferait d'une capacité optionnelle une panne du courrier.
- Runbook `deploy/README.md` : générer la clé, monter le volume, vérifier le chiffrement.
- ADR 0043 (§4.4).
- `/health`, métriques (`twalk_collector_search_*`, `twalk_collector_index_documents`).

Lot **transverse mais séquencé après 3a** : on ne peut chiffrer proprement un index que quand
l'index existe.

---

## 9. Écran `/search` — `companion/src/lib/search/`

Quatre décisions, chacune un garde-fou :

1. **Le retrait est dit, pas caché.** En tête du résultat : « N résultats, dont M retenus par
   vos décisions de consentement » (§5.3). Un contact révoqué n'est pas une absence.
2. **La borne est dite.** Si la recherche a atteint la borne de fenêtre, l'écran le dit — patron
   `window.reached_start_of_stream`.
3. **Nommer le correspondant est légitime ici.** C'est la **première** surface du produit où un
   nom de contact s'affiche : l'écran d'approbation ne peut pas le faire (#97, ADR 0012), parce
   qu'il n'a que l'id et le type du déclencheur. Ici, l'owner cherche *ses* messages dans *sa*
   session : lui montrer qui a écrit est la fonction de l'écran, pas une fuite. Le texte des
   extraits, lui, reste borné (un extrait, pas le corps).
4. **Cliquer ouvre le mail dans le client mail, pas dans Twalk.** Twalk n'est pas un client de
   courrier ; il rend la trace, pas le document. Le lien est `twalk://` ou l'identifiant natif
   selon le client.

i18n fr/en (catalogues existants), Playwright `companion/tests/e2e/search/`, jamais de
formateur.

---

## 10. Articulation avec Twalk — et avec le brief OpenRAG

### 10.1 Les ADR qui gouvernent ce design

| ADR | Ce qu'il impose ici |
|---|---|
| 0028 | le bus détient le clair des tiers 7 j ; l'index est l'exception **nommée** (§4.4) |
| 0012 | un message réduit n'a pas de corps ; le filtre consentement retire le hit (§5.3) |
| 0010 | snapshot puis deltas — le collecteur lit le snapshot du Gateway (§5.3) |
| 0018/0021 | l'owner n'est jamais un contact ; ses propres messages ne sont pas « indexés comme tiers » |
| 0033 | email réutilise les types message ; le `Source` mail est un compte de l'owner |
| 0037 | le bus garde 90 j / 2 GiB ; **le backfill n'y écrit jamais** (§2) |
| 0038 | une connexion = un grant OIDC ; l'index est un consommateur de plus de la lecture |
| 0042 | aucun modèle dans le chemin d'exécution ; **chercher n'est pas décider** (§1.2) |
| 0032 | Twalk gouverne ce qu'un agent peut voir ; l'index n'est pas offert aux personas (§4.2) |
| 0039 | grade les lectures de l'agent ; la recherche **n'est pas** une lecture gouvernée d'agent |

### 10.2 Ce design **n'est pas** le brief OpenRAG

Le brief `docs/specs/brief-20261004-lecture-gouvernee-openrag-via-le-companion-gateway.md`
(écrit par une session parallèle) et ce design touchent tous deux « la recherche », et il faut
les garder distincts :

| | **Ce design** | **Brief OpenRAG** |
|---|---|---|
| Corpus | l'archive personnelle de l'owner **dans Twalk** (mails, messageries reçus par le collecteur) | le corpus documentaire de l'owner **dans OpenRAG** (documents tiers, index Milvus) |
| Qui lit | **l'owner**, dans sa session | **l'agent Hermes**, qui rédige |
| Rôle | capacité du Companion | **troisième lecture gouvernée** (ADR 0039) |
| Index | Tantivy **local**, dans Twalk | OpenRAG **externe**, déjà indexé |
| Journal | non (l'owner lit ses propres données) | oui (`hermes_read`) |

Ce sont **deux lanes indépendantes**. Le brief OpenRAG est une lecture signée
`Hermes → Gateway → OpenRAG` ; ce design est une lecture d'owner
`navigateur → Gateway → collecteur → Tantivy`. Aucune dépendance de l'un à l'autre.

Un point de contact **possible, plus tard, pas dans ce design** : les deux pourraient partager
la fonction de *gouvernance* (un index local ET un corpus externe exposés par la même
architecture de route signée). Mais la gouvernance OpenRAG est signée-Hermes (l'agent), et
celle-ci est session-owner : les fusionner reviendrait à donner à l'agent l'accès à l'index des
mails de l'owner, ce que §4.2 refuse. **À ne pas fusionner.** Si l'owner veut, un jour, que
l'agent cherche dans l'archive, c'est une **quatrième lecture gouvernée** distincte, avec son
ADR, son argument et son journal — exactement le coût qu'ADR 0039 impose.

---

## 11. Questions laissées ouvertes (pour le plan ou 3b)

1. **Embeddings** : endpoint fourni par le déploiement, ou modèle local ? (3b)
2. **HNSW vs usearch** : dépendance pure-Rust contre C++ plus rapide. (3b)
3. **Chercher dans les messageries** : le `Sensor` est la source, pas le collecteur ; le
   `Source` Matrix lit-il le bus (rétention 90 j) ou le homeserver (archive complète) ?
   Probablement le homeserver, avec le même patron `backfill` **hors bus**. (lot suivant)
4. **Index multi-source, un ou plusieurs ?** Recommandation : **un index Tantivy par
   déploiement**, le champ `source` comme facette — une seule base, des facettes.
5. **Résultats par fil** : regrouper les hits par `thread` dans l'écran ? (3a ou 3c)

---

## 12. Contexte de la session

- Pré-spec Berger v2 : `~/work/berger/docs/berger-v2-pre-specification.pdf` (11 pages, FR).
- Brief de reprise : `~/work/berger/docs/brief-reprise-v2.md` (question ouverte §1).
- ce design : dérive de la lecture critique de la pré-spec, validé section par section en
  conversation le 2026-10-04.
- Brief OpenRAG (lane distincte) :
  `docs/specs/brief-20261004-lecture-gouvernee-openrag-via-le-companion-gateway.md`.
- Patrons copiés : `collector/src/http.rs`, `collector/src/fs.rs`,
  `companion-gateway/src/hermes_freebusy_http.rs`, `companion-gateway/src/settings.rs`.
