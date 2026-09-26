//! Every sentence the clerk writes, in French and in English.
//!
//! This module is the whole of what the clerk can say, and its shape is the
//! confidentiality argument: no function here takes a parameter that could
//! carry a contact's name, identifier, excerpt or room, so no sentence can
//! name one — not by a bug, not by a later edit to a caller. An
//! `approbations` post is `suggestion.body` verbatim plus the clerk's own
//! lines and the reference line last; a `journal` line names a network, the
//! account the reply was posted as, its `reach` and the approval's id, and
//! **no text**; an `activite` line about a consent decision names the
//! *kind* of subject and the state and networks, never the subject's Matrix
//! ID (`companion/src/lib/dashboard/model.ts` is the rule; ticket #265).
//!
//! One line of a post is not this module's: delivery — where the owner's
//! own account stands in the conversation (#216), so whether a ✅ would
//! reach anybody — is read from the Companion Gateway once, at posting
//! time, and written in the Companion's own sentence by
//! [`crate::refusals::delivery_line`]; when it could not be read the post
//! says so and says why ([`crate::refusals::delivery_unread_line`]), and
//! where it is read (the Approvals screen), rather than leaving a blank
//! the owner would read as "fine". [`approval_post`] takes that line as a
//! parameter, which is the one place this module's signature argument
//! leans on a caller: the string it is handed is one of those two
//! functions' — built from the catalogue and from two enum words the
//! Gateway answered, never from a body — and `consumers.rs` is the one
//! caller. The gesture line is what it says: since #284 a ✅, a ❌ or a
//! reply on the post is the decision.
//!
//! What the clerk answers in a post's **thread** (`thread_*`) is about the
//! owner's gesture and never about the contact: a refusal is the Companion
//! Gateway's code turned into the Companion's own sentence for it
//! ([`crate::refusals`]), so an owner deciding from Buzz reads exactly what
//! the approval screen would have shown them. A `code` is the Gateway's
//! vocabulary, not a contact's words, and the one sentence that embeds it
//! is the one for a code this build has never met.
//!
//! The sentences exist in [`Lang::Fr`] and [`Lang::En`]. The Companion's
//! other three languages fall back to English, which [`lang`] says so the
//! binary can warn about it once at startup. A state, a kind of subject or
//! a network this module has no word for is written as the contract spelled
//! it rather than dropped: a line that says less than the event is the
//! silence this project keeps shipping.
//!
//! Times are `HH:MM UTC`. The clerk knows no timezone — the bus carries
//! instants, not places — and a local-looking time that was really UTC
//! would be read as the wrong hour by anyone not in London in winter, so
//! the post says which it is and says that the local one is not known.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// The two languages the clerk's own sentences exist in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Fr,
    En,
}

/// The language to write in for a `CLERK_USER_LANGUAGE`, and whether that
/// is a fallback: `fr` and `en` are theirs, anything else is English.
pub fn lang(user_language: &str) -> (Lang, bool) {
    match user_language {
        "fr" => (Lang::Fr, false),
        "en" => (Lang::En, false),
        _ => (Lang::En, true),
    }
}

/// A network's name as the owner reads it — the contract's tag otherwise,
/// so a network this module has no name for is still named.
pub fn network_name(network: &str) -> &str {
    match network {
        "whatsapp" => "WhatsApp",
        "signal" => "Signal",
        "sms" => "SMS",
        "telegram" => "Telegram",
        "discord" => "Discord",
        "matrix" => "Matrix",
        other => other,
    }
}

/// One `approbations` post: the body verbatim between quotes, the clerk's
/// own lines, the reference line last.
///
/// The first line is what a forum post is listed by; the reference line
/// is last so that [`crate::reference::parse`] finds it below whatever the
/// body says. `expires_at` is shown as `HH:MM UTC` when it reads as RFC
/// 3339 and as the event spelled it otherwise — it is the event's own
/// field, never a contact's words, and a time the owner can see is better
/// than one the clerk could not format. `delivery_line` is the third line,
/// whole, as [`crate::refusals::delivery_line`] or
/// [`crate::refusals::delivery_unread_line`] wrote it — between the body
/// and the gesture line, so it is read before the ✅ it qualifies. The
/// gesture line names the three decisions `crate::decision` reads off the
/// post (#284): the two emoji it takes as approve and refuse, and a reply
/// as the edited text to send.
/// What the draft did before it wrote, as lines under the context (#377).
///
/// Lines and not a paragraph: the post is read on a phone, in the minute it
/// arrives, and a wall of prose above the text to approve would cost the
/// reading of the text itself.
///
/// Three rules the approval screen already follows, kept here word for word.
/// A **refused** read names its code and carries no count — `0 busy periods`
/// would read as an empty agenda. Nothing says what a step *learned*:
/// intervals are counted, not kept, and a question's answer is in the
/// conversation where the owner wrote it. And a step whose `kind` this build
/// does not know is skipped rather than guessed at, so a Gateway that grows a
/// fourth kind does not stop a clerk from posting.
pub fn path_lines(l: Lang, path: &[crate::gateway::Step]) -> String {
    let mut lines = String::new();
    for step in path {
        let line = match (step.kind.as_str(), l) {
            ("freebusy", l) => {
                let (Some(from), Some(to)) = (step.from.as_deref(), step.to.as_deref()) else {
                    continue;
                };
                // The day, not the instant: an owner reading this on a phone
                // needs to recognise the window, not to check it to the
                // second. A window this cannot format is not drawn at all,
                // for the reason the post never shows a half-fact.
                let (Some(from), Some(to)) = (day_utc(from), day_utc(to)) else {
                    continue;
                };
                match (l, step.outcome.as_deref(), step.intervals) {
                    (Lang::Fr, Some("served"), Some(intervals)) => format!(
                        "· a lu vos disponibilités du {from} au {to} — {intervals} périodes occupées"
                    ),
                    (Lang::Fr, Some(outcome), _) => format!(
                        "· a essayé de lire vos disponibilités du {from} au {to}, sans y arriver ({outcome})"
                    ),
                    (Lang::En, Some("served"), Some(intervals)) => format!(
                        "· read your free/busy from {from} to {to} — {intervals} busy periods"
                    ),
                    (Lang::En, Some(outcome), _) => format!(
                        "· tried to read your free/busy from {from} to {to}, and could not ({outcome})"
                    ),
                    _ => continue,
                }
            }
            ("event_facts", Lang::Fr) => match step.outcome.as_deref() {
                Some("served") => "· a demandé ce que porte un de vos événements".to_owned(),
                Some(outcome) => format!(
                    "· a essayé de demander ce que porte un de vos événements, sans y arriver ({outcome})"
                ),
                None => continue,
            },
            ("event_facts", Lang::En) => match step.outcome.as_deref() {
                Some("served") => "· asked what one of your events carries".to_owned(),
                Some(outcome) => format!(
                    "· tried to ask what one of your events carries, and could not ({outcome})"
                ),
                None => continue,
            },
            ("asked", l) => {
                let Some(asked) = step.asked.as_deref() else {
                    continue;
                };
                match l {
                    Lang::Fr => format!("· vous a demandé : {asked}"),
                    Lang::En => format!("· asked you: {asked}"),
                }
            }
            // A kind this build does not know: skipped, not guessed at.
            _ => continue,
        };
        lines.push_str(&line);
        lines.push('\n');
    }
    lines
}

pub fn approval_post(
    l: Lang,
    body: &str,
    network: &str,
    context: Option<&crate::events::Answering>,
    // `path`: what the draft did before it wrote, already rendered as lines
    // by `path_lines` — empty for a draft that looked nothing up, which is
    // the post exactly as it was (#377).
    path: &str,
    expires_at: Option<&str>,
    delivery_line: &str,
    reference: &str,
) -> String {
    let network = network_name(network);
    let expiry = expires_at.map(|at| match (l, day_utc(at), clock_utc(at)) {
        (Lang::Fr, Some(day), Some(clock)) => format!(" · expire le {day} à {clock}"),
        (Lang::Fr, ..) => format!(" · expire à {at}"),
        (Lang::En, Some(day), Some(clock)) => format!(" · expires on {day} at {clock}"),
        (Lang::En, ..) => format!(" · expires at {at}"),
    });
    let expiry = expiry.as_deref().unwrap_or("");
    // What the persona says it is answering, above the text it proposes
    // (#335): the owner reads why before they read what. The contact's
    // name is prefixed only when the summary does not already carry it,
    // since the persona writes as a person would.
    let context = context
        .map(|answering| match &answering.contact {
            Some(contact) if !answering.summary.contains(contact.as_str()) => {
                format!("{contact} · {}\n", answering.summary)
            }
            _ => format!("{}\n", answering.summary),
        })
        .unwrap_or_default();
    match l {
        Lang::Fr => format!(
            "Réponse proposée · {network}{expiry}\n\
             {context}\
             {path}\
             « {body} »\n\
             {delivery_line}\n\
             ✅ envoyer tel quel · ❌ refuser · répondre ici pour envoyer un autre texte\n\
             {reference}"
        ),
        Lang::En => format!(
            "Proposed reply · {network}{expiry}\n\
             {context}\
             {path}\
             “{body}”\n\
             {delivery_line}\n\
             ✅ send as is · ❌ decline · reply here to send a different text\n\
             {reference}"
        ),
    }
}

/// The thread answer to a gesture by a key that is not the owner's (#284):
/// the relay let a member react, and the clerk did nothing with it, and
/// says so once rather than silently ignoring a person who thinks they
/// approved.
pub fn thread_not_the_owner(l: Lang) -> String {
    match l {
        Lang::Fr => "Seul le propriétaire approuve.".to_owned(),
        Lang::En => "Only the owner approves.".to_owned(),
    }
}

/// The thread answer to a refusal by the Companion Gateway: "not sent",
/// then the Companion's own sentence for the code ([`crate::refusals`]) —
/// cause and next step, exactly as the approval screen would show them.
///
/// One refusal is not a failure ([`crate::refusals::sent`]): the reply went
/// out and the Companion Gateway could not record it. That one begins
/// "sent but not recorded", because "not sent" before the Companion's own
/// "Your reply was sent" would be a false first word — and the word the
/// owner acts on, by sending it again.
pub fn thread_refused(l: Lang, code: &str) -> String {
    let sentence = crate::refusals::sentence(l, code);
    match (l, crate::refusals::sent(code)) {
        (Lang::Fr, true) => format!("Envoyée mais non enregistrée — {sentence}"),
        (Lang::Fr, false) => format!("Non envoyée — {sentence}"),
        (Lang::En, true) => format!("Sent but not recorded — {sentence}"),
        (Lang::En, false) => format!("Not sent — {sentence}"),
    }
}

/// The thread answer when the owner's gesture could not be carried before
/// the suggestion expired, because the Companion Gateway never answered:
/// the post is about to be swept, and this line is what the owner finds
/// in its place — with the one thing they can do, which is to decide
/// again on the next one.
pub fn thread_not_recorded(l: Lang) -> String {
    match l {
        Lang::Fr => "Non enregistrée — le Companion Gateway n’a pas répondu avant l’expiration ; \
                     réagis de nouveau."
            .to_owned(),
        Lang::En => "Not recorded — the Companion Gateway did not answer before the expiry; react \
                     again."
            .to_owned(),
    }
}

/// The thread answer when the Companion Gateway refused the clerk's own
/// session: the owner revoked the `Buzz` device on the dashboard — a
/// legitimate act, and this names the script that issues a new one rather
/// than leaving "unauthenticated" to be read as a defect.
pub fn thread_revoked(l: Lang) -> String {
    match l {
        Lang::Fr => "Non enregistrée — l’appareil « Buzz » a été révoqué sur le tableau de bord ; \
                     relance provision-clerk-device.sh."
            .to_owned(),
        Lang::En => "Not recorded — the “Buzz” device was revoked on the dashboard; run \
                     provision-clerk-device.sh again."
            .to_owned(),
    }
}

/// The thread answer to an owner's ✅ or edited reply on a post whose
/// suggestion had already expired when the clerk read the gesture: the
/// decision came in the suggestion's last seconds, the Companion Gateway
/// would refuse it as `suggestion_expired`, and the sweep is about to
/// delete the post — so the owner is told, once, rather than left to read
/// "expired undecided" in `activite` about a suggestion they decided.
pub fn thread_expired(l: Lang) -> String {
    match l {
        Lang::Fr => "Non envoyée — la suggestion avait expiré.".to_owned(),
        Lang::En => "Not sent — the suggestion had expired.".to_owned(),
    }
}

/// The network named on one of the clerk's own `approbations` posts, read
/// back off its first line — the inverse of [`approval_post`]'s
/// `Réponse proposée · WhatsApp · expire à …`, kept beside it so the
/// layout and its reader are one edit. As the owner reads it (`WhatsApp`,
/// not `whatsapp`), which [`network_name`] passes through unchanged, for
/// the `activite` line the decisions loop writes when a suggestion is
/// approved or declined from Buzz (#284). `None` for a post not in that
/// shape. Read off the post rather than kept, because the relay is the
/// clerk's only memory (ADR 0035), and off the first line rather than the
/// reference line, which names the suggestion and its expiry and nothing
/// else.
pub fn network_off_post(post_content: &str) -> Option<&str> {
    let first = post_content.lines().next()?;
    let mut parts = first.split(" · ");
    parts.next()?;
    parts
        .next()
        .map(str::trim)
        .filter(|network| !network.is_empty())
}

/// ` · WhatsApp` for a line that names a network, nothing for one that
/// names none — a post the loop could not read a network off must not
/// leave a dangling separator.
fn network_suffix(network: &str) -> String {
    if network.is_empty() {
        String::new()
    } else {
        format!(" · {}", network_name(network))
    }
}

/// An `activite` line: a suggestion was approved from Buzz, edited or as
/// proposed, on which network — and never what it said.
pub fn activity_approved(l: Lang, network: &str, edited: bool) -> String {
    let network = network_suffix(network);
    match (l, edited) {
        (Lang::Fr, true) => format!("Suggestion approuvée depuis Buzz (modifiée){network}"),
        (Lang::Fr, false) => format!("Suggestion approuvée depuis Buzz{network}"),
        (Lang::En, true) => format!("Suggestion approved from Buzz (edited){network}"),
        (Lang::En, false) => format!("Suggestion approved from Buzz{network}"),
    }
}

/// An `activite` line: the owner refused a suggestion from Buzz with ❌.
/// The line says the refusal went nowhere, because it did not: no route on
/// the Companion Gateway refuses a suggestion, so the clerk deleted the post
/// and told nobody else (the approval screen's own `dismissed.ts` is the
/// same decision, with the same disclosure).
pub fn activity_refused_locally(l: Lang, network: &str) -> String {
    let network = network_suffix(network);
    match l {
        Lang::Fr => format!("Suggestion refusée depuis Buzz (❌, non transmise){network}"),
        Lang::En => format!("Suggestion declined from Buzz (❌, not forwarded){network}"),
    }
}

/// One `journal` line: what went out and where, as which account, whether
/// it reached the contact, which approval — and no text.
///
/// `reach` is the Sensor's word on the `.posted` report (#216): `contact`
/// or `nobody`, and a `nobody` line reads as one and names #123, the
/// ticket that explains an account the bridge ignores. The approval id is
/// shown by its first twelve characters: enough to find it in
/// `GET /api/approvals/{id}`, short enough to read. `at` is the report's
/// time, `HH:MM UTC` when it reads as RFC 3339, as given otherwise, and
/// left out when empty. `edited` is the approval's own flag — the reply
/// went out with a text the owner wrote rather than the persona's (#284) —
/// and it ends the line when true; it still says nothing of the text.
pub fn journal_line(
    l: Lang,
    network: &str,
    reach: &str,
    posted_as: &str,
    approval_id: &str,
    at: &str,
    edited: bool,
) -> String {
    let network = network_name(network);
    let approval = short_id(approval_id);
    let when = if at.is_empty() {
        String::new()
    } else {
        format!(" · {}", clock_utc(at).unwrap_or_else(|| at.to_owned()))
    };
    let edited = match (l, edited) {
        (_, false) => "",
        (Lang::Fr, true) => " · modifiée",
        (Lang::En, true) => " · edited",
    };
    match (l, reach) {
        (Lang::Fr, "contact") => format!(
            "Partie · {network} · postée en tant que {posted_as} · a atteint le contact · \
             approbation {approval}{when}{edited}"
        ),
        (Lang::Fr, "nobody") => format!(
            "Personne ne l’a reçue · {network} · postée en tant que {posted_as} — un compte que le \
             bridge ignore (#123) · approbation {approval}{when}{edited}"
        ),
        (Lang::Fr, other) => format!(
            "Partie · {network} · postée en tant que {posted_as} · portée : {other} · approbation \
             {approval}{when}{edited}"
        ),
        (Lang::En, "contact") => format!(
            "Sent · {network} · posted as {posted_as} · reached the contact · approval \
             {approval}{when}{edited}"
        ),
        (Lang::En, "nobody") => format!(
            "Nobody received it · {network} · posted as {posted_as} — an account the bridge \
             ignores (#123) · approval {approval}{when}{edited}"
        ),
        (Lang::En, other) => format!(
            "Sent · {network} · posted as {posted_as} · reach: {other} · approval \
             {approval}{when}{edited}"
        ),
    }
}

/// One `journal` line for a reply that never left (#311): which network it
/// was meant for, what stopped it, which approval — and no text.
///
/// It reads as a failure, not as a variant of "sent": the whole defect this
/// answers is that an approval whose reply was given up on looked posted.
/// The reason is the sender's own words, capped by the sender; a report
/// without one still gets a line, because "it did not leave" is the fact
/// that matters and the log holds the rest.
pub fn journal_undelivered_line(
    l: Lang,
    network: &str,
    reason: Option<&str>,
    approval_id: &str,
    at: &str,
) -> String {
    let network = network_name(network);
    let approval = short_id(approval_id);
    let when = if at.is_empty() {
        String::new()
    } else {
        format!(" · {}", clock_utc(at).unwrap_or_else(|| at.to_owned()))
    };
    let because = match (l, reason) {
        (Lang::Fr, Some(reason)) => format!(" · cause : {reason}"),
        (Lang::Fr, None) => " · cause non indiquée".to_owned(),
        (Lang::En, Some(reason)) => format!(" · cause: {reason}"),
        (Lang::En, None) => " · no cause was given".to_owned(),
    };
    match l {
        Lang::Fr => format!(
            "Non partie · {network} · abandonnée par le service qui devait l’envoyer{because} \
             · approbation {approval}{when}"
        ),
        Lang::En => format!(
            "Never sent · {network} · the service that had to send it gave up{because} · \
             approval {approval}{when}"
        ),
    }
}

/// A bridge's state change. The bridge is named by its id alone — a
/// deployment's bridge instance, never a network (ADR 0005), and by the
/// contract's own pattern always `bridge-…`, so a word "Bridge" before it
/// would read twice in both languages — and the state by the contract's
/// word, translated when there is a translation.
pub fn activity_bridge(l: Lang, bridge_id: &str, state: &str) -> String {
    let state = match (l, state) {
        (Lang::Fr, "starting") => "démarre",
        (Lang::Fr, "connected") => "connecté",
        (Lang::Fr, "degraded") => "dégradé",
        (Lang::Fr, "disconnected") => "déconnecté",
        (Lang::Fr, "session_expired") => "session expirée",
        (_, other) => other,
    };
    format!("{bridge_id} · {state}")
}

/// A consent decision, by the kind of subject it is about, its new state
/// and the networks it covers — never the subject's identifier.
///
/// There is no parameter for the subject's id, and that is the point: on
/// `activite` a consent line says that *a contact* was granted on WhatsApp,
/// which is what calls for a look, and not which one, which is a sender
/// identity (`companion/src/lib/dashboard/model.ts`).
pub fn activity_consent(
    l: Lang,
    subject_type: &str,
    new_state: &str,
    networks: &[String],
) -> String {
    let kind = match (l, subject_type) {
        (Lang::Fr, "contact") => "un contact",
        (Lang::Fr, "persona") => "un persona",
        (Lang::Fr, "network") => "un réseau entier",
        (Lang::En, "contact") => "a contact",
        (Lang::En, "persona") => "a persona",
        (Lang::En, "network") => "a whole network",
        (_, other) => other,
    };
    let state = match (l, new_state) {
        (Lang::Fr, "granted") => "accordé",
        (Lang::Fr, "pending") => "en attente",
        (Lang::Fr, "revoked") => "révoqué",
        (_, other) => other,
    };
    let networks = networks
        .iter()
        .map(|network| network_name(network))
        .collect::<Vec<_>>()
        .join(", ");
    match l {
        Lang::Fr => format!("Consentement · {kind} · {state} · {networks}"),
        Lang::En => format!("Consent · {kind} · {state} · {networks}"),
    }
}

/// A suggestion was produced and is waiting in `approbations`. The channel
/// is named as the owner's provisioning named it, in both languages.
pub fn activity_suggested(l: Lang, network: &str) -> String {
    let network = network_name(network);
    match l {
        Lang::Fr => format!("L’assistant a proposé une réponse · {network} → approbations"),
        Lang::En => format!("The assistant proposed a reply · {network} → approbations"),
    }
}

/// A suggestion expired undecided and its post was deleted (#219).
pub fn activity_expired(l: Lang) -> String {
    match l {
        Lang::Fr => "Une proposition a expiré sans décision · retirée d’approbations".to_owned(),
        Lang::En => "A proposal expired undecided · removed from approbations".to_owned(),
    }
}

/// `HH:MM UTC` for an RFC 3339 instant, whatever offset it was written
/// with; `None` when it does not read as one.
fn clock_utc(rfc3339: &str) -> Option<String> {
    let at = OffsetDateTime::parse(rfc3339, &Rfc3339).ok()?.to_utc();
    Some(format!("{:02}:{:02} UTC", at.hour(), at.minute()))
}

/// The day an instant falls on, `DD/MM`, for a deadline that is not
/// today's (#326). A suggestion's window is the deployment's to set and is
/// often a day: `expire à 09:52 UTC`, read at 10:30, says a proposal is
/// dead when it has twenty-three hours left — and the owner acts on that,
/// or worse does not act at all.
fn day_utc(rfc3339: &str) -> Option<String> {
    let at = OffsetDateTime::parse(rfc3339, &Rfc3339).ok()?.to_utc();
    Some(format!("{:02}/{:02}", at.day(), u8::from(at.month())))
}

/// The first twelve characters of an id and an ellipsis, or the whole id
/// when it is no longer than that.
fn short_id(id: &str) -> String {
    match id.char_indices().nth(12) {
        Some((cut, _)) => format!("{}…", &id[..cut]),
        None => id.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE: &str = "twalk:suggestion:319be8ff15d5dee005c8aa27119b983da8223959987e5dbc639d81e370b5ef9b expires 2026-09-17T11:00:00Z";
    const APPROVAL_ID: &str = "8d3fb3fe9d2d8a4a1c2c0b5b0e6b3f0a1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f60";

    /// The delivery line a post is handed, as `crate::refusals` writes it
    /// when the write half is not configured — the shape every post has on
    /// a deployment that reads nothing.
    fn unread(l: Lang) -> String {
        crate::refusals::delivery_unread_line(l, crate::refusals::Unread::NoDevice)
    }

    #[test]
    fn lang_falls_back_to_english_for_it_es_de() {
        assert_eq!(lang("fr"), (Lang::Fr, false));
        assert_eq!(lang("en"), (Lang::En, false));
        for other in ["it", "es", "de"] {
            assert_eq!(lang(other), (Lang::En, true), "{other}");
        }
        assert_eq!(lang("pt"), (Lang::En, true));
        assert_eq!(lang(""), (Lang::En, true));
    }

    #[test]
    fn a_network_is_named_as_the_owner_reads_it() {
        assert_eq!(network_name("whatsapp"), "WhatsApp");
        assert_eq!(network_name("signal"), "Signal");
        assert_eq!(network_name("sms"), "SMS");
        assert_eq!(network_name("telegram"), "Telegram");
        assert_eq!(network_name("discord"), "Discord");
        assert_eq!(network_name("matrix"), "Matrix");
        assert_eq!(network_name("irc"), "irc");
    }

    #[test]
    fn a_post_contains_the_body_and_the_reference_and_nothing_else_of_the_event() {
        // The whole of what an inbound event knows about a contact — and
        // what the approval screen cannot show (#160, ADR 0012). None of
        // it is a parameter of `approval_post`: the function cannot be
        // handed a contact, so the assertion is on the signature as much as
        // on the string. The body is chosen not to contain any of them, so
        // that their absence from the post is the function's doing.
        let markers = [
            "@whatsapp_33612345678:example.com",
            "Alice Martin",
            "+33 6 12 34 56 78",
            "!portal:example.com",
            "On se voit à 20h ?",
        ];
        let body = "Pas de problème, à 20h !";
        for l in [Lang::Fr, Lang::En] {
            let post = approval_post(
                l,
                body,
                "whatsapp",
                None,
                "",
                Some("2026-09-17T11:00:00Z"),
                &unread(l),
                REFERENCE,
            );
            assert!(post.contains(body), "{post}");
            assert_eq!(post.lines().last(), Some(REFERENCE), "{post}");
            assert!(post.contains("WhatsApp"), "{post}");
            assert!(post.contains("11:00 UTC"), "{post}");
            for marker in markers {
                assert!(!post.contains(marker), "{marker} in {post}");
            }
        }
    }

    #[test]
    fn the_french_post_is_laid_out_as_the_ticket_shows() {
        let post = approval_post(
            Lang::Fr,
            "Pas de problème, à 20h !",
            "whatsapp",
            Some(&crate::events::Answering {
                contact: Some("Aïcha Benali".to_owned()),
                summary: "Aïcha Benali demande si le dîner tient toujours.".to_owned(),
            }),
            "",
            Some("2026-09-17T21:41:00Z"),
            &unread(Lang::Fr),
            REFERENCE,
        );
        let expected = format!(
            "Réponse proposée · WhatsApp · expire le 17/09 à 21:41 UTC\n\
             Aïcha Benali demande si le dîner tient toujours.\n\
             « Pas de problème, à 20h ! »\n\
             Livraison : non lue par le greffier (aucun device configuré) — l’écran Approbations \
             la connaît.\n\
             ✅ envoyer tel quel · ❌ refuser · répondre ici pour envoyer un autre texte\n\
             {REFERENCE}"
        );
        assert_eq!(post, expected);
    }

    #[test]
    fn the_english_post_mirrors_the_french_one() {
        let post = approval_post(
            Lang::En,
            "No problem, see you at 8!",
            "signal",
            Some(&crate::events::Answering {
                contact: Some("Aïcha Benali".to_owned()),
                summary: "She asks whether dinner is still on.".to_owned(),
            }),
            "",
            Some("2026-09-17T21:41:00Z"),
            &unread(Lang::En),
            REFERENCE,
        );
        let expected = format!(
            "Proposed reply · Signal · expires on 17/09 at 21:41 UTC\n\
             Aïcha Benali · She asks whether dinner is still on.\n\
             “No problem, see you at 8!”\n\
             Delivery: not read by the clerk (no device configured) — the Approvals screen knows \
             it.\n\
             ✅ send as is · ❌ decline · reply here to send a different text\n\
             {REFERENCE}"
        );
        assert_eq!(post, expected);
    }

    /// #335: the name is prefixed only when the summary does not already
    /// carry it, since a persona writes as a person would; and a post for
    /// a suggestion with no context is the post as it was.
    #[test]
    fn the_context_line_names_the_contact_once_or_not_at_all() {
        let post = |contact: Option<&str>, summary: &str| {
            approval_post(
                Lang::Fr,
                "Oui",
                "whatsapp",
                Some(&crate::events::Answering {
                    contact: contact.map(str::to_owned),
                    summary: summary.to_owned(),
                }),
                "",
                None,
                &unread(Lang::Fr),
                REFERENCE,
            )
        };
        assert!(
            post(Some("Aïcha"), "Aïcha demande l'heure.").contains("\nAïcha demande l'heure.\n")
        );
        assert!(post(Some("Aïcha"), "Elle demande l'heure.")
            .contains("\nAïcha · Elle demande l'heure.\n"));
        assert!(post(None, "Elle demande l'heure.").contains("\nElle demande l'heure.\n"));
        let without = approval_post(
            Lang::Fr,
            "Oui",
            "whatsapp",
            None,
            "",
            None,
            &unread(Lang::Fr),
            REFERENCE,
        );
        assert!(
            without.starts_with("Réponse proposée · WhatsApp\n« Oui »"),
            "a suggestion with no context is posted as before: {without}"
        );
    }

    /// #377: the post says what the draft did, so Buzz alone is enough.
    #[test]
    fn the_post_says_what_the_draft_did_before_it_wrote() {
        use crate::gateway::Step;
        let step = |kind: &str| Step {
            kind: kind.to_owned(),
            from: Some("2026-09-28T06:00:00Z".to_owned()),
            to: Some("2026-10-02T18:00:00Z".to_owned()),
            outcome: Some("served".to_owned()),
            intervals: Some(4),
            asked: Some("de quel projet il parle".to_owned()),
        };

        let lines = path_lines(Lang::Fr, &[step("freebusy"), step("asked")]);
        assert!(
            lines.contains("a lu vos disponibilités du 28/09 au 02/10 — 4 périodes occupées"),
            "{lines}"
        );
        assert!(lines.contains("vous a demandé : de quel projet il parle"), "{lines}");
        assert!(
            path_lines(Lang::En, &[step("freebusy")]).contains("read your free/busy"),
            "both languages, and no English sentence left in a French post"
        );

        // A refused read names its code and carries **no count**: `0 busy
        // periods` would read as an empty agenda.
        let refused = Step {
            outcome: Some("window_too_wide".to_owned()),
            intervals: None,
            ..step("freebusy")
        };
        let refused = path_lines(Lang::Fr, &[refused]);
        assert!(refused.contains("sans y arriver (window_too_wide)"), "{refused}");
        assert!(!refused.contains("0 "), "a refused read was given a count: {refused}");

        // A kind this build does not know is skipped, not guessed at: a
        // Gateway that grows a fourth kind must not stop a clerk posting.
        assert_eq!(path_lines(Lang::Fr, &[step("something_new")]), "");
        assert_eq!(path_lines(Lang::Fr, &[]), "");

        // And in the post: under the context, above the text to approve —
        // the order the owner decides in.
        let post = approval_post(
            Lang::Fr,
            "Jeudi 12h30 me va",
            "email",
            Some(&crate::events::Answering {
                contact: Some("Christelle".to_owned()),
                summary: "Christelle demande trois créneaux.".to_owned(),
            }),
            &lines,
            None,
            &unread(Lang::Fr),
            REFERENCE,
        );
        let read: Vec<&str> = post.lines().collect();
        // The contact is not prefixed when the summary already names them,
        // which is this post's own older rule and not this ticket's.
        assert_eq!(read[1], "Christelle demande trois créneaux.");
        assert!(read[2].starts_with("· a lu vos disponibilités"), "{post}");
        assert!(read[3].starts_with("· vous a demandé"), "{post}");
        assert!(read[4].starts_with("« Jeudi"), "{post}");

        // A draft that looked nothing up is posted exactly as before.
        let without = approval_post(
            Lang::Fr,
            "Oui",
            "email",
            None,
            "",
            None,
            &unread(Lang::Fr),
            REFERENCE,
        );
        assert_eq!(
            without.lines().nth(1),
            Some("« Oui »"),
            "a draft that looked nothing up is posted exactly as before: {without}"
        );
    }

    /// #326: a window longer than an hour read as already expired, because
    /// the post said a time of day and nothing else.
    #[test]
    fn a_deadline_says_which_day_it_falls_on() {
        let post = approval_post(
            Lang::Fr,
            "Oui",
            "whatsapp",
            None,
            "",
            Some("2026-09-24T09:52:09Z"),
            &unread(Lang::Fr),
            REFERENCE,
        );
        assert!(post.contains("expire le 24/09 à 09:52 UTC"), "{post}");
        assert!(
            !post.contains("heure locale"),
            "the parenthesis went with the ambiguity it was apologising for"
        );
    }

    #[test]
    fn the_delivery_line_is_the_third_line_whatever_it_says() {
        // A line read from the Companion Gateway takes the same place as
        // the unread one: between the body and the gesture, so it is read
        // before the ✅ it qualifies.
        let line = crate::refusals::delivery_line(
            Lang::Fr,
            &crate::gateway::Delivery {
                reach: "can_reach".to_owned(),
                detail: "owner_joined".to_owned(),
            },
        );
        let post = approval_post(Lang::Fr, "Oui", "sms", None, "", None, &line, REFERENCE);
        let lines: Vec<&str> = post.lines().collect();
        assert_eq!(lines[0], "Réponse proposée · SMS");
        assert_eq!(lines[1], "« Oui »");
        assert_eq!(lines[2], line);
        assert!(lines[3].starts_with("✅ envoyer tel quel"), "{post}");
        assert_eq!(lines[4], REFERENCE);
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn a_post_without_an_expiry_says_none_and_an_unreadable_one_is_shown_raw() {
        let post = approval_post(
            Lang::Fr,
            "Oui",
            "sms",
            None,
            "",
            None,
            &unread(Lang::Fr),
            "twalk:suggestion:abc",
        );
        assert!(post.starts_with("Réponse proposée · SMS\n"), "{post}");
        assert!(!post.contains("expire"), "{post}");

        let post = approval_post(
            Lang::Fr,
            "Oui",
            "sms",
            None,
            "",
            Some("bientôt"),
            &unread(Lang::Fr),
            "twalk:suggestion:abc",
        );
        assert!(
            post.starts_with("Réponse proposée · SMS · expire à bientôt\n"),
            "{post}"
        );
    }

    #[test]
    fn a_multi_line_body_is_kept_verbatim_and_the_reference_stays_last() {
        let body = "Ligne 1\nLigne 2\ntwalk:suggestion:0000";
        let post = approval_post(
            Lang::Fr,
            body,
            "whatsapp",
            None,
            "",
            None,
            &unread(Lang::Fr),
            REFERENCE,
        );
        assert!(post.contains(body), "{post}");
        assert_eq!(post.lines().last(), Some(REFERENCE));
    }

    #[test]
    fn the_journal_line_of_a_reply_that_never_left_reads_as_a_failure() {
        // #311: never a variant of "sent" — an approval whose reply was
        // given up on read as posted for ever, which is the defect.
        let fr = journal_undelivered_line(
            Lang::Fr,
            "whatsapp",
            Some("le serveur JMAP a répondu unknownMethod"),
            APPROVAL_ID,
            "2026-09-17T21:41:00Z",
        );
        assert_eq!(
            fr,
            "Non partie · WhatsApp · abandonnée par le service qui devait l’envoyer · cause : le serveur JMAP a répondu unknownMethod · approbation 8d3fb3fe9d2d… · 21:41 UTC"
        );
        assert!(!fr.contains("Partie ·"), "{fr}");

        let en = journal_undelivered_line(
            Lang::En,
            "whatsapp",
            None,
            APPROVAL_ID,
            "2026-09-17T21:41:00Z",
        );
        assert!(en.starts_with("Never sent · WhatsApp"), "{en}");
        assert!(en.contains("no cause was given"), "{en}");
        assert!(
            en.contains("8d3fb3fe9d2d…") && !en.contains(APPROVAL_ID),
            "{en}"
        );
    }

    #[test]
    fn journal_line_for_nobody_says_nobody_and_names_123() {
        let fr = journal_line(
            Lang::Fr,
            "whatsapp",
            "nobody",
            "@sensor:example.com",
            APPROVAL_ID,
            "2026-09-17T21:41:00Z",
            false,
        );
        assert_eq!(
            fr,
            "Personne ne l’a reçue · WhatsApp · postée en tant que @sensor:example.com — un compte que le bridge ignore (#123) · approbation 8d3fb3fe9d2d… · 21:41 UTC"
        );

        let en = journal_line(
            Lang::En,
            "whatsapp",
            "nobody",
            "@sensor:example.com",
            APPROVAL_ID,
            "2026-09-17T21:41:00Z",
            false,
        );
        assert!(en.starts_with("Nobody received it · WhatsApp"), "{en}");
        assert!(en.contains("#123"), "{en}");
        assert!(en.contains("8d3fb3fe9d2d…"), "{en}");
        assert!(!en.contains(APPROVAL_ID), "{en}");
    }

    #[test]
    fn journal_line_for_the_contact_says_it_reached_them() {
        let fr = journal_line(
            Lang::Fr,
            "signal",
            "contact",
            "@michel:example.com",
            APPROVAL_ID,
            "2026-09-17T21:41:00Z",
            false,
        );
        assert_eq!(
            fr,
            "Partie · Signal · postée en tant que @michel:example.com · a atteint le contact · approbation 8d3fb3fe9d2d… · 21:41 UTC"
        );
        assert!(!fr.contains("#123"));

        let en = journal_line(
            Lang::En,
            "signal",
            "contact",
            "@michel:example.com",
            APPROVAL_ID,
            "2026-09-17T21:41:00Z",
            false,
        );
        assert_eq!(
            en,
            "Sent · Signal · posted as @michel:example.com · reached the contact · approval 8d3fb3fe9d2d… · 21:41 UTC"
        );
    }

    #[test]
    fn a_short_approval_id_is_shown_whole_and_an_unreadable_time_raw() {
        let fr = journal_line(Lang::Fr, "sms", "contact", "@m:x", "abc", "now-ish", false);
        assert!(fr.ends_with("approbation abc · now-ish"), "{fr}");
        let fr = journal_line(Lang::Fr, "sms", "contact", "@m:x", "abc", "", false);
        assert!(fr.ends_with("approbation abc"), "{fr}");
    }

    #[test]
    fn activity_consent_never_carries_the_subject() {
        // The function has no parameter for the subject's id; what it is
        // given is the kind, the state and the networks, and the sentence
        // is exactly those.
        let networks = vec!["whatsapp".to_owned(), "signal".to_owned()];
        let fr = activity_consent(Lang::Fr, "contact", "granted", &networks);
        assert_eq!(fr, "Consentement · un contact · accordé · WhatsApp, Signal");
        assert!(!fr.contains('@'), "{fr}");

        let fr = activity_consent(Lang::Fr, "persona", "revoked", &networks[..1]);
        assert_eq!(fr, "Consentement · un persona · révoqué · WhatsApp");

        let fr = activity_consent(Lang::Fr, "network", "pending", &networks[1..]);
        assert_eq!(fr, "Consentement · un réseau entier · en attente · Signal");

        let en = activity_consent(Lang::En, "contact", "granted", &networks);
        assert_eq!(en, "Consent · a contact · granted · WhatsApp, Signal");
        let en = activity_consent(Lang::En, "persona", "revoked", &networks);
        assert_eq!(en, "Consent · a persona · revoked · WhatsApp, Signal");
        let en = activity_consent(Lang::En, "network", "pending", &networks);
        assert_eq!(en, "Consent · a whole network · pending · WhatsApp, Signal");

        // A kind or a state this module has no word for is shown as the
        // contract spelled it, never dropped: a line that says less than
        // the event is the silence this project keeps shipping.
        let fr = activity_consent(Lang::Fr, "device", "frozen", &networks);
        assert_eq!(fr, "Consentement · device · frozen · WhatsApp, Signal");
    }

    #[test]
    fn activity_bridge_names_the_bridge_and_its_state() {
        assert_eq!(
            activity_bridge(Lang::Fr, "bridge-whatsapp-1", "disconnected"),
            "bridge-whatsapp-1 · déconnecté"
        );
        assert_eq!(
            activity_bridge(Lang::Fr, "bridge-whatsapp-1", "connected"),
            "bridge-whatsapp-1 · connecté"
        );
        assert_eq!(
            activity_bridge(Lang::Fr, "bridge-signal-1", "session_expired"),
            "bridge-signal-1 · session expirée"
        );
        assert_eq!(
            activity_bridge(Lang::En, "bridge-signal-1", "degraded"),
            "bridge-signal-1 · degraded"
        );
        assert_eq!(
            activity_bridge(Lang::En, "bridge-gmessages-1", "starting"),
            "bridge-gmessages-1 · starting"
        );
        assert_eq!(
            activity_bridge(Lang::En, "bridge-gmessages-1", "rebooting"),
            "bridge-gmessages-1 · rebooting"
        );
    }

    #[test]
    fn activity_suggested_and_expired_point_at_approbations() {
        assert_eq!(
            activity_suggested(Lang::Fr, "whatsapp"),
            "L’assistant a proposé une réponse · WhatsApp → approbations"
        );
        assert_eq!(
            activity_suggested(Lang::En, "whatsapp"),
            "The assistant proposed a reply · WhatsApp → approbations"
        );
        assert_eq!(
            activity_expired(Lang::Fr),
            "Une proposition a expiré sans décision · retirée d’approbations"
        );
        assert_eq!(
            activity_expired(Lang::En),
            "A proposal expired undecided · removed from approbations"
        );
    }

    #[test]
    fn nothing_the_clerk_writes_in_a_post_names_a_ticket() {
        // A post once named the ticket that would make the clerk read
        // delivery, and a post that cites a ticket as "to come" is wrong
        // the day it ships. So the clerk's own lines carry no `#` at all,
        // whichever delivery line they are handed — asserted on the post
        // with that line taken out, since the line is the Companion's
        // sentence and not this module's to police.
        for l in [Lang::Fr, Lang::En] {
            for line in [
                unread(l),
                crate::refusals::delivery_unread_line(
                    l,
                    crate::refusals::Unread::GatewayUnreachable,
                ),
                crate::refusals::delivery_line(
                    l,
                    &crate::gateway::Delivery {
                        reach: "unknown".to_owned(),
                        detail: "not_a_known_portal".to_owned(),
                    },
                ),
            ] {
                let post = approval_post(
                    l,
                    "Oui",
                    "sms",
                    None,
                    "",
                    Some("2026-09-17T11:00:00Z"),
                    &line,
                    REFERENCE,
                );
                assert!(!post.contains("à venir"), "{post}");
                assert!(!post.contains("coming"), "{post}");
                let own = post.replace(&line, "");
                assert!(!own.contains('#'), "{post}");
            }
        }
    }

    #[test]
    fn journal_line_says_edited_only_when_the_reply_was() {
        let fr = journal_line(
            Lang::Fr,
            "signal",
            "contact",
            "@michel:example.com",
            APPROVAL_ID,
            "2026-09-17T21:41:00Z",
            true,
        );
        assert_eq!(
            fr,
            "Partie · Signal · postée en tant que @michel:example.com · a atteint le contact · approbation 8d3fb3fe9d2d… · 21:41 UTC · modifiée"
        );
        let en = journal_line(
            Lang::En,
            "signal",
            "contact",
            "@michel:example.com",
            APPROVAL_ID,
            "2026-09-17T21:41:00Z",
            true,
        );
        assert_eq!(
            en,
            "Sent · Signal · posted as @michel:example.com · reached the contact · approval 8d3fb3fe9d2d… · 21:41 UTC · edited"
        );
        // Without a time, the flag still ends the line; and a line that was
        // not edited does not say so.
        let fr = journal_line(Lang::Fr, "sms", "nobody", "@m:x", "abc", "", true);
        assert!(fr.ends_with("approbation abc · modifiée"), "{fr}");
        let en = journal_line(Lang::En, "sms", "contact", "@m:x", "abc", "", false);
        assert!(!en.contains("edited"), "{en}");
    }

    #[test]
    fn thread_lines_are_exactly_these() {
        assert_eq!(
            thread_not_the_owner(Lang::Fr),
            "Seul le propriétaire approuve."
        );
        assert_eq!(thread_not_the_owner(Lang::En), "Only the owner approves.");
        assert_eq!(
            thread_not_recorded(Lang::Fr),
            "Non enregistrée — le Companion Gateway n’a pas répondu avant l’expiration ; réagis de nouveau."
        );
        assert_eq!(
            thread_not_recorded(Lang::En),
            "Not recorded — the Companion Gateway did not answer before the expiry; react again."
        );
        assert_eq!(
            thread_revoked(Lang::Fr),
            "Non enregistrée — l’appareil « Buzz » a été révoqué sur le tableau de bord ; relance provision-clerk-device.sh."
        );
        assert_eq!(
            thread_revoked(Lang::En),
            "Not recorded — the “Buzz” device was revoked on the dashboard; run provision-clerk-device.sh again."
        );
        assert_eq!(
            thread_expired(Lang::Fr),
            "Non envoyée — la suggestion avait expiré."
        );
        assert_eq!(
            thread_expired(Lang::En),
            "Not sent — the suggestion had expired."
        );
    }

    #[test]
    fn thread_refused_is_not_sent_then_the_companions_sentence() {
        let fr = thread_refused(Lang::Fr, "consent_revoked");
        assert_eq!(
            fr,
            format!(
                "Non envoyée — {}",
                crate::refusals::sentence(Lang::Fr, "consent_revoked")
            )
        );
        assert!(
            fr.starts_with("Non envoyée — Vous avez révoqué votre consentement"),
            "{fr}"
        );
        let en = thread_refused(Lang::En, "consent_revoked");
        assert!(
            en.starts_with("Not sent — You have revoked your consent for this contact"),
            "{en}"
        );
        assert!(
            en.ends_with("There is nothing to do here, and nothing was half-done."),
            "{en}"
        );
        // A code this build has never met is still a sentence, and names
        // the code.
        let en = thread_refused(Lang::En, "a_code_from_the_future");
        assert!(en.starts_with("Not sent — "), "{en}");
        assert!(en.contains("(a_code_from_the_future)"), "{en}");
    }

    #[test]
    fn the_refusal_that_went_out_never_begins_with_not_sent() {
        // `approval_published_but_not_recorded`: the reply reached the bus
        // and the Companion Gateway could not write that down. The
        // Companion's sentence opens with "Your reply was sent", and a "Not
        // sent" in front of it would be the first word the owner reads and
        // the one they act on — by sending it twice.
        let fr = thread_refused(Lang::Fr, "approval_published_but_not_recorded");
        assert!(
            fr.starts_with("Envoyée mais non enregistrée — Votre réponse est partie."),
            "{fr}"
        );
        assert!(!fr.contains("Non envoyée"), "{fr}");
        let en = thread_refused(Lang::En, "approval_published_but_not_recorded");
        assert!(
            en.starts_with("Sent but not recorded — Your reply was sent."),
            "{en}"
        );
        assert!(!en.contains("Not sent"), "{en}");
        // And it is the only one that opens that way.
        for code in crate::refusals::known_codes() {
            if code == "approval_published_but_not_recorded" {
                continue;
            }
            assert!(
                thread_refused(Lang::Fr, code).starts_with("Non envoyée — "),
                "{code}"
            );
            assert!(
                thread_refused(Lang::En, code).starts_with("Not sent — "),
                "{code}"
            );
        }
    }

    #[test]
    fn activity_approved_and_refused_locally_name_buzz_and_the_network() {
        assert_eq!(
            activity_approved(Lang::Fr, "whatsapp", true),
            "Suggestion approuvée depuis Buzz (modifiée) · WhatsApp"
        );
        assert_eq!(
            activity_approved(Lang::Fr, "whatsapp", false),
            "Suggestion approuvée depuis Buzz · WhatsApp"
        );
        assert_eq!(
            activity_approved(Lang::En, "signal", true),
            "Suggestion approved from Buzz (edited) · Signal"
        );
        assert_eq!(
            activity_approved(Lang::En, "signal", false),
            "Suggestion approved from Buzz · Signal"
        );
        assert_eq!(
            activity_refused_locally(Lang::Fr, "whatsapp"),
            "Suggestion refusée depuis Buzz (❌, non transmise) · WhatsApp"
        );
        assert_eq!(
            activity_refused_locally(Lang::En, "sms"),
            "Suggestion declined from Buzz (❌, not forwarded) · SMS"
        );
        // A post the loop could not read a network off: no dangling
        // separator.
        assert_eq!(
            activity_approved(Lang::Fr, "", false),
            "Suggestion approuvée depuis Buzz"
        );
        assert_eq!(
            activity_approved(Lang::En, "", true),
            "Suggestion approved from Buzz (edited)"
        );
        assert_eq!(
            activity_refused_locally(Lang::En, ""),
            "Suggestion declined from Buzz (❌, not forwarded)"
        );
    }

    #[test]
    fn the_network_is_read_back_off_the_posts_first_line() {
        let reference = format!("twalk:suggestion:{}", "1".repeat(64));
        for lang in [Lang::Fr, Lang::En] {
            for expires in [Some("2026-09-17T11:00:00Z"), Some("tomorrow"), None] {
                // A body with the separator in it does not confuse the
                // reader: only the first line is read.
                let post = approval_post(
                    lang,
                    "Oui · à 20h",
                    "whatsapp",
                    None,
                    "",
                    expires,
                    &unread(lang),
                    &reference,
                );
                assert_eq!(network_off_post(&post), Some("WhatsApp"), "{post}");
                // …and what is read back is what the activity line names.
                assert_eq!(
                    activity_approved(lang, network_off_post(&post).unwrap(), false),
                    activity_approved(lang, "whatsapp", false)
                );
                let post =
                    approval_post(lang, "Oui", "irc", None, "", expires, &unread(lang), &reference);
                assert_eq!(network_off_post(&post), Some("irc"), "{post}");
            }
        }
        assert_eq!(network_off_post(""), None);
        assert_eq!(network_off_post("Just prose\nand a second line"), None);
        assert_eq!(network_off_post("Proposed reply ·  \n« … »"), None);
    }
}
