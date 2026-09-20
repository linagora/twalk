//! The Companion Gateway's approval refusal vocabulary (#284), answered in
//! the thread with the Companion's own sentence for each code, in the
//! owner's interface language rather than the reply's.
//!
//! # Why the sentences are the Companion's and not the clerk's
//!
//! `POST /api/approvals` refuses with a closed vocabulary of codes, and the
//! approval screen has a sentence for every one of them
//! (`companion/src/lib/approvals/refusal.ts`, ticket #100): the cause, then
//! what the user can do about it. An owner who approves from Buzz meets the
//! same refusal as one who approves from the Companion, and "two doors onto
//! one fact must not teach a client two vocabularies" is the rule the
//! Gateway's own `suggestions_http.rs` was written to. So the clerk does not
//! write a second set: it embeds the Companion's catalogues at compile time
//! ([`FR`], [`EN`]) and reads the sentence out of them. A translator who
//! improves a sentence in the Companion improves it on Buzz at the next
//! build, and the two cannot drift by being edited separately, because
//! there is only one place to edit.
//!
//! The remedy table — code → what the user can do — is the one thing copied
//! rather than embedded, since it is TypeScript and not data. It is copied
//! **exactly**, and a test holds it to the Companion's row by row.
//!
//! # Why a test reads `openapi.yaml`
//!
//! The Companion's table is held to the Gateway's own description by a test
//! that walks `companion-gateway/openapi.yaml` and fails when the approval
//! route grows a code the screen has no words for, or when the table names a
//! code the Gateway no longer answers with (`refusal.test.ts`). The clerk's
//! test is that walk in Rust, over the one operation the clerk calls
//! (`approveSuggestion`), in both directions. Without it the fallback for an
//! unknown code — which must exist, because a code invented after this build
//! still has to be answered with *something* terminal — would make every
//! other test pass while the owner read "a reason this version does not
//! know" for a code the Companion had a sentence for.
//!
//! # What an unknown code is answered with
//!
//! [`sentence`] is total. A code outside the table gets the Companion's own
//! sentence for that case, `approvals.refusal.unknown`, with the code
//! substituted where the catalogue leaves a placeholder, and the
//! `diagnostics` remedy — the Companion's `UNKNOWN` row. That is the one
//! sentence that embeds the code, and a test asserts no other one does: the
//! codes are the Gateway's vocabulary, not the owner's.
//!
//! # The delivery line, from the same catalogues
//!
//! Since the clerk's device also reads (`GET /api/suggestions/{id}`), a
//! post in `approbations` says whether a ✅ on it would reach anybody — the
//! `delivery` the approval screen shows above its own button (#216:
//! `can_reach`, `cannot_reach`, `unknown`, each with a `detail` word behind
//! it). That sentence is the Companion's too ([`delivery_line`]):
//! `approvals.delivery.canReach|cannotReach|unknown`, with the `{detail}`
//! placeholder filled from `approvals.delivery.detail.<detail>`, exactly as
//! `rows.ts`'s `deliveryCopy` and `deliveryDetailKey` compose it. The same
//! test discipline holds it in place: every `detail` the Gateway's
//! description enumerates has an entry in both catalogues and the
//! catalogues name no `detail` the Gateway does not, and a `detail` this
//! build has never met renders the word itself rather than stopping the
//! post. And when the line could **not** be read ([`delivery_unread_line`])
//! the post says so and says why, in one of four ways ([`Unread`]) — no
//! device configured, a Gateway that did not answer, one that refused, a
//! suggestion it could not find — because a post with no delivery line at
//! all would be read as "fine", which is the one thing an unread delivery
//! is not.

use std::sync::OnceLock;

use serde_json::Value;

use crate::gateway::Delivery;
use crate::text::Lang;

/// The Companion's catalogues, embedded: the clerk says exactly what the
/// approval screen says. The paths are relative to this file, which is why
/// they climb out of the crate. The repository root is the Docker build
/// context of every image here, but `clerk.Dockerfile` copies only what it
/// names into that context, so the image build sees these two files
/// **only because it copies `companion/src/lib/i18n/`** beside `clerk/` and
/// `tests/harness/` — a build that forgets that line fails at this
/// `include_str!`, loudly, which is the right failure. The test below that
/// embeds `companion-gateway/openapi.yaml` is `#[cfg(test)]`, so the image
/// needs nothing of the Gateway.
const FR: &str = include_str!("../../companion/src/lib/i18n/fr.json");
const EN: &str = include_str!("../../companion/src/lib/i18n/en.json");

/// The placeholder `approvals.refusal.unknown` leaves for the raw code. It is
/// the Companion's ICU argument name, not this crate's choice.
const CODE_PLACEHOLDER: &str = "{error}";

/// The placeholder `approvals.delivery.cannotReach` and `.unknown` leave for
/// the `detail` sentence — again the Companion's ICU argument name.
const DETAIL_PLACEHOLDER: &str = "{detail}";

/// The catalogue prefix of the `detail` sentences: one key per value of the
/// Gateway's `Delivery.detail` enum.
const DETAIL_PREFIX: &str = "approvals.delivery.detail.";

/// What the user can do about a refusal — the Companion's five
/// (`refusal.ts`'s `Remedy`), because there are five different actions and
/// not five words for "try again".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remedy {
    /// The same request may work: the bus or the store was momentarily out.
    Retry,
    /// The picture of the world is stale; re-reading fixes it.
    Reload,
    /// The session is gone. Nothing is wrong with the deployment.
    SignIn,
    /// A defect in the Companion (or here) or a misconfiguration.
    Diagnostics,
    /// Nothing the user can do from here, and saying so is the honest answer.
    None,
}

impl Remedy {
    /// The catalogue key of this remedy's sentence, as the Companion's
    /// `REMEDY_TEXT` maps it (`sign-in` is spelled `signIn` in the
    /// catalogue).
    fn key(self) -> &'static str {
        match self {
            Remedy::Retry => "approvals.remedy.retry",
            Remedy::Reload => "approvals.remedy.reload",
            Remedy::SignIn => "approvals.remedy.signIn",
            Remedy::Diagnostics => "approvals.remedy.diagnostics",
            Remedy::None => "approvals.remedy.none",
        }
    }
}

/// The Companion's table (`companion/src/lib/approvals/refusal.ts`, `TABLE`),
/// code → remedy, copied exactly and in its order. It holds the codes
/// `POST /api/approvals` answers with and no other: the Companion's row for
/// `suggestions_not_configured` is `GET /api/suggestions`'s, a route the
/// clerk never calls, and the openapi test below would rightly refuse a row
/// for a code the operation the clerk calls does not document.
const TABLE: &[(&str, Remedy)] = &[
    // The request was wrong. Nothing the owner did: the clerk builds the
    // request, so these are defects here.
    ("malformed_request", Remedy::Diagnostics),
    ("approval_is_not_a_batch", Remedy::Diagnostics),
    ("unknown_value", Remedy::Diagnostics),
    ("approved_by_is_not_the_owner", Remedy::Diagnostics),
    // The session. Never described as a deployment problem.
    ("unauthenticated", Remedy::SignIn),
    // It is not there, and the bus was read to its first retained message.
    ("suggestion_not_found", Remedy::Reload),
    ("trigger_not_found", Remedy::None),
    // It may be there, further back than the Gateway reads.
    ("suggestion_out_of_reach", Remedy::None),
    ("trigger_out_of_reach", Remedy::None),
    // It exists and cannot be approved. Seven facts, seven sentences.
    ("suggestion_expired", Remedy::None),
    ("consent_revoked", Remedy::None),
    ("consent_pending", Remedy::None),
    ("suggestion_was_never_consented", Remedy::None),
    ("already_approved", Remedy::Reload),
    ("trigger_has_no_room", Remedy::None),
    ("suggestion_unreadable", Remedy::None),
    // The things behind the Gateway.
    ("bus_unreachable", Remedy::Retry),
    ("store_unavailable", Remedy::Retry),
    // The one that is not a failure: the reply went out.
    ("approval_published_but_not_recorded", Remedy::Reload),
    // This deployment does not do this at all.
    ("approvals_not_configured", Remedy::None),
];

/// The Companion's table, code → remedy; a code it does not know is
/// `Diagnostics`, as the Companion's `UNKNOWN` row has it.
pub fn remedy(code: &str) -> Remedy {
    TABLE
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, remedy)| *remedy)
        .unwrap_or(Remedy::Diagnostics)
}

/// Every code the table knows, for the test that reads the Gateway's
/// description — and for the suite that stubs each one.
pub fn known_codes() -> Vec<&'static str> {
    TABLE.iter().map(|(code, _)| *code).collect()
}

/// The one refusal that is not a failure: the Companion's `sent: true`
/// (`refusal.ts`'s `approval_published_but_not_recorded` row, the only one
/// with that member). The reply *went out* and the Companion Gateway could
/// not write that down, so a thread answer that began "not sent" would be a
/// lie that makes the owner send it twice — the very defect the Companion
/// grew that member to prevent. Whether the code is one the table knows is
/// a separate question ([`remedy`], [`sentence`]); this one is only "did
/// the reply reach the bus?", and for every other code the answer is no.
pub fn sent(code: &str) -> bool {
    code == "approval_published_but_not_recorded"
}

/// The sentence for one Gateway code: the Companion's cause
/// (`approvals.refusal.<code>`), a space, and the Companion's sentence for
/// the remedy (`approvals.remedy.<remedy>`). An unknown code — or a known one
/// whose catalogue entry is missing, which a test rules out at build time —
/// is answered with `approvals.refusal.unknown`, the code substituted, and
/// the `diagnostics` remedy. Total: there is no code this returns nothing
/// for, so nothing the Gateway says can leave the owner's gesture
/// unanswered.
pub fn sentence(l: Lang, code: &str) -> String {
    let cause_key = format!("approvals.refusal.{code}");
    let (cause, remedy) = match (
        TABLE.iter().any(|(known, _)| *known == code),
        entry(l, &cause_key),
    ) {
        (true, Some(cause)) => (cause.to_owned(), remedy(code)),
        _ => (unknown(l, code), Remedy::Diagnostics),
    };
    match entry(l, remedy.key()) {
        Some(next) => format!("{cause} {next}"),
        None => cause,
    }
}

/// `approvals.refusal.unknown` with the code in the placeholder's place. If
/// even that entry were missing, the code alone: a sentence that names the
/// word the Gateway used is the floor.
fn unknown(l: Lang, code: &str) -> String {
    match entry(l, "approvals.refusal.unknown") {
        Some(template) => template.replace(CODE_PLACEHOLDER, code),
        None => code.to_owned(),
    }
}

/// Why a post carries no delivery reading: the four ways the one read at
/// posting time can come to nothing. Each is a different sentence, because
/// "no device configured" is a deployment that chose not to, and "the
/// Companion Gateway did not answer" is one that is in trouble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// The write half is not configured (`CLERK_GATEWAY_SESSION_FILE`), so
    /// the clerk has no device to read as. The post is #265's.
    NoDevice,
    /// Nothing answered within the request timeout, or the answer was not
    /// the route's shape: the Gateway is down, starting, or behind a proxy
    /// page. The post goes up in the same tick regardless.
    GatewayUnreachable,
    /// The Gateway answered and refused: the clerk's session is gone, the
    /// store or the bus behind it is unavailable, or the suggestion is on
    /// the bus and this Gateway could not read it.
    GatewayRefused,
    /// `404 suggestion_not_found` or `410 suggestion_out_of_reach`: the bus
    /// has let the suggestion go, or it lies further back than the Gateway
    /// reads. The clerk holds the event that proposed it and posts it.
    NotFound,
}

/// The delivery line of a post, in the Companion's own words:
/// `Livraison : ` / `Delivery: ` and then the sentence the approval screen
/// shows for this `reach` — `approvals.delivery.canReach`, `cannotReach` or
/// `unknown`, the last for any `reach` this build has never met, as
/// `rows.ts`'s `deliveryCopy` falls back — with `{detail}` replaced by the
/// sentence for the `detail` word (`approvals.delivery.detail.<detail>`),
/// or by the word itself when the catalogue has none for it. Total: no
/// answer the Gateway gives leaves the post without a line.
///
/// The sentence is embedded **verbatim**, including what the Companion
/// chose to put in it: a translator improves it in one place, and an owner
/// reading it on Buzz and on the approval screen reads one sentence.
pub fn delivery_line(l: Lang, delivery: &Delivery) -> String {
    let key = match delivery.reach.as_str() {
        "can_reach" => "approvals.delivery.canReach",
        "cannot_reach" => "approvals.delivery.cannotReach",
        _ => "approvals.delivery.unknown",
    };
    let detail = entry(l, &format!("{DETAIL_PREFIX}{}", delivery.detail))
        .unwrap_or(delivery.detail.as_str());
    let sentence = match entry(l, key) {
        Some(template) => template.replace(DETAIL_PLACEHOLDER, detail),
        // The catalogue could not be read at all: the two words the
        // Gateway gave, which is still more than a blank.
        None => format!("{} ({detail})", delivery.reach),
    };
    format!("{}{sentence}", delivery_prefix(l))
}

/// The delivery line of a post whose delivery the clerk could not read:
/// says that it was not read, why, and where it *is* read — the Approvals
/// screen — so an owner who wants the answer knows where to look, and
/// never mistakes the absence for "can reach".
pub fn delivery_unread_line(l: Lang, why: Unread) -> String {
    let why = match (l, why) {
        (Lang::Fr, Unread::NoDevice) => "aucun device configuré",
        (Lang::Fr, Unread::GatewayUnreachable) => "le Companion Gateway n’a pas répondu",
        (Lang::Fr, Unread::GatewayRefused) => "le Companion Gateway a refusé la lecture",
        (Lang::Fr, Unread::NotFound) => "le Companion Gateway ne trouve pas cette suggestion",
        (Lang::En, Unread::NoDevice) => "no device configured",
        (Lang::En, Unread::GatewayUnreachable) => "the Companion Gateway did not answer",
        (Lang::En, Unread::GatewayRefused) => "the Companion Gateway refused the read",
        (Lang::En, Unread::NotFound) => "the Companion Gateway cannot find this suggestion",
    };
    match l {
        Lang::Fr => format!(
            "{}non lue par le greffier ({why}) — l’écran Approbations la connaît.",
            delivery_prefix(l)
        ),
        Lang::En => format!(
            "{}not read by the clerk ({why}) — the Approvals screen knows it.",
            delivery_prefix(l)
        ),
    }
}

/// The label both delivery lines begin with, so that a reader of the post
/// finds the line by one word whichever of the two it is.
fn delivery_prefix(l: Lang) -> &'static str {
    match l {
        Lang::Fr => "Livraison : ",
        Lang::En => "Delivery: ",
    }
}

/// Every `detail` word the Companion's French catalogue has a sentence for
/// (`approvals.delivery.detail.*`), for the test that holds it to the
/// Gateway's description and to the English one — and for a suite that
/// scripts each. The French catalogue is the reference the Companion's
/// own tooling treats as complete.
pub fn known_details() -> Vec<&'static str> {
    catalogue(Lang::Fr)
        .as_object()
        .map(|entries| {
            entries
                .keys()
                .filter_map(|key| key.strip_prefix(DETAIL_PREFIX))
                .collect()
        })
        .unwrap_or_default()
}

/// One catalogue entry, or `None` when the key is not there or is not a
/// string.
fn entry(l: Lang, key: &str) -> Option<&'static str> {
    catalogue(l).get(key)?.as_str()
}

/// The parsed catalogue for a language, parsed once. The files are embedded
/// and the Companion's own build refuses a catalogue that is not JSON, so a
/// parse failure here is a build broken in a way its tests would have
/// caught first; it is still not a panic in the running clerk.
fn catalogue(l: Lang) -> &'static Value {
    static FR_PARSED: OnceLock<Value> = OnceLock::new();
    static EN_PARSED: OnceLock<Value> = OnceLock::new();
    let (cell, raw) = match l {
        Lang::Fr => (&FR_PARSED, FR),
        Lang::En => (&EN_PARSED, EN),
    };
    cell.get_or_init(|| serde_json::from_str(raw).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// The Gateway's own description, the same file the Companion's
    /// `refusal.test.ts` reads.
    const OPENAPI: &str = include_str!("../../companion-gateway/openapi.yaml");

    /// The operations the clerk calls, and therefore has to have words for.
    /// One: the clerk approves and reads no suggestion listing.
    const OPERATIONS: [&str; 1] = ["approveSuggestion"];

    /// Every `error` value the description enumerates for those operations
    /// — `refusal.test.ts`'s `documentedCodes`, transcribed. The codes live
    /// in two shapes, inline under a status's schema and behind a `$ref` to
    /// a shared response, so this walks every value under `responses` for a
    /// property named `error` carrying an `enum`, and then names the two
    /// shared responses the walk cannot follow, exactly as the Companion's
    /// test does. If either is renamed, the assertion that every code in the
    /// table is documented catches it.
    fn documented_codes() -> BTreeSet<String> {
        let document: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(OPENAPI).expect("openapi.yaml parses");
        let mut codes = BTreeSet::new();
        let paths = document
            .get("paths")
            .and_then(|paths| paths.as_mapping())
            .expect("openapi.yaml has paths");
        for (_, operations) in paths {
            let Some(operations) = operations.as_mapping() else {
                continue;
            };
            for (_, operation) in operations {
                let named = operation
                    .get("operationId")
                    .and_then(|id| id.as_str())
                    .is_some_and(|id| OPERATIONS.contains(&id));
                if named {
                    if let Some(responses) = operation.get("responses") {
                        collect(responses, &mut codes);
                    }
                }
            }
        }
        for shared in ["approvals_not_configured", "unauthenticated"] {
            codes.insert(shared.to_owned());
        }
        codes
    }

    fn collect(node: &serde_yaml_ng::Value, codes: &mut BTreeSet<String>) {
        match node {
            serde_yaml_ng::Value::Sequence(items) => {
                for item in items {
                    collect(item, codes);
                }
            }
            serde_yaml_ng::Value::Mapping(record) => {
                if let Some(values) = record.get("error").and_then(|error| error.get("enum")) {
                    if let Some(values) = values.as_sequence() {
                        codes.extend(values.iter().filter_map(|v| v.as_str()).map(str::to_owned));
                    }
                }
                for (_, value) in record {
                    collect(value, codes);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn every_code_openapi_documents_has_a_sentence_and_every_sentence_a_code() {
        let documented = documented_codes();
        // A sanity floor: if the walk found nothing, both assertions below
        // would pass vacuously and this test would be worthless.
        assert!(documented.len() > 10, "the walk found {documented:?}");
        let known: BTreeSet<String> = known_codes().into_iter().map(str::to_owned).collect();

        let missing: Vec<_> = documented.difference(&known).collect();
        assert!(
            missing.is_empty(),
            "codes the Gateway documents for POST /api/approvals and the clerk has no words for: {missing:?}"
        );
        let invented: Vec<_> = known.difference(&documented).collect();
        assert!(
            invented.is_empty(),
            "codes the clerk's table names and the Gateway no longer documents: {invented:?}"
        );
    }

    /// `components.schemas.Delivery.properties.<member>.enum` of the
    /// Gateway's description: the closed vocabulary of one `Delivery`
    /// member, as the Companion's `rows.test.ts` reads it.
    fn delivery_enum(member: &str) -> BTreeSet<String> {
        let document: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(OPENAPI).expect("openapi.yaml parses");
        document
            .get("components")
            .and_then(|c| c.get("schemas"))
            .and_then(|s| s.get("Delivery"))
            .and_then(|d| d.get("properties"))
            .and_then(|p| p.get(member))
            .and_then(|m| m.get("enum"))
            .and_then(|e| e.as_sequence())
            .unwrap_or_else(|| panic!("openapi.yaml enumerates Delivery.{member}"))
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn every_delivery_detail_openapi_names_has_a_sentence_in_both_languages_and_no_extra() {
        let documented = delivery_enum("detail");
        // A sanity floor, for the same reason as the codes' walk.
        assert!(documented.len() >= 5, "the walk found {documented:?}");
        let known: BTreeSet<String> = known_details().into_iter().map(str::to_owned).collect();

        let missing: Vec<_> = documented.difference(&known).collect();
        assert!(
            missing.is_empty(),
            "details the Gateway documents for Delivery and the Companion has no words for: {missing:?}"
        );
        let invented: Vec<_> = known.difference(&documented).collect();
        assert!(
            invented.is_empty(),
            "details the Companion's catalogue names and the Gateway no longer documents: {invented:?}"
        );
        // Both languages, and the English catalogue names the same set:
        // `known_details` reads the French one, so an English entry missing
        // or extra would otherwise go unnoticed here.
        for detail in &documented {
            for l in [Lang::Fr, Lang::En] {
                let key = format!("{DETAIL_PREFIX}{detail}");
                assert!(
                    entry(l, &key).is_some_and(|s| !s.is_empty()),
                    "{l:?}: {key}"
                );
            }
        }
        let english: BTreeSet<String> = catalogue(Lang::En)
            .as_object()
            .unwrap()
            .keys()
            .filter_map(|key| key.strip_prefix(DETAIL_PREFIX))
            .map(str::to_owned)
            .collect();
        assert_eq!(english, documented);

        // And every `reach` the Gateway documents is one `delivery_line`
        // has a sentence for — a fourth value would fall into `unknown`'s
        // sentence silently, which this pins.
        let reaches = delivery_enum("reach");
        let expected: BTreeSet<String> = ["can_reach", "cannot_reach", "unknown"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(reaches, expected);
        for l in [Lang::Fr, Lang::En] {
            for key in [
                "approvals.delivery.canReach",
                "approvals.delivery.cannotReach",
                "approvals.delivery.unknown",
            ] {
                assert!(entry(l, key).is_some_and(|s| !s.is_empty()), "{l:?}: {key}");
            }
            // The two that take a detail leave the placeholder for it.
            for key in [
                "approvals.delivery.cannotReach",
                "approvals.delivery.unknown",
            ] {
                assert!(
                    entry(l, key).is_some_and(|s| s.contains(DETAIL_PLACEHOLDER)),
                    "{l:?}: {key}"
                );
            }
        }
    }

    fn delivery(reach: &str, detail: &str) -> Delivery {
        Delivery {
            reach: reach.to_owned(),
            detail: detail.to_owned(),
        }
    }

    #[test]
    fn delivery_line_substitutes_the_detail() {
        // The representative one, spelled out: what the owner reads on Buzz
        // under a reply that would reach nobody is the Companion's warning,
        // whole, with the reason in its parenthesis — including the
        // Companion's own reference to the mechanism that changes it.
        assert_eq!(
            delivery_line(Lang::Fr, &delivery("cannot_reach", "owner_invited")),
            "Livraison : Cette réponse ne peut pas atteindre le contact. Votre compte n’est pas \
             dans cette conversation (votre compte y a été invité et aucun de vos appareils n’a \
             accepté), et un bridge ne relaie que ce que votre propre compte envoie : l’approuver \
             la publierait sur votre flux, et personne ne la recevrait. Ce qui change cela est le \
             device qui agit en votre nom (#123) — il doit avoir rejoint la conversation. Ce \
             n’est pas un défaut de votre installation."
        );
        assert_eq!(
            delivery_line(Lang::En, &delivery("cannot_reach", "owner_invited")),
            "Delivery: This reply cannot reach the contact. Your account is not in this \
             conversation (your account was invited and none of your devices accepted), and a \
             bridge relays only what your own account sends: approving would publish it on your \
             stream, and nobody would receive it. What changes this is the device that acts in \
             your name (#123) — it has to have joined the conversation. This is not a fault in \
             your setup."
        );
        assert_eq!(
            delivery_line(Lang::Fr, &delivery("can_reach", "owner_joined")),
            "Livraison : Votre compte est dans cette conversation : une fois approuvée, la \
             réponse est relayée au contact par le réseau, comme si vous l’aviez écrite."
        );
        assert_eq!(
            delivery_line(Lang::En, &delivery("unknown", "not_a_known_portal")),
            "Delivery: Twalk cannot tell in advance whether it will reach the contact (none of \
             your bridges knows this conversation — a Matrix conversation, most likely). Your \
             Sensor will say once the reply is posted."
        );
        // And, for every reach and every detail, the line is the prefix, the
        // Companion's sentence for the reach, and the Companion's sentence
        // for the detail in the placeholder's place — never the two words.
        for l in [Lang::Fr, Lang::En] {
            for (reach, key) in [
                ("can_reach", "approvals.delivery.canReach"),
                ("cannot_reach", "approvals.delivery.cannotReach"),
                ("unknown", "approvals.delivery.unknown"),
            ] {
                for detail in known_details() {
                    let line = delivery_line(l, &delivery(reach, detail));
                    let sentence = entry(l, key).unwrap().replace(
                        DETAIL_PLACEHOLDER,
                        entry(l, &format!("{DETAIL_PREFIX}{detail}")).unwrap(),
                    );
                    assert_eq!(line, format!("{}{sentence}", delivery_prefix(l)));
                    assert!(!line.contains(detail), "{l:?} {reach} {detail}: {line}");
                    assert!(!line.contains(reach), "{l:?} {reach} {detail}: {line}");
                    assert!(!line.contains(DETAIL_PLACEHOLDER), "{line}");
                }
            }
        }
    }

    #[test]
    fn an_unknown_detail_renders_the_word() {
        for l in [Lang::Fr, Lang::En] {
            let line = delivery_line(l, &delivery("unknown", "a_detail_from_the_future"));
            assert!(line.contains("(a_detail_from_the_future)"), "{l:?}: {line}");
            assert!(!line.contains(DETAIL_PLACEHOLDER), "{l:?}: {line}");
            assert!(line.starts_with(delivery_prefix(l)), "{l:?}: {line}");
            // A `reach` this build has never met is the `unknown` sentence,
            // as the Companion's `deliveryCopy` falls back — not a panic and
            // not a blank.
            let line = delivery_line(l, &delivery("a_reach_from_the_future", "owner_absent"));
            assert_eq!(
                line,
                format!(
                    "{}{}",
                    delivery_prefix(l),
                    entry(l, "approvals.delivery.unknown").unwrap().replace(
                        DETAIL_PLACEHOLDER,
                        entry(l, &format!("{DETAIL_PREFIX}owner_absent")).unwrap()
                    )
                )
            );
        }
    }

    #[test]
    fn unread_lines_in_both_languages() {
        assert_eq!(
            delivery_unread_line(Lang::Fr, Unread::NoDevice),
            "Livraison : non lue par le greffier (aucun device configuré) — l’écran Approbations \
             la connaît."
        );
        assert_eq!(
            delivery_unread_line(Lang::Fr, Unread::GatewayUnreachable),
            "Livraison : non lue par le greffier (le Companion Gateway n’a pas répondu) — l’écran \
             Approbations la connaît."
        );
        assert_eq!(
            delivery_unread_line(Lang::Fr, Unread::GatewayRefused),
            "Livraison : non lue par le greffier (le Companion Gateway a refusé la lecture) — \
             l’écran Approbations la connaît."
        );
        assert_eq!(
            delivery_unread_line(Lang::Fr, Unread::NotFound),
            "Livraison : non lue par le greffier (le Companion Gateway ne trouve pas cette \
             suggestion) — l’écran Approbations la connaît."
        );
        assert_eq!(
            delivery_unread_line(Lang::En, Unread::NoDevice),
            "Delivery: not read by the clerk (no device configured) — the Approvals screen knows \
             it."
        );
        assert_eq!(
            delivery_unread_line(Lang::En, Unread::GatewayUnreachable),
            "Delivery: not read by the clerk (the Companion Gateway did not answer) — the \
             Approvals screen knows it."
        );
        assert_eq!(
            delivery_unread_line(Lang::En, Unread::GatewayRefused),
            "Delivery: not read by the clerk (the Companion Gateway refused the read) — the \
             Approvals screen knows it."
        );
        assert_eq!(
            delivery_unread_line(Lang::En, Unread::NotFound),
            "Delivery: not read by the clerk (the Companion Gateway cannot find this suggestion) \
             — the Approvals screen knows it."
        );
        // Four sentences, all different, none naming a ticket: an unread
        // delivery is a fact about this deployment now, not a promise.
        for l in [Lang::Fr, Lang::En] {
            let lines: BTreeSet<String> = [
                Unread::NoDevice,
                Unread::GatewayUnreachable,
                Unread::GatewayRefused,
                Unread::NotFound,
            ]
            .into_iter()
            .map(|why| delivery_unread_line(l, why))
            .collect();
            assert_eq!(lines.len(), 4);
            for line in &lines {
                assert!(line.starts_with(delivery_prefix(l)), "{line}");
                assert!(!line.contains('#'), "{line}");
            }
        }
    }

    #[test]
    fn every_known_code_has_a_catalogue_entry_in_both_languages() {
        for l in [Lang::Fr, Lang::En] {
            for code in known_codes() {
                let key = format!("approvals.refusal.{code}");
                let cause = entry(l, &key);
                assert!(cause.is_some_and(|s| !s.is_empty()), "{l:?}: {key}");
                let remedy_key = remedy(code).key();
                let next = entry(l, remedy_key);
                assert!(next.is_some_and(|s| !s.is_empty()), "{l:?}: {remedy_key}");
            }
            assert!(
                entry(l, "approvals.refusal.unknown").is_some_and(|s| s.contains(CODE_PLACEHOLDER))
            );
        }
    }

    #[test]
    fn a_sentence_is_the_cause_then_the_remedy_exactly_as_the_companion_has_them() {
        // The representative pair, spelled out: what the owner reads on
        // Buzz is the Companion's sentence, whole, followed by the
        // Companion's next step.
        assert_eq!(
            sentence(Lang::Fr, "already_approved"),
            "Vous avez déjà approuvé cette réponse. Elle n’a pas été envoyée une seconde fois — \
             actualisez pour voir où la première a atterri. Actualisez cet écran pour voir où en \
             sont les choses."
        );
        assert_eq!(
            sentence(Lang::En, "already_approved"),
            "You have already approved this reply. It was not sent a second time — refresh to see \
             where the first one landed. Refresh this screen to see where things stand."
        );
        // And, for every code, the two halves are the catalogue's own.
        for l in [Lang::Fr, Lang::En] {
            for code in known_codes() {
                let cause = entry(l, &format!("approvals.refusal.{code}")).unwrap();
                let next = entry(l, remedy(code).key()).unwrap();
                assert_eq!(sentence(l, code), format!("{cause} {next}"), "{l:?} {code}");
            }
        }
    }

    #[test]
    fn a_sentence_never_embeds_the_code_except_for_unknown() {
        for l in [Lang::Fr, Lang::En] {
            for code in known_codes() {
                let s = sentence(l, code);
                assert!(!s.contains(code), "{l:?} {code}: {s}");
            }
            let s = sentence(l, "a_code_from_the_future");
            assert!(s.contains("(a_code_from_the_future)"), "{l:?}: {s}");
            assert!(!s.contains(CODE_PLACEHOLDER), "{l:?}: {s}");
            // The unknown sentence still names a next step, and it is the
            // Companion's `diagnostics` one.
            assert!(
                s.ends_with(entry(l, Remedy::Diagnostics.key()).unwrap()),
                "{l:?}: {s}"
            );
        }
        assert_eq!(remedy("a_code_from_the_future"), Remedy::Diagnostics);
        assert_eq!(remedy(""), Remedy::Diagnostics);
    }

    #[test]
    fn remedy_table_matches_the_companions() {
        // `companion/src/lib/approvals/refusal.ts`, `TABLE`, copied by hand
        // and asserted per code: the one thing embedded as code rather than
        // as data, so the one thing a test has to hold in place.
        let companions = [
            ("malformed_request", Remedy::Diagnostics),
            ("approval_is_not_a_batch", Remedy::Diagnostics),
            ("unknown_value", Remedy::Diagnostics),
            ("approved_by_is_not_the_owner", Remedy::Diagnostics),
            ("unauthenticated", Remedy::SignIn),
            ("suggestion_not_found", Remedy::Reload),
            ("trigger_not_found", Remedy::None),
            ("suggestion_out_of_reach", Remedy::None),
            ("trigger_out_of_reach", Remedy::None),
            ("suggestion_expired", Remedy::None),
            ("consent_revoked", Remedy::None),
            ("consent_pending", Remedy::None),
            ("suggestion_was_never_consented", Remedy::None),
            ("already_approved", Remedy::Reload),
            ("trigger_has_no_room", Remedy::None),
            ("suggestion_unreadable", Remedy::None),
            ("bus_unreachable", Remedy::Retry),
            ("store_unavailable", Remedy::Retry),
            ("approval_published_but_not_recorded", Remedy::Reload),
            ("approvals_not_configured", Remedy::None),
        ];
        for (code, expected) in companions {
            assert_eq!(remedy(code), expected, "{code}");
        }
        // Nothing more and nothing less: the Companion's
        // `suggestions_not_configured` is deliberately absent (see `TABLE`).
        assert_eq!(known_codes().len(), companions.len());
        assert!(!known_codes().contains(&"suggestions_not_configured"));

        // `refusal.ts` lines 129–133: `sent: true` on exactly one row, and
        // no other row has the member at all. Telling the owner their reply
        // failed when it did not is how a message gets sent twice.
        let went_out: Vec<_> = known_codes().into_iter().filter(|c| sent(c)).collect();
        assert_eq!(went_out, ["approval_published_but_not_recorded"]);
        assert!(!sent("a_code_from_the_future"));
    }

    #[test]
    fn the_remedy_keys_are_the_companions_own() {
        // `REMEDY_TEXT` in `refusal.ts`: the one spelling that differs from
        // the variant's name is `sign-in` → `signIn`.
        assert_eq!(Remedy::Retry.key(), "approvals.remedy.retry");
        assert_eq!(Remedy::Reload.key(), "approvals.remedy.reload");
        assert_eq!(Remedy::SignIn.key(), "approvals.remedy.signIn");
        assert_eq!(Remedy::Diagnostics.key(), "approvals.remedy.diagnostics");
        assert_eq!(Remedy::None.key(), "approvals.remedy.none");
    }
}
