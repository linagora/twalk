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
//! Two lines this lot deliberately does not write are named in the post
//! itself: the delivery line and the gesture that decides are #284's, and
//! the post says so rather than leaving a blank the owner would read as a
//! defect.
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
/// than one the clerk could not format.
pub fn approval_post(
    l: Lang,
    body: &str,
    network: &str,
    expires_at: Option<&str>,
    reference: &str,
) -> String {
    let network = network_name(network);
    let expiry = expires_at.map(|at| match (l, clock_utc(at)) {
        (Lang::Fr, Some(clock)) => format!(" · expire à {clock} (heure locale non connue)"),
        (Lang::Fr, None) => format!(" · expire à {at}"),
        (Lang::En, Some(clock)) => format!(" · expires at {clock} (local time not known)"),
        (Lang::En, None) => format!(" · expires at {at}"),
    });
    let expiry = expiry.as_deref().unwrap_or("");
    match l {
        Lang::Fr => format!(
            "Réponse proposée · {network}{expiry}\n\
             « {body} »\n\
             Livraison : pas encore connue — ce sera dit ici quand le greffier saura lire l’état \
             de la conversation (#284).\n\
             ✅ envoyer tel quel · ❌ refuser · répondre ici pour envoyer un autre texte (à venir : \
             #284)\n\
             {reference}"
        ),
        Lang::En => format!(
            "Proposed reply · {network}{expiry}\n\
             “{body}”\n\
             Delivery: not yet known — it will be said here once the clerk can read the state of \
             the conversation (#284).\n\
             ✅ send as is · ❌ decline · reply here to send a different text (coming: #284)\n\
             {reference}"
        ),
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
/// left out when empty.
pub fn journal_line(
    l: Lang,
    network: &str,
    reach: &str,
    posted_as: &str,
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
    match (l, reach) {
        (Lang::Fr, "contact") => format!(
            "Partie · {network} · postée en tant que {posted_as} · a atteint le contact · \
             approbation {approval}{when}"
        ),
        (Lang::Fr, "nobody") => format!(
            "Personne ne l’a reçue · {network} · postée en tant que {posted_as} — un compte que le \
             bridge ignore (#123) · approbation {approval}{when}"
        ),
        (Lang::Fr, other) => format!(
            "Partie · {network} · postée en tant que {posted_as} · portée : {other} · approbation \
             {approval}{when}"
        ),
        (Lang::En, "contact") => format!(
            "Sent · {network} · posted as {posted_as} · reached the contact · approval \
             {approval}{when}"
        ),
        (Lang::En, "nobody") => format!(
            "Nobody received it · {network} · posted as {posted_as} — an account the bridge \
             ignores (#123) · approval {approval}{when}"
        ),
        (Lang::En, other) => format!(
            "Sent · {network} · posted as {posted_as} · reach: {other} · approval {approval}{when}"
        ),
    }
}

/// A bridge's state change. The bridge is named by its id — a deployment's
/// bridge instance, never a network (ADR 0005) — and the state by the
/// contract's word, translated when there is a translation.
pub fn activity_bridge(l: Lang, bridge_id: &str, state: &str) -> String {
    let state = match (l, state) {
        (Lang::Fr, "starting") => "démarre",
        (Lang::Fr, "connected") => "connecté",
        (Lang::Fr, "degraded") => "dégradé",
        (Lang::Fr, "disconnected") => "déconnecté",
        (Lang::Fr, "session_expired") => "session expirée",
        (_, other) => other,
    };
    format!("Bridge {bridge_id} · {state}")
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
            let post = approval_post(l, body, "whatsapp", Some("2026-09-17T11:00:00Z"), REFERENCE);
            assert!(post.contains(body), "{post}");
            assert_eq!(post.lines().last(), Some(REFERENCE), "{post}");
            assert!(post.contains("WhatsApp"), "{post}");
            assert!(post.contains("11:00 UTC"), "{post}");
            assert!(post.contains("#284"), "{post}");
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
            Some("2026-09-17T21:41:00Z"),
            REFERENCE,
        );
        let expected = format!(
            "Réponse proposée · WhatsApp · expire à 21:41 UTC (heure locale non connue)\n\
             « Pas de problème, à 20h ! »\n\
             Livraison : pas encore connue — ce sera dit ici quand le greffier saura lire l’état de la conversation (#284).\n\
             ✅ envoyer tel quel · ❌ refuser · répondre ici pour envoyer un autre texte (à venir : #284)\n\
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
            Some("2026-09-17T21:41:00Z"),
            REFERENCE,
        );
        let expected = format!(
            "Proposed reply · Signal · expires at 21:41 UTC (local time not known)\n\
             “No problem, see you at 8!”\n\
             Delivery: not yet known — it will be said here once the clerk can read the state of the conversation (#284).\n\
             ✅ send as is · ❌ decline · reply here to send a different text (coming: #284)\n\
             {REFERENCE}"
        );
        assert_eq!(post, expected);
    }

    #[test]
    fn a_post_without_an_expiry_says_none_and_an_unreadable_one_is_shown_raw() {
        let post = approval_post(Lang::Fr, "Oui", "sms", None, "twalk:suggestion:abc");
        assert!(post.starts_with("Réponse proposée · SMS\n"), "{post}");
        assert!(!post.contains("expire"), "{post}");

        let post = approval_post(
            Lang::Fr,
            "Oui",
            "sms",
            Some("bientôt"),
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
        let post = approval_post(Lang::Fr, body, "whatsapp", None, REFERENCE);
        assert!(post.contains(body), "{post}");
        assert_eq!(post.lines().last(), Some(REFERENCE));
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
        );
        assert_eq!(
            en,
            "Sent · Signal · posted as @michel:example.com · reached the contact · approval 8d3fb3fe9d2d… · 21:41 UTC"
        );
    }

    #[test]
    fn a_short_approval_id_is_shown_whole_and_an_unreadable_time_raw() {
        let fr = journal_line(Lang::Fr, "sms", "contact", "@m:x", "abc", "now-ish");
        assert!(fr.ends_with("approbation abc · now-ish"), "{fr}");
        let fr = journal_line(Lang::Fr, "sms", "contact", "@m:x", "abc", "");
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
            "Bridge bridge-whatsapp-1 · déconnecté"
        );
        assert_eq!(
            activity_bridge(Lang::Fr, "bridge-whatsapp-1", "connected"),
            "Bridge bridge-whatsapp-1 · connecté"
        );
        assert_eq!(
            activity_bridge(Lang::Fr, "bridge-signal-1", "session_expired"),
            "Bridge bridge-signal-1 · session expirée"
        );
        assert_eq!(
            activity_bridge(Lang::En, "bridge-signal-1", "degraded"),
            "Bridge bridge-signal-1 · degraded"
        );
        assert_eq!(
            activity_bridge(Lang::En, "bridge-gmessages-1", "starting"),
            "Bridge bridge-gmessages-1 · starting"
        );
        assert_eq!(
            activity_bridge(Lang::En, "bridge-gmessages-1", "rebooting"),
            "Bridge bridge-gmessages-1 · rebooting"
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
}
