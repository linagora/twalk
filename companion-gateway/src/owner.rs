//! The owner, as the Gateway recognises them in a consent decision (ticket
//! #149).
//!
//! # Why the Gateway needs this at all
//!
//! `CONTEXT.md` says the owner *"is never a contact and never has a consent
//! state"*. #109 and #147 made the **Sensor** stop producing events that
//! contradict it, and ADR 0021 says in as many words what neither of them
//! could reach: the Sensor can refuse to *use* a consent row about the owner,
//! it cannot remove one from this Gateway's store. Before ADR 0018 the user's
//! own messages were published as a contact's, so they fed the
//! pending-contact projection (#54) and the user was offered a decision about
//! their own ghost — and a deployment upgraded across #109 still holds that
//! row. The Gateway is the single writer of consent state (ADR 0006), so the
//! invariant is only true once the Gateway holds it: it refuses to record such
//! a decision, and no read of its store serves one.
//!
//! That requires the prior question the Gateway had no answer to: **which
//! Matrix IDs are the owner's?**
//!
//! # The set is configured, it is not derived, and it grows
//!
//! On the wire the owner's own traffic does not come from their Matrix ID. A
//! bridge materialises a *network ghost* of the user's own account, and it is
//! indistinguishable in shape from a contact's. Captured live on the reference
//! deployment (#109):
//!
//! ```text
//! @whatsapp_33660469852:twalk.localhost           "Michel-Marie MAUDET (WA)"
//! @whatsapp_lid-115332874281144:twalk.localhost   "Michel-Marie MAUDET (WA)"
//! @signal_75af9e9a-b173-4fa0-9228-03d4a03a1e2c    "Michel-Marie MAUDET"
//! ```
//!
//! Three facts from that evidence, and each of them is load-bearing here
//! exactly as it is in `sensor/src/owner.rs`:
//!
//! - **more than one ghost per network** — WhatsApp answers a phone-number
//!   ghost *and* a LID ghost, and the messages arrived under the LID one. So
//!   the owner is a *set*, per deployment rather than per network;
//! - **it cannot be derived** — not by string-building `@<network>_<login id>`
//!   (the localpart template belongs to each bridge), and not from a bridge's
//!   own provisioning answers either: `GET /whoami` carries a login's `id`,
//!   `name` and `profile` (`tests/harness/fixtures/mautrix-whatsapp/whoami.json`)
//!   and **no ghost Matrix ID at all**. This is the same reason
//!   `GATEWAY_BRIDGE_<ID>_BOT_USER_ID` is configuration rather than built from
//!   a bridge id (#171);
//! - **the set grows after the fact** — a LID started being used
//!   mid-conversation on the reference deployment, so an identity is added to a
//!   running deployment's configuration and not discovered once at install.
//!
//! So the set is handed over: `GATEWAY_OWNER_IDENTITIES`, beside the
//! `GATEWAY_OWNER` it always contains. And because it is handed over, it is
//! **served**: `GET /api/consent/snapshot` carries it
//! ([`crate::consent_snapshot`]), so a consumer that has to apply the same
//! rule reads it from the single writer of consent state instead of keeping a
//! second copy of a list nobody can derive. Two components reading one list is
//! tolerable; two components maintaining their own is not (#149).
//!
//! # Unknown is not the owner
//!
//! An exact match against what the deployment confirmed, never a prefix, a
//! pattern or a display name. A ghost the deployment has not confirmed stays a
//! contact: their decision is recorded, their row is served, and the user
//! decides about them like anybody else. That is the safe failure — the unsafe
//! one is exempting somebody from the consent model on a guess, and matching
//! by display name would let a contact who copies the owner's name out of it.
//! The same reasoning is `Owner::is_owner`'s in the Sensor and
//! `QuotedAuthor::Unknown`'s in `sensor/src/normalize.rs`.
//!
//! # No `replace_identities` here
//!
//! The Sensor's [`Owner`](../../sensor/src/owner.rs) can have its set replaced
//! while it runs, because the Sensor is a *reader* of a set somebody else
//! holds. This one is the holder: the set comes from the process's own
//! environment, so it changes when the operator changes it and the deployment
//! restarts the Gateway. An immutable set is then the honest type — nothing
//! here can be out of date with respect to a source of truth, because it *is*
//! the source.

use std::collections::BTreeSet;

/// The deployment's one owner (ADR 0011) and every Matrix ID their own traffic
/// is observed to arrive under.
///
/// Cheap to clone behind an `Arc` into the consent writer, the store and the
/// pending-contact projection — all three of which are handed the same one, so
/// that "who is not a subject" has exactly one answer in this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    matrix_id: String,
    identities: BTreeSet<String>,
}

impl Owner {
    /// The owner's Matrix ID, plus the network ghosts the deployment has
    /// confirmed as theirs.
    ///
    /// The Matrix ID is always one of the identities, and it is the reason
    /// this refusal works on a deployment that has configured no ghost at all:
    /// on a native Matrix conversation (ADR 0009) the user writes from their
    /// own account, and a decision about `GATEWAY_OWNER` as a *contact* is the
    /// one an operator or a browser can reach today (the Companion's consent
    /// screen labels exactly that row, #170).
    ///
    /// Blank entries are dropped rather than stored: an empty string would
    /// match nothing and a list with a trailing comma is an ordinary way to
    /// write one.
    pub fn new<I>(matrix_id: impl Into<String>, ghosts: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let matrix_id = matrix_id.into();
        let mut identities: BTreeSet<String> = ghosts
            .into_iter()
            .map(|ghost| ghost.trim().to_owned())
            .filter(|ghost| !ghost.is_empty())
            .collect();
        identities.insert(matrix_id.clone());
        Self {
            matrix_id,
            identities,
        }
    }

    /// The owner's own Matrix ID — the account, never a ghost: what a decision
    /// is attributed to as its `actor`.
    pub fn matrix_id(&self) -> &str {
        &self.matrix_id
    }

    /// Whether this Matrix user ID is one of the owner's.
    ///
    /// A confirmed identity and nothing else: an exact match against the set
    /// the deployment handed over.
    pub fn is_owner(&self, user_id: &str) -> bool {
        self.identities.contains(user_id)
    }

    /// The identities, sorted: the startup log line an operator reads to check
    /// what their deployment confirmed, and the list
    /// `GET /api/consent/snapshot` serves.
    pub fn identities(&self) -> Vec<String> {
        self.identities.iter().cloned().collect()
    }

    /// The identities, as SQL parameters for the exclusions in
    /// [`crate::store`]. Borrowed rather than cloned, because every read of
    /// the consent store binds them.
    pub(crate) fn as_sql_params(&self) -> Vec<&str> {
        self.identities.iter().map(String::as_str).collect()
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
    fn several_ghosts_on_one_network_are_all_the_owner() {
        // The live evidence on #109: WhatsApp answers a phone-number ghost and
        // a LID ghost, and the traffic arrived under the LID one. A Gateway
        // that recognised one and not the other would keep serving a consent
        // row about the identity that actually writes.
        let owner = reference_deployment();
        assert!(owner.is_owner(WHATSAPP_PHONE_GHOST));
        assert!(owner.is_owner(WHATSAPP_LID_GHOST));
        assert!(owner.is_owner(SIGNAL_GHOST));
    }

    #[test]
    fn the_owners_own_matrix_id_is_always_an_identity() {
        // The property that makes the refusal reachable on a deployment that
        // has confirmed no ghost at all — and the row the Companion's consent
        // screen can recognise in a browser (#170).
        let owner = Owner::new(OWNER, []);
        assert!(owner.is_owner(OWNER));
        assert_eq!(owner.matrix_id(), OWNER);
        assert_eq!(owner.identities(), vec![OWNER.to_owned()]);
    }

    #[test]
    fn an_unconfirmed_ghost_is_a_contact() {
        // Unknown is not the owner. A ghost of the same shape, on the same
        // network, one digit apart, stays a subject: their consent row is
        // recorded and served like anybody else's, because exempting somebody
        // from the consent model on a guess is the unsafe failure.
        let owner = reference_deployment();
        assert!(!owner.is_owner("@whatsapp_lid-115332874281145:twalk.localhost"));
        assert!(!owner.is_owner("@whatsapp_33612345678:twalk.localhost"));
        assert!(!owner.is_owner("@signal_f6c1c57b-4268-4dea-a325-4a0565e94ee5:twalk.localhost"));
    }

    #[test]
    fn an_identity_is_matched_whole_and_never_by_prefix() {
        // The owner's ghost localparts are prefixes of other valid localparts
        // (a LID is a bare number, and a longer one starts with it), so
        // matching by prefix would exempt a contact from the consent model.
        let owner = Owner::new(OWNER, ["@whatsapp_lid-1153:twalk.localhost".to_owned()]);
        assert!(!owner.is_owner("@whatsapp_lid-11533:twalk.localhost"));
        assert!(!owner.is_owner("@whatsapp_lid-1153:other.localhost"));
        assert!(owner.is_owner("@whatsapp_lid-1153:twalk.localhost"));
    }

    #[test]
    fn the_set_is_sorted_deduplicated_and_free_of_blanks() {
        let owner = Owner::new(
            OWNER,
            [
                SIGNAL_GHOST.to_owned(),
                WHATSAPP_PHONE_GHOST.to_owned(),
                SIGNAL_GHOST.to_owned(),
                OWNER.to_owned(),
                // A trailing comma in the operator's variable, and a value
                // padded for readability.
                String::new(),
                format!("  {WHATSAPP_LID_GHOST}  "),
            ],
        );
        assert_eq!(
            owner.identities(),
            vec![
                OWNER.to_owned(),
                SIGNAL_GHOST.to_owned(),
                WHATSAPP_PHONE_GHOST.to_owned(),
                WHATSAPP_LID_GHOST.to_owned(),
            ]
        );
        assert!(owner.is_owner(WHATSAPP_LID_GHOST), "a padded value is read");
        assert!(!owner.is_owner(""), "and a blank one matches nothing");
    }
}
