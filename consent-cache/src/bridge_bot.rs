//! A bridge's own bot, as the Sensor recognises it on the wire.
//!
//! A bridge materialises two kinds of Matrix account. A **ghost** stands in
//! for a person on the other network, and everything the consent model says
//! applies to it. The **bot** — each bridge's `appservice.bot.username`,
//! `@whatsappbot:…`, `@signalbot:…` — is the appservice's own service
//! identity: it creates portals, puppets ghosts, invites the user, and is a
//! member of every portal room of its network. It is not a person.
//!
//! # What was wrong
//!
//! The Sensor had no notion of it, so a bot reaching a handler was resolved
//! as a **subject**: looked up in the consent cache, labelled with whatever
//! decision applied (a network-wide grant answers `granted` — [ADR 0021]),
//! given a `contact` object with its display name, and published. Measured on
//! the reference deployment right after #147 landed:
//!
//! ```text
//! @whatsappbot   680 presence events        @signalbot   470        the owner   34
//! ```
//!
//! Two service accounts were 1,150 of 1,216 presence events — and still are,
//! two a minute, for as long as the deployment runs. The volume is the least
//! of it: those events travel through the *contact* machinery, so a
//! deployment's consent state can acquire rows about `@whatsappbot`, and a
//! consumer reading the snapshot sees a robot among the people the user has
//! decided about. Same class as #147 (the owner) and #149 (the Gateway's
//! half), with a third non-person.
//!
//! # The set is handed over, never derived
//!
//! Each configured bridge names its bot in `appservice.bot.username` —
//! which is NOT the appservice's `sender_localpart`, a different account
//! that is joined to no rooms (ADR 0026, issue #171) — and the deployment
//! already names those accounts once, in `SENSOR_ALLOWED_INVITERS` — that is
//! how the Sensor accepts a portal invitation at all. This set is a second,
//! separate list (the Sensor's `Config::bridge_bots`), for the
//! same reason [`crate::owner`]'s is: exact matches against what the
//! deployment confirmed, and nothing inferred.
//!
//! Two derivations were considered and rejected, and both rejections are the
//! same rule read twice — **suppressing a person is the failure to avoid**:
//!
//! - *`SENSOR_ALLOWED_INVITERS` minus the operator.* That list answers a
//!   different question ("whose invitation do I honour?") and an operator may
//!   legitimately add a second account of their own to it. Reusing it would
//!   make that person's presence vanish from the bus, silently, as a
//!   side effect of an unrelated setting.
//! - *The portal's own `m.bridge` marker, whose `bridgebot` field names the
//!   bot.* It is room state, so it is attacker-influenced: a member with the
//!   power to send state in a room the Sensor observes could name a contact
//!   as that room's `bridgebot` and delete them from the stream. Requiring
//!   the marker to have been **sent by** the account it names would close
//!   that, but it would rest on a mautrix behaviour Twalk does not control,
//!   and the failure mode of getting it wrong is silent: the bots are simply
//!   never recognised and the defect is still there.
//!
//! There *is* an authoritative source, and it is not the Sensor's to read:
//! each bridge's provisioning API answers its own bot in `GET /whoami` as
//! `bridge_bot` (`sensor/tests/bridges_deployment.rs` asserts exactly that
//! field), and the Companion Gateway already holds every bridge's secret. So
//! the seam a Gateway-served set plugs into later is [`BridgeBots::new`], the
//! same shape [`crate::owner::Owner::replace_identities`] left open for the
//! operator's ghosts. Unlike the owner's set, this one does not change while a
//! deployment runs, so there is nothing to replace in place today.
//!
//! # Unknown is not a bot
//!
//! An identity the deployment has not named stays a contact, and its events
//! are published exactly as before. That is the safe failure here, and it is
//! the *opposite* direction from most of this project: everywhere else doubt
//! goes to restraint, but mistaking a contact for a bot makes a real person
//! vanish from the bus in silence, which no consumer can detect and no
//! operator can diagnose. So the doubt goes to publishing.
//!
//! [ADR 0021]: ../../../docs/architecture/adr/0021-the-owner-is-never-a-contact-on-any-event.md

use std::collections::BTreeSet;
use std::sync::Arc;

/// The Matrix IDs of the bridges' own bots, as the deployment named them.
///
/// Cheap to clone into every event handler. Empty is the ordinary state of a
/// deployment that runs no bridge — and also of one whose operator has not
/// named them, in which case the Sensor behaves exactly as it did before this
/// module existed.
#[derive(Debug, Clone, Default)]
pub struct BridgeBots {
    ids: Arc<BTreeSet<String>>,
}

impl BridgeBots {
    pub fn new<I>(ids: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        Self {
            ids: Arc::new(ids.into_iter().collect()),
        }
    }

    /// Whether this Matrix user ID is a bridge's own bot: an exact match
    /// against the set the deployment handed over, never a prefix, a pattern
    /// or a display name. A ghost of the same bridge is not its bot, and
    /// neither is an account whose localpart merely looks like one.
    pub fn contains(&self, user_id: &str) -> bool {
        self.ids.contains(user_id)
    }

    /// The ids, sorted, for the startup log line an operator reads to check
    /// what their deployment named.
    pub fn ids(&self) -> Vec<String> {
        self.ids.iter().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::BridgeBots;

    const WHATSAPP_BOT: &str = "@whatsappbot:twalk.localhost";
    const SIGNAL_BOT: &str = "@signalbot:twalk.localhost";

    fn reference_deployment() -> BridgeBots {
        BridgeBots::new([WHATSAPP_BOT.to_owned(), SIGNAL_BOT.to_owned()])
    }

    #[test]
    fn every_configured_bridges_bot_is_recognised() {
        // Both, not one: the reference deployment's 1,150 presence events
        // came from two bridges, and a fix that knew about one would have
        // halved a defect rather than closed it.
        let bots = reference_deployment();
        assert!(bots.contains(WHATSAPP_BOT));
        assert!(bots.contains(SIGNAL_BOT));
    }

    #[test]
    fn a_ghost_of_the_same_bridge_is_not_its_bot() {
        // The whole point of the distinction: `@whatsapp_<phone>` is a person
        // the bridge stands in for, `@whatsappbot` is the appservice itself.
        let bots = reference_deployment();
        assert!(!bots.contains("@whatsapp_33612345678:twalk.localhost"));
        assert!(!bots.contains("@whatsapp_lid-115332874281144:twalk.localhost"));
        assert!(!bots.contains("@signal_75af9e9a-b173-4fa0-9228-03d4a03a1e2c:twalk.localhost"));
    }

    #[test]
    fn an_identity_is_matched_whole_and_never_by_prefix() {
        // A contact is free to register `@whatsappbot2` or to hold
        // `@whatsappbot` on another homeserver. Matching by prefix — or
        // ignoring the server name — would delete them from the bus.
        let bots = reference_deployment();
        assert!(!bots.contains("@whatsappbot2:twalk.localhost"));
        assert!(!bots.contains("@whatsappbot:evil.example"));
        assert!(!bots.contains("whatsappbot:twalk.localhost"));
    }

    #[test]
    fn an_unnamed_account_is_a_contact() {
        // Unknown is not a bot. A deployment that named nothing behaves as
        // it did before this module: everything is published, which is the
        // safe failure in this one direction.
        let bots = BridgeBots::default();
        assert!(bots.is_empty());
        assert!(!bots.contains(WHATSAPP_BOT));
        let partial = BridgeBots::new([WHATSAPP_BOT.to_owned()]);
        assert!(!partial.contains(SIGNAL_BOT));
    }

    #[test]
    fn ids_are_listed_sorted_and_deduplicated() {
        let bots = BridgeBots::new([
            SIGNAL_BOT.to_owned(),
            WHATSAPP_BOT.to_owned(),
            SIGNAL_BOT.to_owned(),
        ]);
        assert_eq!(
            bots.ids(),
            vec![SIGNAL_BOT.to_owned(), WHATSAPP_BOT.to_owned()]
        );
    }
}
