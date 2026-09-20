# The clerk (lot 1, #265) — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A fifth Twalk component, the clerk (*le greffier*), that writes onto the owner's Buzz relay what the bus says — one forum post per suggestion in `approbations`, one line per posted reply in `journal`, and only what calls for a look in `activite` — under a Nostr key of its own, holding no state, deleting a suggestion's post when it expires.

**Architecture:** A standalone Cargo package `clerk/` (no workspace in this repo) with pure modules (`config`, `text`, `reference`, `metrics`) and a binary that wires three durable JetStream pull consumers and a sweep timer to a small Buzz relay client (`relay.rs`, the `nostr` crate, NIP-98 over `POST /events` and `POST /query`). The relay is the clerk's memory: what it already posted is found by querying its own posts and reading the reference line each carries. Tests run at the process boundary against a **real Buzz relay** (relay + Postgres + Redis) the suite brings up itself beside the shared Synapse/NATS stack.

**Tech Stack:** Rust 2021, tokio, async-nats 0.50, axum 0.8 (health/metrics), reqwest 0.12 (rustls), `nostr` 0.44 (keys, events, NIP-98), serde/serde_json, tracing; Docker (`ghcr.io/block/buzz:main`, `postgres:17-alpine`, `redis:7-alpine`) for tests; bash for the two operator scripts.

**Spec:** GitHub issue #265 (canonical; mirror in `.scratch/clerk/ticket-265.md`), decided against ADR 0032, #218, #219, #216. The write half is #284 and is **out of scope** here: no Gateway credential, no approval, no `delivery` line.

## Global Constraints

- Vocabulary (`CONTEXT.md`): **clerk** / *le greffier*; never "herald", "bot", "agent" for it. **Hermes** is Nous Research's runtime; the component in `hermes/` is the **persona runtime**.
- The clerk holds **no store**, **no Gateway credential** (no service token, no device token), **no key of Hermes's**, and **never quotes a contact**: a post body is `suggestion.body` plus the clerk's own sentences, nothing else (no display name, excerpt, `network_identifier`, room id, Matrix user ID).
- Every metric is `twalk_clerk_*`, hand-rolled Prometheus text (no metrics crate), every labelled outcome rendered at zero (`sensor/src/metrics.rs` is the model).
- Config from env: absent or empty = unset (`hermes/src/config.rs::optional`); refusals are long operator-facing sentences naming the variable and the way out.
- Languages: `CLERK_USER_LANGUAGE` ∈ `["en","fr","it","es","de"]` (`hermes/src/config.rs::USER_LANGUAGES`), unset = `en` with a warning; sentences exist in `fr` and `en`; `it`/`es`/`de` fall back to `en` with a startup warning naming it.
- Relay URL: `CLERK_RELAY_URL` is the URL the relay **announces** (`RELAY_URL`); NIP-98 `u` tags use it; the `Host` the relay sees must be its authority (multi-tenant by `Host`).
- Style: `cargo fmt` for Rust (the only formatter allowed), tabs and single quotes never apply (no TS here). Match the file you edit. No `.env` committed.
- Tests: process boundary, `twalk-test-harness` as dev-dependency, unique stream/subject prefix per run, `validate_against_contract` on every event the test publishes, `poll_until` for waits. Suites bring up their own relay stack under `TWALK_CLERK_TEST_STACK` (default `twalk-clerk-test`) on `TWALK_CLERK_TEST_RELAY_PORT` (default `17800`).
- CI: `.github/ci/suites.json` must route `clerk/` (and `tests/harness/` → clerk), `clerk/README.md` goes to `ignored`, and `test_selection.py`'s component tuples gain `"clerk"`; `cd .github/ci && python3 -m unittest discover -s . -p 'test_*.py'` must pass.
- Commit messages: title = one sentence about the outcome, body = why, `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>` last.

---

## File structure

```
clerk/
├── Cargo.toml                      # twalk-clerk; deps listed with a reason each
├── README.md                       # every variable, the decisions, how to run the suite
├── src/
│   ├── lib.rs                      # pub mod config; text; reference; metrics; relay; events
│   ├── config.rs                   # Config::from_env, validate, USER_LANGUAGES, refusals
│   ├── reference.rs                # the reference line: format / parse (pure)
│   ├── text.rs                     # every sentence the clerk writes, fr + en (pure)
│   ├── events.rs                   # what the clerk reads off a bus message: Suggestion, PostedReport, Activity (typed, no `data` beyond what is used)
│   ├── metrics.rs                  # counters + render()
│   ├── relay.rs                    # Buzz client: publish(kind, tags, content), query(filters), NIP-98
│   └── main.rs                     # wiring: consumers, sweep, axum /health /metrics, shutdown
└── tests/
    ├── harness/
    │   ├── mod.rs                  # re-exports twalk-test-harness + RelayStack + ClerkProc + Owner
    │   ├── relay.rs                # RelayStack: compose up/down, owner key, add_member(9030), create_channel(9007), query(), events_in(channel)
    │   └── clerk.rs                # ClerkProc: start the binary with env, logs, wait_for_log
    ├── compose.relay.yaml          # relay + postgres + redis, minimal env, mem limits
    ├── smoke.rs                    # stack up, clerk starts, /health, /metrics at zero
    ├── approbations.rs             # one post per suggestion; no second on redelivery/restart; expired skipped; no contact word on the relay
    ├── journal.rs                  # .posted → journal line, reach visible, no text
    ├── activite.rs                 # bridge/consent/suggestion lines; thinking ignored; no Matrix ID
    └── sweep.rs                    # expired post deleted within the sweep; activite says so
deploy/docker-compose/
├── compose.yaml                    # + `clerk` service
├── clerk.Dockerfile                # like hermes.Dockerfile + curl, USER clerk
├── .env.example                    # + Clerk section
├── provision-nostr-key.sh          # renamed from provision-hermes-nostr-key.sh (symlink keeps the old name)
└── provision-buzz-channels.sh      # + --bot <pubkey> (repeatable), prints CLERK_CHANNEL_* lines
deploy/README.md, AGENTS.md, CONTEXT.md, docs/architecture/adr/0035-the-clerk-is-a-projection-of-the-relay.md
.github/ci/suites.json, .github/ci/test_selection.py
```

**Dependency order:** T1 (skeleton) → {T2 relay client, T3 text+reference, T4 events+metrics, T5 test relay stack, T8 scripts+docs+CI+ADR} in parallel → T6 (wiring) → T7 (integration suites) → T9 (deployment service) → T10 (README + final).

---

### Task 1: The package skeleton, config and the refusals

**Files:**
- Create: `clerk/Cargo.toml`, `clerk/src/lib.rs`, `clerk/src/config.rs`, `clerk/src/main.rs` (minimal: config + tracing + "clerk starting" + shutdown)
- Create: `clerk/.gitignore` (`/target`)

**Interfaces:**
- Produces: `clerk::config::Config` with fields below; `Config::from_env() -> anyhow::Result<Config>`; `pub const USER_LANGUAGES: [&str; 5]`; `Config::bus_subject(event_type: &str) -> String` (`fr.linagora.twalk.X` → `<prefix>.X`).

```rust
pub struct Config {
    pub nats_url: String,            // CLERK_NATS_URL, default nats://localhost:4222
    pub stream: String,              // CLERK_STREAM, default "twalk"
    pub subject_prefix: String,      // CLERK_SUBJECT_PREFIX, default "twalk"
    pub relay_url: String,           // CLERK_RELAY_URL, required; http(s)://…, no trailing slash, no path
    pub nostr_key_file: PathBuf,     // CLERK_NOSTR_KEY_FILE, required; one line, 64 hex or nsec
    pub channel_approvals: String,   // CLERK_CHANNEL_APPROVALS, required, a UUID
    pub channel_activity: String,    // CLERK_CHANNEL_ACTIVITY, required
    pub channel_journal: String,     // CLERK_CHANNEL_JOURNAL, required
    pub user_language: String,       // CLERK_USER_LANGUAGE, default "en", one of USER_LANGUAGES
    pub listen: SocketAddr,          // CLERK_LISTEN, default 127.0.0.1:8084 (health + metrics)
    pub sweep: Duration,             // CLERK_SWEEP_SECONDS, default 60, min 1
    pub log_level: String,           // CLERK_LOG_LEVEL, default "info"
}
```

- [ ] **Step 1: Cargo.toml** — copy `hermes/Cargo.toml`'s shape (package `twalk-clerk`, lib + bin `twalk-clerk`), deps with one-line reasons: `anyhow`, `async-nats = "0.50"`, `axum = "0.8"`, `futures`, `nostr = { version = "0.44", features = ["nip98"] }`, `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }`, `serde = { version = "1", features = ["derive"] }`, `serde_json`, `sha2 = "0.10"`, `time = { version = "0.3", features = ["parsing", "formatting"] }`, `tokio` (full), `tracing`, `tracing-subscriber` (env-filter), `uuid = { version = "1", features = ["v4"] }`. Dev: `anyhow`, `reqwest` (json, rustls-tls), `serde_json`, `tokio`, `nostr` (same features), `twalk-test-harness = { path = "../tests/harness" }`.

- [ ] **Step 2: write the failing config tests** in `clerk/src/config.rs` `#[cfg(test)]` — use a helper that builds `Config` from a `&[(&str, &str)]` map (`Config::from_vars(vars: &dyn Fn(&str) -> Option<String>)`, with `from_env` calling it on `std::env::var`), so tests never touch the process environment:

```rust
fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + '_ {
    move |name| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string()).filter(|v| !v.is_empty())
}
const FULL: &[(&str, &str)] = &[
    ("CLERK_RELAY_URL", "http://127.0.0.1:17800"),
    ("CLERK_NOSTR_KEY_FILE", "/run/secrets/clerk.key"),
    ("CLERK_CHANNEL_APPROVALS", "9b1ba94a-38c3-49fe-9eb0-ffaafa62571a"),
    ("CLERK_CHANNEL_ACTIVITY", "2487096b-23e1-45cb-a611-402a5fae439a"),
    ("CLERK_CHANNEL_JOURNAL", "29a57768-7513-43bc-9cc2-6915453467f4"),
];
#[test] fn a_full_configuration_is_accepted_with_the_defaults() { let c = Config::from_vars(&vars(FULL)).unwrap(); assert_eq!(c.user_language, "en"); assert_eq!(c.sweep, Duration::from_secs(60)); assert_eq!(c.listen.to_string(), "127.0.0.1:8084"); }
#[test] fn every_required_variable_is_named_when_missing() { for (name, _) in FULL { let rest: Vec<_> = FULL.iter().filter(|(k, _)| k != name).cloned().collect(); let err = Config::from_vars(&vars(&rest)).unwrap_err().to_string(); assert!(err.contains(name), "{err}"); } }
#[test] fn the_relay_url_is_the_announced_one_without_a_path() { for bad in ["127.0.0.1:17800", "http://127.0.0.1:17800/", "http://127.0.0.1:17800/events", "ws://x"] { let mut v = FULL.to_vec(); v[0] = ("CLERK_RELAY_URL", bad); let err = Config::from_vars(&vars(&v)).unwrap_err().to_string(); assert!(err.contains("CLERK_RELAY_URL") && err.contains("announces"), "{bad}: {err}"); } }
#[test] fn a_channel_must_be_a_uuid() { let mut v = FULL.to_vec(); v[2] = ("CLERK_CHANNEL_APPROVALS", "approbations"); let err = Config::from_vars(&vars(&v)).unwrap_err().to_string(); assert!(err.contains("CLERK_CHANNEL_APPROVALS") && err.contains("UUID")); }
#[test] fn the_language_is_one_of_the_companions_five() { for bad in ["fr-FR", "FR", "pt"] { let mut v = FULL.to_vec(); v.push(("CLERK_USER_LANGUAGE", bad)); let err = Config::from_vars(&vars(&v)).unwrap_err().to_string(); assert!(err.contains("en, fr, it, es, de") && err.contains(bad)); } }
#[test] fn the_sweep_is_at_least_a_second() { let mut v = FULL.to_vec(); v.push(("CLERK_SWEEP_SECONDS", "0")); assert!(Config::from_vars(&vars(&v)).unwrap_err().to_string().contains("CLERK_SWEEP_SECONDS")); }
#[test] fn bus_subjects_follow_the_prefix() { let c = Config::from_vars(&vars(FULL)).unwrap(); assert_eq!(c.bus_subject("fr.linagora.twalk.persona.suggest.produced.v1"), "twalk.persona.suggest.produced.v1"); }
```

- [ ] **Step 3: run** `cd clerk && cargo test --lib config` → fails to compile.
- [ ] **Step 4: implement** `config.rs` — helpers `optional/required/number` as in `hermes/src/config.rs:539-575` but taking the `vars` closure; refusal wording modelled on Hermes's (`CLERK_USER_LANGUAGE is the language the clerk writes in … must be one of {}, spelled as the Companion writes it; got {:?}`; `CLERK_RELAY_URL is the URL the relay announces (its RELAY_URL) — http:// or https://, host and port only — because NIP-98 binds every signed request to it and the relay is multi-tenant by host; got {:?}`). UUID check: 36 chars, hyphens at 8/13/18/23, hex elsewhere (no uuid parse needed, but `uuid::Uuid::parse_str` is fine).
- [ ] **Step 5: `main.rs`** minimal: `Config::from_env()?`, tracing init with `config.log_level`, `info!(relay = %config.relay_url, stream = %config.stream, "clerk starting")`, `shutdown_signal().await` (copy `hermes/src/main.rs:744-753`), `info!("clerk stopped")`.
- [ ] **Step 6: run** `cargo test --lib` → pass; `cargo build` → ok.
- [ ] **Step 7: commit** `A fifth package: the clerk's configuration and its refusals`.

---

### Task 2: The relay client (`relay.rs`)

**Files:** Create `clerk/src/relay.rs`; modify `clerk/src/lib.rs` (`pub mod relay;`).

**Interfaces (produces):**

```rust
pub struct Relay { base: String /* announced URL, no trailing slash */, keys: nostr::Keys, http: reqwest::Client }
pub struct Published { pub event_id: String, pub accepted: bool, pub message: String }
#[derive(Debug, thiserror-free)] pub enum RelayError { Unreachable(String), Refused { status: u16, body: String }, Malformed(String) }
impl RelayError { pub fn is_transient(&self) -> bool /* Unreachable, or Refused with 429/5xx */ }
impl Relay {
    pub fn new(base: &str, keys: nostr::Keys) -> anyhow::Result<Self>;
    pub fn public_key_hex(&self) -> String;
    /// Signs and publishes one event. `kind` is the raw kind number, `tags` are string vectors.
    pub async fn publish(&self, kind: u16, tags: Vec<Vec<String>>, content: &str) -> Result<Published, RelayError>;
    /// POST /query with one or more Nostr filters (serde_json::Value), events newest first.
    pub async fn query(&self, filters: Vec<serde_json::Value>) -> Result<Vec<nostr::Event>, RelayError>;
    // Convenience, all building on publish:
    pub async fn forum_post(&self, channel: &str, content: &str) -> Result<Published, RelayError>;   // kind 45001, ["h", channel]
    pub async fn stream_message(&self, channel: &str, content: &str) -> Result<Published, RelayError>; // kind 9, ["h", channel]
    pub async fn delete(&self, channel: &str, event_id: &str) -> Result<Published, RelayError>;      // kind 9005, ["h", channel], ["e", event_id]
    pub async fn own_posts(&self, channel: &str, kind: u16, limit: u32) -> Result<Vec<nostr::Event>, RelayError>; // {"kinds":[kind],"#h":[channel],"authors":[me],"limit":limit}
}
pub fn load_keys(path: &Path) -> anyhow::Result<nostr::Keys>; // one line, hex or nsec, via nostr::Keys::parse; the file mode is checked (refuse group/other-readable, name the chmod)
```

NIP-98 (from the CLI, `buzz-cli/src/client.rs:84-110`): per request a kind-27235 event with tags `["u", <base>/events]`, `["method","POST"]`, `["nonce", uuid_v4]`, `["payload", sha256hex(body)]`, signed with the clerk's keys, `Authorization: Nostr <base64 standard(event JSON)>`, `Content-Type: application/json`. `POST /events` body = the signed event JSON; response `{"event_id","accepted","message"}`. `POST /query` body = JSON array of filters; response = JSON array of events. A `429` carries `{"error":"rate-limited: … retry in Ns"}` → `is_transient`.

- [ ] **Step 1: unit tests** (no network): `nip98_header_signs_the_url_method_and_payload` — build the header, decode base64, parse as `nostr::Event`, assert `kind == 27235`, tags contain `["u", "http://127.0.0.1:17800/events"]`, `["method","POST"]`, a 64-hex `payload` equal to `sha256(body)`, and `event.verify()` passes; `load_keys_reads_hex_and_nsec_and_refuses_a_readable_file` (tempdir, `std::os::unix::fs::PermissionsExt`); `own_posts_filter_names_the_author_and_channel` (make the filter builder a pure fn `own_posts_filter(pubkey, channel, kind, limit) -> Value` and assert its JSON).
- [ ] **Step 2: run** `cargo test --lib relay` → red.
- [ ] **Step 3: implement**, with `reqwest` timeouts (10 s), no retries inside (callers decide), `RelayError::Refused` for non-2xx with the body text (truncated to 300 chars in logs).
- [ ] **Step 4: run** → green; `cargo clippy` clean on the module.
- [ ] **Step 5: commit** `The clerk speaks to a Buzz relay: NIP-98 over /events and /query, nothing else`.

---

### Task 3: The sentences and the reference line (`text.rs`, `reference.rs`)

**Files:** Create `clerk/src/text.rs`, `clerk/src/reference.rs`; modify `lib.rs`.

**Interfaces (produces):**

```rust
// reference.rs — the line every approbations post ends with, and the sweep's key
pub const PREFIX: &str = "twalk:suggestion:";
pub struct Reference { pub suggestion_id: String /* 64 hex */, pub expires_at: Option<String> /* RFC 3339 as the event carries it */ }
pub fn line(r: &Reference) -> String;                 // "twalk:suggestion:<id>" or "twalk:suggestion:<id> expires <rfc3339>"
pub fn parse(content: &str) -> Option<Reference>;      // last line of the content that starts with PREFIX; None otherwise
pub fn has_expired(expires_at: &str, now_unix: i64) -> bool; // false when unparsable (an unparsable expiry never deletes)

// text.rs — every sentence the clerk writes, by language
#[derive(Clone, Copy)] pub enum Lang { Fr, En }
pub fn lang(user_language: &str) -> (Lang, bool /* fell back to English */);
pub fn network_name(network: &str) -> &'static str; // whatsapp→WhatsApp, signal→Signal, sms→SMS, telegram→Telegram, discord→Discord, matrix→Matrix, else the tag itself
pub fn approval_post(l: Lang, body: &str, network: &str, expires_at: Option<&str>, reference: &str) -> String;
pub fn journal_line(l: Lang, network: &str, reach: &str, posted_as: &str, approval_id: &str, at: &str) -> String;
pub fn activity_bridge(l: Lang, bridge_id: &str, state: &str) -> String;
pub fn activity_consent(l: Lang, subject_type: &str, new_state: &str, networks: &[String]) -> String; // never the subject's id
pub fn activity_suggested(l: Lang, network: &str) -> String;
pub fn activity_expired(l: Lang) -> String;
```

The French `approval_post`:

```
Réponse proposée · WhatsApp · expire à 23:41 (heure locale non connue : 21:41 UTC)
« <body> »
Livraison : pas encore connue — ce sera dit ici quand le greffier saura lire l'état de la conversation (#284).
✅ envoyer tel quel · ❌ refuser · répondre ici pour envoyer un autre texte (à venir : #284)
twalk:suggestion:<id> expires <rfc3339>
```
(Format the expiry as `HH:MM UTC` from the RFC 3339 string; the `time` crate parses it.) English mirrors it. `journal_line` fr: `Partie · WhatsApp · postée en tant que <posted_as> · a atteint le contact · approbation <id[:12]>…` / for `nobody`: `Personne ne l'a reçue · WhatsApp · postée en tant que <posted_as> — un compte que le bridge ignore (#123) · approbation <id[:12]>…`. `activity_consent` names only the *kind* (`un contact`, `un persona`) and the state and networks.

- [ ] **Step 1: unit tests**: `the_reference_line_round_trips` (format → parse, with and without expiry); `parse_takes_the_last_reference_line_and_ignores_prose`; `an_unparsable_expiry_never_expires`; `a_post_contains_the_body_and_the_reference_and_nothing_else_of_the_event` (assert the body appears verbatim, the reference is the last line, and — the negative — none of a set of marker strings passed as "context" appear: the function takes no such arguments, so assert the signature by construction: `approval_post` has no parameter that could carry a contact); `journal_line_for_nobody_says_nobody_and_names_123`; `activity_consent_never_carries_the_subject`; `lang_falls_back_to_english_for_it_es_de`.
- [ ] **Step 2: run** → red. **Step 3: implement.** **Step 4: run** → green.
- [ ] **Step 5: commit** `The clerk's sentences, in French and English, and the reference line it finds itself by`.

---

### Task 4: What the clerk reads off the bus, and its metrics (`events.rs`, `metrics.rs`)

**Files:** Create `clerk/src/events.rs`, `clerk/src/metrics.rs`; modify `lib.rs`.

**Interfaces (produces):**

```rust
// events.rs — typed views with NO member for anything the clerk must not hold
#[derive(Deserialize)] pub struct Suggestion { pub id: String, pub time: String, pub network: String, pub data: SuggestionData }
#[derive(Deserialize)] pub struct SuggestionData { pub suggestion: Content, #[serde(default)] pub expires_at: Option<String> }
#[derive(Deserialize)] pub struct Content { pub body: String }
// note: no `trigger`, no `rationale`, no `persona_id` beyond what a sentence needs (persona_id is fine — add it)
pub struct PostedReport { pub approval_id: String, pub network: String, pub reach: String, pub posted_as: String, pub time: String }
pub fn posted_report(payload: &[u8], headers: Option<&async_nats::HeaderMap>) -> Option<PostedReport>; // id+time+network from the JSON, reach/posted-as from headers "reach"/"posted-as"
#[derive(Deserialize)] pub struct BridgeStatus { pub data: BridgeStatusData } pub struct BridgeStatusData { pub bridge_id: String, pub state: String }
#[derive(Deserialize)] pub struct ConsentChange { pub data: ConsentData } pub struct ConsentData { pub subject: Subject, pub new_state: String, #[serde(default)] pub scope: Option<Scope> } pub struct Subject { #[serde(rename = "type")] pub kind: String } pub struct Scope { #[serde(default)] pub networks: Vec<String> }
pub const SUGGEST_PRODUCED: &str = "fr.linagora.twalk.persona.suggest.produced.v1";
pub const REPLY_APPROVED: &str = "fr.linagora.twalk.persona.reply.approved.v1";   // its `.posted` sibling is REPLY_APPROVED subject + ".posted"
pub const BRIDGE_STATUS: &str = "fr.linagora.twalk.bridge.status.changed.v1";
pub const CONSENT_CHANGED: &str = "fr.linagora.twalk.consent.state.changed.v1";

// metrics.rs
pub struct Metrics { … }
pub enum Channel { Approvals, Activity, Journal } impl Channel { pub fn as_str(&self) -> &'static str }
pub enum Deleted { Expired } pub enum Skipped { Expired, Unreadable, Duplicate }
impl Metrics { pub fn new() -> Self; pub fn record_post(&self, c: Channel) -> u64; pub fn record_deleted(&self, d: Deleted) -> u64; pub fn record_skipped(&self, s: Skipped) -> u64; pub fn record_relay_failure(&self) -> u64; pub fn record_sweep(&self); pub fn render(&self, now_unix_seconds: u64) -> String; }
```
Render (all at zero when nothing happened): `twalk_clerk_posts_total{channel="approbations"|"activite"|"journal"}`, `twalk_clerk_deleted_total{why="expired"}`, `twalk_clerk_skipped_total{why="expired"|"unreadable"|"duplicate"}`, `twalk_clerk_relay_failures_total`, `twalk_clerk_sweeps_total`, `twalk_clerk_up 1`, `twalk_clerk_started_at_seconds`.

- [ ] **Step 1: unit tests**: `a_suggestion_is_read_from_the_contract_fixture` — load `contracts/cloudevents/v1/fixtures/persona.suggest.produced.json` via `std::fs` relative to `CARGO_MANIFEST_DIR/../contracts/...` and deserialize; `the_view_of_a_suggestion_has_no_member_for_the_contact` — assert via `serde_json::to_value(&Suggestion)` re-serialisation that the keys are exactly `{id,time,network,data:{persona_id,suggestion:{body},expires_at}}`; `a_posted_report_takes_reach_and_posted_as_from_headers`; `every_metric_exists_at_zero` and `the_exposition_is_prometheus_shaped` (copy the Sensor's shape test).
- [ ] **Step 2: run** → red. **Step 3: implement.** **Step 4: run** → green. **Step 5: commit** `What the clerk reads off the bus, typed so it cannot hold a contact, and what it counts`.

---

### Task 5: A real Buzz relay in the test stack (`clerk/tests/compose.relay.yaml`, `tests/harness/relay.rs`)

**Files:** Create `clerk/tests/compose.relay.yaml`, `clerk/tests/harness/mod.rs`, `clerk/tests/harness/relay.rs`, `clerk/tests/harness/clerk.rs` (stub for now: only `ClerkProc::start(env)` / `logs()` / `wait_for_log()` / `stop()` copied from `hermes/tests/harness/runtime.rs:368-393,484-494,593-647` with `CARGO_BIN_EXE_twalk-clerk`), `clerk/tests/smoke.rs`.

**Interfaces (produces):**

```rust
pub const TEST_OWNER_SECRET_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111"; // test-only, on a local relay
pub struct RelayStack { pub url: String /* http://127.0.0.1:<port> */, pub owner: nostr::Keys, project: String }
impl RelayStack {
    pub async fn ensure() -> anyhow::Result<Self>;                       // OnceCell-guarded compose up -d --wait, like ensure_stack
    pub async fn add_member(&self, pubkey_hex: &str) -> anyhow::Result<()>; // kind 9030 ["p", hex] ["role","member"], signed by the owner, POST /events (NIP-98 as the owner)
    pub async fn create_channel(&self, name: &str, kind: &str /* "forum"|"stream" */, member: &str) -> anyhow::Result<String>; // kind 9007 with a fresh uuid v4 as ["h"], visibility private; then add `member` as ["p",hex] via kind 9000? — NO: channel membership is kind 9000 (NIP-29 put-user); verify in buzz-sdk builders (`add_member` builder) and use it; returns the uuid
    pub async fn events_in(&self, channel: &str, kinds: &[u16]) -> anyhow::Result<Vec<nostr::Event>>; // as the owner: {"kinds":kinds,"#h":[channel],"limit":1000}
    pub async fn all_events_in(&self, channel: &str) -> anyhow::Result<Vec<nostr::Event>>; // kinds 9,45001,45003,7,40003 — for absence searches
}
pub fn relay_env(stack: &RelayStack, key_file: &Path, channels: &Channels, run: &str) -> Vec<(String,String)>; // CLERK_* for ClerkProc
pub struct Channels { pub approvals: String, pub activity: String, pub journal: String }
pub async fn fresh_clerk_key(dir: &Path) -> anyhow::Result<(PathBuf, String /* pubkey hex */)>; // generates keys, writes hex to <dir>/clerk.key mode 0600
```

`compose.relay.yaml` (project `${TWALK_CLERK_TEST_STACK}`, port `${TWALK_CLERK_TEST_RELAY_PORT:-17800}`), from the research report:

```yaml
name: ${TWALK_CLERK_TEST_STACK:-twalk-clerk-test}
services:
  relay:
    image: ghcr.io/block/buzz@sha256:dffaf6695e5a243a0826ff1ca4b682536a1f92b285b55bfd912a5b8570bef560
    depends_on: { postgres: { condition: service_healthy }, redis: { condition: service_healthy } }
    ports: ["127.0.0.1:${TWALK_CLERK_TEST_RELAY_PORT:-17800}:3000"]
    environment:
      DATABASE_URL: postgres://buzz:test-only@postgres:5432/buzz
      REDIS_URL: redis://redis:6379
      RELAY_URL: http://127.0.0.1:${TWALK_CLERK_TEST_RELAY_PORT:-17800}     # the community's host is 127.0.0.1:<port>
      BUZZ_BIND_ADDR: 0.0.0.0:3000
      RELAY_OWNER_PUBKEY: <pubkey of TEST_OWNER_SECRET_HEX — compute once with provision-nostr-key.sh --self-test style and paste; key 1 → 79be667e…>
      BUZZ_RELAY_PRIVATE_KEY: 2222222222222222222222222222222222222222222222222222222222222222
      BUZZ_REQUIRE_RELAY_MEMBERSHIP: "true"
      BUZZ_REQUIRE_AUTH_TOKEN: "true"          # the clerk must sign; no X-Pubkey shortcut in tests
      BUZZ_ALLOW_NIP_OA_AUTH: "false"
      BUZZ_AUTO_MIGRATE: "true"
      BUZZ_GIT_CONFORMANCE_PROBE: "false"      # no MinIO: the only boot-time S3 call
      BUZZ_STORAGE_METRICS: "off"
      BUZZ_AUDIT_ENABLED: "false"
      RUST_LOG: buzz_relay=info
    healthcheck:
      test: ["CMD", "bash", "-ec", "exec 3<>/dev/tcp/127.0.0.1/8080; printf 'GET /_readiness HTTP/1.1\\r\\nHost: 127.0.0.1\\r\\nConnection: close\\r\\n\\r\\n' >&3; grep -q '200 OK' <&3"]
      interval: 2s
      timeout: 3s
      retries: 30
      start_period: 5s
    mem_limit: 256m
  postgres: { image: postgres:17-alpine, environment: { POSTGRES_USER: buzz, POSTGRES_PASSWORD: test-only, POSTGRES_DB: buzz }, healthcheck: { test: ["CMD-SHELL", "pg_isready -U buzz -d buzz"], interval: 2s, timeout: 3s, retries: 30 }, mem_limit: 256m, tmpfs: [/var/lib/postgresql/data] }
  redis: { image: redis:7-alpine, healthcheck: { test: ["CMD", "redis-cli", "ping"], interval: 2s, timeout: 3s, retries: 30 }, mem_limit: 64m }
```
Use the owner key **1** (`0000…0001`) so `RELAY_OWNER_PUBKEY=79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798` (the self-test vector) — set `TEST_OWNER_SECRET_HEX` to that. **Check with `buzz-sdk/src/builders.rs` which kind adds a member to a channel** (search `fn add_member` / kind 9000 `["p", hex, role]`) and use exactly that; the research report covered relay membership (9030) and channel creation (9007) only.

`ensure()`: `docker compose -p <project> -f clerk/tests/compose.relay.yaml up -d --wait` under a `tokio::sync::OnceCell`; then `poll_until` `GET {url}/` with `Accept: application/nostr+json` answers JSON. Teardown: none by default (the shared-stack convention: it persists across runs; `TWALK_CLERK_TEST_KEEP` is not needed; document `docker compose -p twalk-clerk-test down -v`).

- [ ] **Step 1: `smoke.rs`**: `RelayStack::ensure()`; NIP-11 shows `restricted_writes: true`; `fresh_clerk_key`; `add_member(clerk)`; three channels created with the clerk as member; `ClerkProc::start(relay_env(..))` with `CLERK_LISTEN=127.0.0.1:0`?? — no: axum needs a known port; use `CLERK_LISTEN=127.0.0.1:<free port from the kernel>` (bind a `std::net::TcpListener` to port 0, read the port, drop it); wait for log `clerk running`; `GET /health` 200; `GET /metrics` contains `twalk_clerk_posts_total{channel="approbations"} 0`. (This test goes red until Task 6 wires main; write it now, run it at Task 7.)
- [ ] **Step 2: run** `TWALK_CLERK_TEST_STACK=twalk-clerk-dev cargo test --test smoke` → the stack comes up (verify with `docker compose -p twalk-clerk-dev ps`) and the clerk step fails on the missing "clerk running" line. Fix anything in the stack until the relay is healthy and `add_member` + `create_channel` are accepted (`accepted: true`).
- [ ] **Step 3: commit** `A real Buzz relay in the clerk's test stack: three containers, seeded by signed events`.

---

### Task 6: The wiring — consumers, the sweep, health and metrics (`main.rs`)

**Files:** Modify `clerk/src/main.rs`; add `clerk/src/consumers.rs` if `main.rs` passes 400 lines.

**Interfaces (consumes):** everything from T1–T4.

Behaviour, in order of `main`:
1. Config, tracing, `Metrics`, `Relay::new(&config.relay_url, load_keys(&config.nostr_key_file)?)`; `info!(relay, pubkey = %relay.public_key_hex(), language, fallback_to_english, "clerk starting")`; if the language fell back, `warn!`.
2. axum on `config.listen`: `/health` → `{"status":"ok","version":env!("CARGO_PKG_VERSION")}`; `/metrics` → `metrics.render(now)` with `text/plain; version=0.0.4; charset=utf-8`. `info!(address, "clerk listening")`.
3. Bus: connect, `get_or_create_stream` (subjects `["<prefix>.>"]`), then three tasks:
   - **suggestions**: durable `clerk-suggestions`, filter `bus_subject(SUGGEST_PRODUCED)`, `DeliverPolicy::All`, `AckPolicy::Explicit`, `ack_wait 60s`, `max_deliver 5`. Per message: parse `Suggestion` (unreadable → `skipped{unreadable}`, ack); if `expires_at` has passed → `skipped{expired}`, ack; else `relay.own_posts(approvals, 45001, 1000)` and if any post's `reference::parse(content).suggestion_id == id` → `skipped{duplicate}`, ack; else `relay.forum_post(approvals, text::approval_post(...))` then `relay.stream_message(activity, text::activity_suggested(...))`, `posts{approbations}`, ack. Relay error transient → `nak` with delay `min(2^delivered, 60)` s and `relay_failures`; non-transient → warn, ack (a refused post is not retried for ever: it is counted and logged once with the relay's body).
   - **posted reports**: durable `clerk-journal`, filter `bus_subject(REPLY_APPROVED) + ".posted"`, `DeliverPolicy::New` (a fresh clerk journals from now). `events::posted_report` → `relay.stream_message(journal, text::journal_line(...))`; same error handling.
   - **activity**: durable `clerk-activity`, `filter_subjects: [bridge status, consent changed]` (async-nats `pull::Config.filter_subjects`), `DeliverPolicy::New`. Bridge → `activity_bridge`; consent → `activity_consent` (never the subject id). Same error handling.
   - **sweep**: every `config.sweep`: `relay.own_posts(approvals, 45001, 1000)`; for each with a parsed reference whose `has_expired(expires_at, now)` → `relay.delete(approvals, id)`, `deleted{expired}`, then `stream_message(activity, activity_expired)`; `record_sweep()`; a relay error is a warning + `relay_failures`, never a stop. `info!("clerk running")` once the consumers are open.
4. `shutdown_signal().await` → `info!("clerk stopped")`.

Every consumer loop is wrapped like the Sensor's `consume_approved_replies` (`sensor/src/main.rs:2261-2290`): on stream end or error, log `"the <name> consumer failed; rebuilding it"`, sleep 1 s, rebuild.

- [ ] **Step 1: implement** (there is no unit test for wiring; the process-boundary suites of Task 7 are its tests). Keep pure decisions out of `main.rs`: `fn already_posted(posts: &[nostr::Event], id: &str) -> bool` and `fn nak_delay(delivered: i64) -> Duration` go in `lib.rs`-side modules with unit tests (`already_posted_matches_on_the_reference_line_only`, `nak_delay_doubles_and_caps_at_a_minute`).
- [ ] **Step 2:** `cargo build`, `cargo clippy --all-targets` clean, `cargo fmt`.
- [ ] **Step 3: commit** `The clerk runs: three durable consumers, a sweep, and an origin for /health and /metrics`.

---

### Task 7: The process-boundary suites

**Files:** Create `clerk/tests/approbations.rs`, `clerk/tests/journal.rs`, `clerk/tests/activite.rs`, `clerk/tests/sweep.rs`; finish `clerk/tests/harness/clerk.rs` (`ClerkProc::wait_for_log`, `logs`, `stop`, `metrics(port) -> String`).

Shared setup per test (write it once as `harness::Run::start(test_name)`): `ensure_stack()` (Synapse/NATS — for the bus), `RelayStack::ensure()`, `Bus::connect()`, a run id `c265-<test>-<pid>-<nanos>` used as **both** stream name and subject prefix (`bus.ensure_stream(&id, &[&format!("{id}.>")])`, like Hermes), a fresh clerk key added to the relay, three fresh channels with the clerk as member, `ClerkProc` with `CLERK_STREAM=<id>`, `CLERK_SUBJECT_PREFIX=<id>`, `CLERK_SWEEP_SECONDS=2`; `Drop`/`shutdown` deletes the stream and stops the clerk. Events are built from `contract_fixture("persona.suggest.produced")` etc. with a fresh `id` (`sha256_hex(run + n)`), `expires_at` set explicitly, and `validate_against_contract` before publishing on `<id>.persona.suggest.produced.v1` with `publish_event`.

- [ ] **`approbations.rs`**
  1. `a_suggestion_becomes_one_post_and_stays_one`: publish a suggestion (expires in 1 h) → `poll_until` `events_in(approvals, [45001])` has exactly one post whose content contains `suggestion.body` and ends with the reference line; `metrics` shows `posts_total{approbations} 1`. Then `publish_event` **the same event again** (JetStream dedups by `Nats-Msg-Id` within 2 min — so instead republish with a *new* `Nats-Msg-Id` but the same CloudEvent id: use `publish_event_with_headers(subject, "again", HeaderMap::new(), &event)`) → after the `activite` line for a second suggestion (publish a *different* suggestion and wait for its post, proving the consumer advanced) assert still exactly one post for the first id and `skipped_total{duplicate} 1`. Then `clerk.stop()`, start a new `ClerkProc` on the same channels/stream → wait `clerk running`, publish a third suggestion, wait for its post → the first id still has one post.
  2. `nothing_of_the_contact_reaches_the_relay`: the fixture's trigger is not on this stream; build the suggestion's `data.suggestion.body` = `"D'accord pour 20h ! MARKER-BODY-<run>"`, `data.rationale` = `"MARKER-RATIONALE-<run>"` (if the schema allows rationale), and publish an `inbound.message.received` on the run's inbound subject with `data.body = "MARKER-INBOUND"`, `contact.display_name = "MARKER-NAME"`, `network_identifier = "MARKER-NUMBER"`, `reply_to.excerpt = "MARKER-EXCERPT"` (validated). Wait for the post. Then `all_events_in` for each of the three channels, join every event's `content` and every tag value, assert `MARKER-BODY` is present exactly once (the post) and `MARKER-RATIONALE`, `MARKER-INBOUND`, `MARKER-NAME`, `MARKER-NUMBER`, `MARKER-EXCERPT`, the trigger's Matrix user ID and its room id are absent.
  3. `an_expired_suggestion_is_not_posted_and_is_counted`: `expires_at` = now − 1 s → no post after a second suggestion (unexpired) has been posted; `skipped_total{expired} 1`.
- [ ] **`journal.rs`**: publish a `persona.reply.approved` event on `<id>.persona.reply.approved.v1.posted` with headers `reach: contact`, `posted-as: @owner:test.twalk` (`publish_event_with_headers(.., "posted", headers, &event)`) → one kind-9 message in `journal` containing `WhatsApp`, `@owner:test.twalk`, the approval id's first 12 chars, and **not** `data.final.body`; then one with `reach: nobody` → its line contains `Personne` (fr) / `Nobody` (en, run the suite with `CLERK_USER_LANGUAGE=en` in one of the two cases) and `#123`.
- [ ] **`activite.rs`**: publish `bridge.status.changed` (fixture) → a line naming the bridge id and state; `consent.state.changed` (fixture, subject a contact) → a line with the new state and **no** Matrix user ID (assert against `MATRIX_USER_ID_SUBJECT_PATTERN` or a regex `@[^:]+:[^ ]+`); `persona.thinking.emitted` (fixture) → no line (prove by a later bridge line arriving with the count unchanged in between).
- [ ] **`sweep.rs`**: a suggestion expiring in 3 s → post exists; within 8 s (`CLERK_SWEEP_SECONDS=2`) the post is gone from `events_in(approvals, [45001])`, `deleted_total{expired} 1`, and `activite` has the expiry line.
- [ ] **Run** all: `TWALK_CLERK_TEST_STACK=twalk-clerk-dev cargo test` from `clerk/` (needs the shared Synapse/NATS stack too: `TWALK_TEST_STACK=…` defaults apply). Fix until green; watch the relay's per-pubkey rate limit (300/min) — the sweep queries once per 2 s in tests, fine.
- [ ] **Commit** `The clerk at its process boundary: one post per suggestion, nothing of a contact on the relay, the journal, the activity feed and the sweep`.

---

### Task 8: Scripts, CI routing, glossary, ADR, docs (independent of code; can run in parallel with T2–T5)

**Files:**
- Rename `deploy/docker-compose/provision-hermes-nostr-key.sh` → `provision-nostr-key.sh` (`git mv`), add symlink `provision-hermes-nostr-key.sh -> provision-nostr-key.sh`; update the header's usage line (`./provision-nostr-key.sh [env-file]   default: ~/.hermes-twalk/.env`) and the comment "Written by provision-nostr-key.sh"; the docstring gains one sentence: "Not Hermes's: the clerk (#265) gets its key the same way, into its own file — `./provision-nostr-key.sh /etc/twalk/clerk.env`".
- Modify `deploy/docker-compose/provision-buzz-channels.sh`: references to the renamed script; a repeatable `--bot <64-hex>` option (parse before the positional env file; each is added with `--role bot` after Hermes; the self-add refusal logic unchanged); at the end print the three `CLERK_CHANNEL_APPROVALS=… / CLERK_CHANNEL_ACTIVITY=… / CLERK_CHANNEL_JOURNAL=…` lines "for deploy/docker-compose/.env" (it does not write that file — it is the compose stack's, and this script writes Hermes's).
- Modify `deploy/README.md` "What is where": rename row, add `clerk.Dockerfile` mention under `*.Dockerfile` (already generic), a row for `provision-nostr-key.sh` (rewrite the existing one) and update the channels row (`--bot`).
- Modify `CONTEXT.md`: under `### Ecosystem`, after **Buzz**:
  > **Clerk**:
  > The Twalk component that writes onto Buzz what the bus says and carries to the Companion Gateway what the owner decides there. A surface and never an authority (ADR 0032): it holds no consent snapshot, no store, no key of Hermes's, and no credential the Gateway would take for anyone but the owner's own device. Called *le greffier* in French.
  > _Avoid_: "Buzz bot", "the Twalk agent", "herald"
- Create `docs/architecture/adr/0035-the-clerk-is-a-projection-of-the-relay-and-holds-no-state.md` in the three-paragraph shape of ADR 0029 (decision with bold assertions and issue links; `Rationale:`; `The costs are accepted rather than argued away.`): the clerk finds what it already wrote by reading its own posts and the reference line each carries; a redelivery and a restart are the same read; the alternative — a SQLite of suggestion id → post id — was weighed and rejected because a store makes the relay a *copy* of something Twalk holds and puts a second truth beside the bus and the relay; costs: one query per suggestion and per sweep (bounded by the relay's 300/min per key), a reference line the owner sees, and no memory of a post the relay lost.
- Modify `.github/ci/suites.json`: a `clerk` suite (`what`, `component: "clerk"`, `workdir: "clerk"`, `run: "cargo test --lib --test activite --test approbations --test journal --test smoke --test sweep"`, `targets`, `needs: ["rust","docker"]`, `tier: "required"`, `triggers: ["clerk/", "contracts/", "tests/harness/"]`), `"clerk/README.md"` in `ignored`; `.github/ci/test_selection.py`: add `"clerk"` to the component tuples at `:226-232`, `:288`, `:329`. Run `cd .github/ci && python3 -m unittest discover -s . -p 'test_*.py' -v` → green (it will fail on missing test files until Task 7 lands: so this task lands **after** T7 in the merge order but can be prepared in parallel; the executor commits it once `clerk/tests/*.rs` exist — or the subagent for this task creates empty placeholder test files? **No placeholders**: this task's commit waits for T7.)
- Modify `AGENTS.md`: a `clerk/` bullet in the repository list (one paragraph in the house voice: what it is, the three channels, the decisions to argue with — own key, no state, the sweep — and the test stack variables `TWALK_CLERK_TEST_STACK`, `TWALK_CLERK_TEST_RELAY_PORT`), and the "Build and test commands" block gains `cd clerk && cargo test`.
- Modify `docs/agents/continuous-integration.md` if it lists suites by name.

- [ ] Steps: do each edit; `bash -n` both scripts; run the channels script's `--help`-less argument parsing by hand on a scratch env (`TWALK_BUZZ_OWNER_KEY_FILE` pointing at a 0600 file holding the **test** key `000…001` against the **test relay** from Task 5 at `BUZZ_RELAY_URL=http://127.0.0.1:17800` — the test relay's owner is that key, so the full happy path runs: four channels, Hermes-less (`BUZZ_PUBLIC_KEY` absent → the script must handle "no Hermes key" gracefully: make the Hermes add optional when `provision-nostr-key.sh` finds no key, printing a note) — simplest: `--bot` entries are added, Hermes is added only when its env file has a key.
- [ ] **Commit** (after T7): `The clerk in the glossary, its ADR, its CI route, and the operator scripts it needs`.

---

### Task 9: The compose service

**Files:** Create `deploy/docker-compose/clerk.Dockerfile`; modify `deploy/docker-compose/compose.yaml` (service `clerk`), `deploy/docker-compose/.env.example` (a `# --- Clerk` section documenting `CLERK_RELAY_URL`, `CLERK_NOSTR_KEY_FILE` (host path, mounted read-only at `/run/secrets/clerk.key`), `CLERK_CHANNEL_APPROVALS/ACTIVITY/JOURNAL`, `CLERK_USER_LANGUAGE` (defaults to `HERMES_USER_LANGUAGE`'s value in compose), `CLERK_LISTEN_PORT` (default 8084, published on 127.0.0.1), `CLERK_SWEEP_SECONDS`), `.dockerignore` unchanged.

Service shape (from `hermes` + the Gateway's healthcheck):
```yaml
  clerk:
    build: { context: ../.., dockerfile: deploy/docker-compose/clerk.Dockerfile }
    image: ${TWALK_CLERK_IMAGE:-twalk/clerk:local}
    depends_on: { nats: { condition: service_started } }
    network_mode: host          # the relay is reached by its announced URL, which on the reference deployment resolves to this host's own Caddy
    init: true
    volumes: ["${CLERK_NOSTR_KEY_FILE:-/dev/null}:/run/secrets/clerk.key:ro"]
    environment:
      CLERK_NATS_URL: nats://127.0.0.1:${NATS_PORT:-4222}
      CLERK_RELAY_URL: ${CLERK_RELAY_URL:-}
      CLERK_NOSTR_KEY_FILE: /run/secrets/clerk.key
      CLERK_CHANNEL_APPROVALS: ${CLERK_CHANNEL_APPROVALS:-}
      CLERK_CHANNEL_ACTIVITY: ${CLERK_CHANNEL_ACTIVITY:-}
      CLERK_CHANNEL_JOURNAL: ${CLERK_CHANNEL_JOURNAL:-}
      CLERK_USER_LANGUAGE: ${CLERK_USER_LANGUAGE:-${HERMES_USER_LANGUAGE:-}}
      CLERK_LISTEN: 127.0.0.1:${CLERK_LISTEN_PORT:-8084}
      CLERK_SWEEP_SECONDS: ${CLERK_SWEEP_SECONDS:-60}
      CLERK_LOG_LEVEL: ${CLERK_LOG_LEVEL:-info}
    healthcheck: { test: ["CMD", "curl", "-fsS", "http://127.0.0.1:${CLERK_LISTEN_PORT:-8084}/health"], interval: 5s, timeout: 5s, retries: 30, start_period: 5s }
    restart: unless-stopped
```
An entrypoint `clerk-entrypoint.sh` in the image, like `hermes-entrypoint.sh`: with `CLERK_RELAY_URL` or a channel unset, print which variables are missing and `sleep infinity` (a stack without Buzz still comes up). The Dockerfile: `rust:1-bookworm` build stage copying `clerk` and `tests/harness`, `cargo build --release --locked`; runtime `debian:bookworm-slim` with `ca-certificates curl`, `USER clerk`.

- [ ] **Steps:** write; `docker compose -f deploy/docker-compose/compose.yaml config` parses; `docker compose build clerk` succeeds; with no `CLERK_*` set, `docker compose up -d clerk` logs the missing variables and stays up. **No deployment suite** in this lot (advisory tier; note it in the PR as the next thing).
- [ ] **Commit** `The clerk as a compose service: built from the repository, hosting nothing until Buzz is configured`.

---

### Task 10: `clerk/README.md`, the ticket mirror, self-review

- [ ] `clerk/README.md`: what the clerk is (glossary sentence), the three channels and what each carries and never carries, every `CLERK_*` variable with its default, the operator steps (key → relay member → channels `--bot` → `.env`), the test stack variables, and the two things deliberately not here (delivery, approval — #284).
- [ ] `.scratch/clerk/ticket-265.md`: `gh issue view 265 --json body --jq .body` saved.
- [ ] Full runs: `cd clerk && cargo test` (private stacks), `cd tests/harness && cargo test`, `cd .github/ci && python3 -m unittest discover -s . -p 'test_*.py'`, `cargo fmt --check` and `cargo clippy --all-targets` in `clerk/`.
- [ ] **Commit** `The clerk's README`; then the PR (`pr` skill), `Closes #265`.

---

## Self-review (done while writing)

- Spec coverage: post per suggestion (T6/T7-1), no contact word (T3/T7-2), journal with reach and no text (T3/T6/T7), activite four kinds and no Matrix ID (T3/T6/T7), deletion on expiry + activite line (T6/T7-sweep), no service token/Hermes key in the container (T9 env holds none; T7 could assert on `ClerkProc`'s env but the stronger check is the compose service — add to the PR evidence a `docker inspect` of the service's env), metrics (T4/T7), CI routing (T8), CONTEXT + ADR (T8), own key + `--bot` + rename (T8), language (T1/T3), announced-URL rule (T1 refusal + README). **Deviation recorded on the tickets:** the `delivery` line is #284's.
- Types: `Relay::forum_post/stream_message/delete/own_posts` (T2) are what T6 calls; `reference::{line,parse,has_expired}` and `text::*` (T3) are what T6 renders; `events::*` (T4) is what T6 parses; `RelayStack`/`ClerkProc`/`Run` (T5/T7) are what the suites use.
- Open fact for T5: the kind that adds a member to a **channel** (NIP-29 `9000` put-user, tags `["p", hex, role]`) — verify in `buzz-sdk/src/builders.rs` before writing `create_channel`.
