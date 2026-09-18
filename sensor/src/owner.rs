//! The operator, as the Sensor recognises them on the wire.
//!
//! A portal room mirrors a conversation in both directions, so the Sensor
//! observes the user's own messages beside their contacts'. It had no notion
//! of the operator, so those messages went out through the inbound door with
//! the user as the subject and `consent: pending` — the deployment holding
//! that its own owner had never decided about themselves, and a persona one
//! consent grant away from suggesting a reply to a note the user wrote to
//! themselves. ADR 0018 settles the shape of the fix: the user's own messages
//! are their own event type and the user is not a contact.
//!
//! What this module answers is the prior question: **which sender is the
//! operator?**
//!
//! # The operator is a set of identities, and the set is given, not derived
//!
//! On the wire the operator's own messages do not come from their Matrix ID.
//! mautrix materialises a *network ghost* of the user's own account, and it
//! is indistinguishable in shape from a contact's ghost: same localpart
//! prefix, same server, a display name that is just a name. Captured live on
//! the reference deployment:
//!
//! ```text
//! @whatsapp_33660469852:twalk.localhost           "Michel-Marie MAUDET (WA)"
//! @whatsapp_lid-115332874281144:twalk.localhost   "Michel-Marie MAUDET (WA)"
//! @signal_75af9e9a-b173-4fa0-9228-03d4a03a1e2c    "Michel-Marie MAUDET"
//! ```
//!
//! Three properties of that evidence shape everything here.
//!
//! **There is more than one ghost per network.** WhatsApp answers both a
//! phone-number ghost and a LID ghost (WhatsApp's newer privacy-preserving
//! identifier), and the message that actually reached the bus came from the
//! LID one. So the operator is a *set*, held per deployment and not per
//! network, and a set that may grow: a LID can start being used
//! mid-conversation.
//!
//! **It cannot be derived.** Not by string-building `@<network>_<login id>`
//! in the Sensor — the localpart template belongs to each bridge, so that
//! would hard-code mautrix's naming for every network Twalk ever adds — and
//! not from the bridge's own provisioning answers either: `GET /whoami`
//! carries a login's `id`, `name` and `profile` (see
//! `companion-gateway/tests/harness/fixtures/mautrix-whatsapp/whoami.json`)
//! and no ghost Matrix ID at all, so deriving from the login id finds the
//! phone-number ghost and misses the LID ghost the messages arrive under.
//! The set is therefore **confirmed by the deployment and handed to the
//! Sensor**, through [`Config::owner_identities`](crate::config::Config).
//!
//! **The display name is a hint, never an identity.** Both WhatsApp ghosts
//! above carry the same display name, and a contact may set any display name
//! they like: matching the operator by name would let a contact impersonate
//! them into an exemption from the consent model. Nothing here reads a
//! display name.
//!
//! # Unknown is not the operator
//!
//! An identity the deployment has not confirmed stays a contact, which is the
//! behaviour that existed before this module: their message is published as
//! `inbound.message.received`, labelled with the user's decision about them,
//! and a persona is gated on it as usual. That is the safe failure — the same
//! reasoning as `QuotedAuthor::Unknown` in [`crate::normalize`], where unknown
//! is not consent. The unsafe failure would be the other one: exempting a
//! sender from the consent model on a guess.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

/// The deployment's one owner (ADR 0011) and every Matrix ID their messages
/// are observed to arrive under.
///
/// Cheap to clone into every event handler: the identities are shared behind
/// an `Arc`, and [`Owner::replace_identities`] is the one way they change —
/// the seam a set served by the Companion Gateway plugs into later, so that a
/// ghost appearing mid-conversation does not need a restart. Today the set is
/// loaded once from the environment, which is the documented limitation.
#[derive(Debug, Clone)]
pub struct Owner {
    matrix_id: Arc<str>,
    identities: Arc<RwLock<BTreeSet<String>>>,
}

impl Owner {
    /// The operator's Matrix ID, plus the network ghosts the deployment has
    /// confirmed as theirs.
    ///
    /// The Matrix ID is always one of the identities: on a native Matrix
    /// conversation (ADR 0009) the user writes from their own account, and
    /// with double puppeting configured a bridge relays their messages under
    /// it too. Neither is the reference deployment's configuration, but both
    /// are somebody's, and neither should have to be listed twice.
    pub fn new<I>(matrix_id: impl Into<String>, ghosts: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let matrix_id = matrix_id.into();
        let mut identities: BTreeSet<String> = ghosts.into_iter().collect();
        identities.insert(matrix_id.clone());
        Self {
            matrix_id: Arc::from(matrix_id.as_str()),
            identities: Arc::new(RwLock::new(identities)),
        }
    }

    /// The operator's Matrix ID: the `subject` of every
    /// `outbound.message.sent` event, whichever identity the message arrived
    /// under. One operator, not one per network and not one per ghost.
    pub fn matrix_id(&self) -> &str {
        &self.matrix_id
    }

    /// Whether this Matrix user ID is the operator. A confirmed identity and
    /// nothing else: an exact match against the set the deployment handed
    /// over, never a prefix, a pattern or a display name.
    pub fn is_owner(&self, user_id: &str) -> bool {
        self.read().contains(user_id)
    }

    /// The identities, sorted, for the startup log line an operator reads to
    /// check what their deployment confirmed.
    pub fn identities(&self) -> Vec<String> {
        self.read().iter().cloned().collect()
    }

    /// Replaces the confirmed ghosts with a freshly resolved set, keeping the
    /// operator's Matrix ID. Exists so that a later source of truth — the
    /// Gateway resolving the set from the bridges and serving it — can be
    /// applied to a running Sensor, since a new ghost can appear at any time.
    pub fn replace_identities<I>(&self, ghosts: I)
    where
        I: IntoIterator<Item = String>,
    {
        let mut identities: BTreeSet<String> = ghosts.into_iter().collect();
        identities.insert(self.matrix_id.to_string());
        *self.write() = identities;
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeSet<String>> {
        self.identities
            .read()
            .expect("the owner identity lock is poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, BTreeSet<String>> {
        self.identities
            .write()
            .expect("the owner identity lock is poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::Owner;

    const OWNER: &str = "@michel:twalk.localhost";
    const WHATSAPP_PHONE_GHOST: &str = "@whatsapp_33660469852:twalk.localhost";
    const WHATSAPP_LID_GHOST: &str = "@whatsapp_lid-115332874281144:twalk.localhost";
    const SIGNAL_GHOST: &str = "@signal_75af9e9a-b173-4fa0-9228-03d4a03a1e2c:twalk.localhost";

    fn reference_deployment() -> Owner {
        Owner::new(
            OWNER,
            [
                WHATSAPP_PHONE_GHOST.to_owned(),
                WHATSAPP_LID_GHOST.to_owned(),
                SIGNAL_GHOST.to_owned(),
            ],
        )
    }

    #[test]
    fn several_ghosts_on_one_network_are_all_the_operator() {
        // The live evidence on #109: WhatsApp answers a phone-number ghost
        // and a LID ghost, and the message that reached the bus came from
        // the LID one. Recognising one and not the other would ship a fix
        // that misses the identity the operator's messages arrive under.
        let owner = reference_deployment();
        assert!(owner.is_owner(WHATSAPP_PHONE_GHOST));
        assert!(owner.is_owner(WHATSAPP_LID_GHOST));
        assert!(owner.is_owner(SIGNAL_GHOST));
    }

    #[test]
    fn the_operators_own_matrix_id_is_always_an_identity() {
        let owner = Owner::new(OWNER, []);
        assert!(owner.is_owner(OWNER));
        assert_eq!(owner.matrix_id(), OWNER);
    }

    #[test]
    fn an_unconfirmed_ghost_is_a_contact() {
        // Unknown is not the operator. A ghost of the same shape, on the
        // same network, one digit apart, stays a contact — it goes through
        // the consent model like anybody else rather than being exempted
        // from it on a guess.
        let owner = reference_deployment();
        assert!(!owner.is_owner("@whatsapp_lid-115332874281145:twalk.localhost"));
        assert!(!owner.is_owner("@whatsapp_33612345678:twalk.localhost"));
        assert!(!owner.is_owner("@signal_f6c1c57b-4268-4dea-a325-4a0565e94ee5:twalk.localhost"));
    }

    #[test]
    fn an_identity_is_matched_whole_and_never_by_prefix() {
        // The operator's ghost localparts are prefixes of other valid
        // localparts (a LID is a bare number, and a longer one starts with
        // it). Matching by prefix would exempt a contact.
        let owner = Owner::new(OWNER, ["@whatsapp_lid-1153:twalk.localhost".to_owned()]);
        assert!(!owner.is_owner("@whatsapp_lid-11533:twalk.localhost"));
        assert!(!owner.is_owner("@whatsapp_lid-1153:other.localhost"));
        assert!(owner.is_owner("@whatsapp_lid-1153:twalk.localhost"));
    }

    #[test]
    fn a_ghost_that_appears_later_is_recognised_without_a_restart() {
        // A LID can start being used mid-conversation, so the set has to be
        // replaceable in place; the handlers hold a clone and must see it.
        let owner = Owner::new(OWNER, [WHATSAPP_PHONE_GHOST.to_owned()]);
        let held_by_a_handler = owner.clone();
        assert!(!held_by_a_handler.is_owner(WHATSAPP_LID_GHOST));

        owner.replace_identities([
            WHATSAPP_PHONE_GHOST.to_owned(),
            WHATSAPP_LID_GHOST.to_owned(),
        ]);

        assert!(held_by_a_handler.is_owner(WHATSAPP_LID_GHOST));
        assert!(
            held_by_a_handler.is_owner(OWNER),
            "a replacement never drops the operator's own Matrix ID"
        );
    }

    #[test]
    fn identities_are_listed_sorted_and_deduplicated() {
        let owner = Owner::new(
            OWNER,
            [
                SIGNAL_GHOST.to_owned(),
                WHATSAPP_PHONE_GHOST.to_owned(),
                SIGNAL_GHOST.to_owned(),
                OWNER.to_owned(),
            ],
        );
        assert_eq!(
            owner.identities(),
            vec![
                OWNER.to_owned(),
                SIGNAL_GHOST.to_owned(),
                WHATSAPP_PHONE_GHOST.to_owned(),
            ]
        );
    }
}
