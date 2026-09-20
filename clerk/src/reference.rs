//! The reference line: the last line of every `approbations` post, and the
//! only memory the clerk has (ADR 0035).
//!
//! `twalk:suggestion:<id>` or `twalk:suggestion:<id> expires <rfc3339>`.
//! The clerk finds out whether it already posted a suggestion by reading
//! this line off its own posts, and the sweep decides what to delete from
//! the expiry it carries — so the relay is the store and nothing here
//! needs one. The line is visible rather than hidden in a tag: what the
//! clerk reads to find its way, the owner reads too (ticket #265).
//!
//! Two rulings are the ones to know. [`parse`] reads the **last** line that
//! starts with [`PREFIX`], because the body above it is a suggestion's own
//! words and could contain anything, including that prefix. And
//! [`has_expired`] answers `false` for an expiry it cannot read: an
//! unparsable timestamp never deletes, because the safe failure of a sweep
//! is to leave a post standing, not to delete one it does not understand.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// What every reference line starts with.
pub const PREFIX: &str = "twalk:suggestion:";

/// What a reference line says: which suggestion the post is about, and
/// when it expires, as the event carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The suggestion's deterministic id: 64 lowercase hex characters.
    pub suggestion_id: String,
    /// The suggestion's `expires_at`, RFC 3339, exactly as the event
    /// carried it; `None` when the event carried none.
    pub expires_at: Option<String>,
}

/// Formats a reference line.
pub fn line(r: &Reference) -> String {
    match &r.expires_at {
        Some(expires_at) => format!("{PREFIX}{} expires {expires_at}", r.suggestion_id),
        None => format!("{PREFIX}{}", r.suggestion_id),
    }
}

/// Reads the last reference line of a post's content, or `None` when the
/// content has no line starting with [`PREFIX`] whose id is 64 hex.
///
/// The last one, not the first: everything above the reference line is a
/// suggestion's own words, which may say anything. And the last one only —
/// a last line that carries the prefix and no readable id is `None`, not a
/// reason to read an earlier line, because the clerk wrote the reference
/// last and a post whose last reference does not parse is not one it
/// recognises.
pub fn parse(content: &str) -> Option<Reference> {
    let rest = content
        .lines()
        .rev()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(PREFIX))?;
    let mut words = rest.split_whitespace();
    let suggestion_id = words.next().filter(|id| is_suggestion_id(id))?;
    let expires_at = match words.next() {
        Some("expires") => words.next().map(str::to_owned),
        _ => None,
    };
    Some(Reference {
        suggestion_id: suggestion_id.to_owned(),
        expires_at,
    })
}

/// The contract's deterministic id: `^[a-f0-9]{64}$`.
fn is_suggestion_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether an RFC 3339 expiry has passed at `now_unix`. `false` when the
/// expiry cannot be read: an unparsable expiry never deletes.
pub fn has_expired(expires_at: &str, now_unix: i64) -> bool {
    match OffsetDateTime::parse(expires_at, &Rfc3339) {
        Ok(expires) => expires.unix_timestamp() <= now_unix,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "319be8ff15d5dee005c8aa27119b983da8223959987e5dbc639d81e370b5ef9b";

    #[test]
    fn the_reference_line_round_trips() {
        let with = Reference {
            suggestion_id: ID.to_owned(),
            expires_at: Some("2026-09-17T11:00:00Z".to_owned()),
        };
        assert_eq!(
            line(&with),
            format!("twalk:suggestion:{ID} expires 2026-09-17T11:00:00Z")
        );
        assert_eq!(parse(&line(&with)), Some(with.clone()));

        let without = Reference {
            suggestion_id: ID.to_owned(),
            expires_at: None,
        };
        assert_eq!(line(&without), format!("twalk:suggestion:{ID}"));
        assert_eq!(parse(&line(&without)), Some(without.clone()));
    }

    #[test]
    fn parse_takes_the_last_reference_line_and_ignores_prose() {
        let other = "0000000000000000000000000000000000000000000000000000000000000000";
        let content = format!(
            "Réponse proposée · WhatsApp\n\
             « Regarde twalk:suggestion:{other} dans ton mail »\n\
             twalk:suggestion:{other}\n\
             ✅ envoyer tel quel\n\
             twalk:suggestion:{ID} expires 2026-09-17T11:00:00Z\n"
        );
        let parsed = parse(&content).unwrap();
        assert_eq!(parsed.suggestion_id, ID);
        assert_eq!(parsed.expires_at.as_deref(), Some("2026-09-17T11:00:00Z"));

        assert_eq!(parse("Just prose, no reference at all."), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn a_reference_needs_a_64_hex_id() {
        assert_eq!(parse("twalk:suggestion:1a5dfe3b"), None);
        assert_eq!(parse("twalk:suggestion:"), None);
        let upper = ID.to_uppercase();
        assert_eq!(parse(&format!("twalk:suggestion:{upper}")), None);
        let not_hex = format!("{}zz", &ID[..62]);
        assert_eq!(parse(&format!("twalk:suggestion:{not_hex}")), None);
    }

    #[test]
    fn an_unparsable_expiry_never_expires() {
        let far_future = i64::MAX / 2;
        assert!(!has_expired("", far_future));
        assert!(!has_expired("tomorrow", far_future));
        assert!(!has_expired("2026-09-17 11:00:00", far_future));
        assert!(!has_expired("1758106800", far_future));
    }

    #[test]
    fn an_expiry_is_compared_against_now() {
        // 2026-09-17T11:00:00Z is 1789642800 seconds after the epoch.
        let at = 1789642800;
        assert!(!has_expired("2026-09-17T11:00:00Z", at - 1));
        assert!(has_expired("2026-09-17T11:00:00Z", at));
        assert!(has_expired("2026-09-17T11:00:00Z", at + 1));
        // An offset is a different instant from the same digits with a Z:
        // 11:00+02:00 is 09:00Z, two hours earlier.
        assert!(!has_expired("2026-09-17T11:00:00+02:00", at - 7201));
        assert!(has_expired("2026-09-17T11:00:00+02:00", at - 7200));
        assert!(has_expired("2026-09-17T11:00:00+02:00", at - 1));
    }
}
