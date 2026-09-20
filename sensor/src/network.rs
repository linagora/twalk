//! Network attribution: which external messaging network an observed event
//! comes from. A network is never a transport (ADR 0005): SMS is `sms`
//! whether it transits through mautrix-gmessages or the SMS Companion.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Network {
    Whatsapp,
    Telegram,
    Signal,
    Discord,
    Sms,
    /// Native Matrix traffic: the user's own account, reached without a
    /// bridge in front of it (ADR 0009).
    Matrix,
    /// A mailbox (ADR 0033): a mail is a message, whatever transport carries
    /// it. Never attributed here — no bridge produces it and no ghost is
    /// named after it — but a consent decision about a sender's address
    /// arrives on the snapshot and the stream like any other, and a value
    /// this enum could not parse would be a decision the Sensor silently
    /// dropped (#268).
    Email,
}

impl Network {
    /// Every value, in the contract's order (`definitions/network.schema.json`,
    /// the one authority — the conformance test below reads it).
    pub const ALL: [Network; 7] = [
        Network::Whatsapp,
        Network::Telegram,
        Network::Signal,
        Network::Discord,
        Network::Sms,
        Network::Matrix,
        Network::Email,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Whatsapp => "whatsapp",
            Self::Telegram => "telegram",
            Self::Signal => "signal",
            Self::Discord => "discord",
            Self::Sms => "sms",
            Self::Matrix => "matrix",
            Self::Email => "email",
        }
    }

    /// Maps a bridge-reported protocol id (from an `m.bridge` state event, or
    /// a ghost prefix) to a contract network. Transport ids like `gmessages`
    /// fold into their user-facing network. `matrix` is deliberately absent:
    /// it is what a room with no bridge in front of it resolves to, never
    /// something a bridge reports about itself.
    pub fn from_bridge_id(id: &str) -> Option<Self> {
        match id {
            "whatsapp" => Some(Self::Whatsapp),
            "telegram" => Some(Self::Telegram),
            "signal" => Some(Self::Signal),
            "discord" => Some(Self::Discord),
            "sms" => Some(Self::Sms),
            // mautrix-gmessages refines its protocol id per conversation.
            "gmessages" | "gmessages-sms" | "gmessages-rcs" => Some(Self::Sms),
            // mautrix-discord (legacy bridge) reports its software id.
            "discordgo" => Some(Self::Discord),
            _ => None,
        }
    }

    /// The mautrix ghost naming convention: `@<network>_<id>:<server>`.
    pub fn from_ghost_localpart(localpart: &str) -> Option<Self> {
        let (prefix, _) = localpart.split_once('_')?;
        Self::from_bridge_id(prefix)
    }

    /// Parses a contract `network` enum value — the exact inverse of
    /// `as_str`, with no transport folding (ADR 0005): unlike
    /// `from_bridge_id`, `gmessages` is NOT a network and is rejected here.
    /// Use this for anything that comes off the bus; use `from_bridge_id`
    /// only for bridge-side identifiers.
    pub fn from_contract_value(value: &str) -> Option<Self> {
        match value {
            "whatsapp" => Some(Self::Whatsapp),
            "telegram" => Some(Self::Telegram),
            "signal" => Some(Self::Signal),
            "discord" => Some(Self::Discord),
            "sms" => Some(Self::Sms),
            "matrix" => Some(Self::Matrix),
            "email" => Some(Self::Email),
            _ => None,
        }
    }
}

/// Resolves the network of an observed event: the room's `m.bridge` state
/// event contents (the mautrix mechanism) win — the first, in the given
/// order, whose `protocol.id` names a known network; the ghost naming
/// convention is the next fallback; and a room that no bridge marked at all,
/// whose sender carries no ghost prefix, is native Matrix traffic —
/// `Network::Matrix`, the user's own account with no bridge in front of it
/// (ADR 0009).
///
/// `None` is what is left: a room some bridge *did* mark, whose markers name
/// no network this version knows and whose sender is not a ghost. That is an
/// unsupported bridge's portal, not native traffic, and the caller skips it
/// rather than mislabelling bridged traffic as Matrix.
///
/// Only `protocol.id` identifies the bridged network: in mautrix the
/// `network` section describes a parent portal (a Discord guild, a Telegram
/// forum) and never carries a network name.
pub fn resolve(bridge_contents: &[Value], sender_localpart: &str) -> Option<Network> {
    let bridged = bridge_contents
        .iter()
        .filter_map(|content| content.pointer("/protocol/id").and_then(Value::as_str))
        .find_map(Network::from_bridge_id)
        .or_else(|| Network::from_ghost_localpart(sender_localpart));
    match bridged {
        Some(network) => Some(network),
        None if bridge_contents.is_empty() => Some(Network::Matrix),
        None => None,
    }
}

/// Which network a **subject** is on, as opposed to which network a **room's
/// traffic** belongs to ([`resolve`]).
///
/// The distinction is issue #150. A message or a reaction happens *in* a room,
/// so the room answers for it and there is nothing else to ask. Presence in
/// Matrix is not room-scoped: it is a fact about an account, and it arrives
/// with no room at all. The handler used to pick the lexicographically first
/// joined room the subject shared with the Sensor that resolved to a bridged
/// network — so the answer came from a **sort over room ids**, which is not a
/// property of the subject.
///
/// For a ghost that was harmless: a bridge materialises only its own ghosts and
/// puts them only in its own portals, so every shared room gives the same
/// answer and the sort chooses nothing. For a **native Matrix contact**
/// (ADR 0009's bring-your-own-account path) it was not: their own rooms carry
/// no `m.bridge` state, but if they are a member of a bridged portal —
/// invited by hand, or a bridge in relay mode — that portal's `protocol.id`
/// answered instead, and their presence was published as `whatsapp` on the
/// strength of an ordering. Consent is looked up by `(subject, network)`, so
/// that is the consent model reading the wrong row: a decision the user made
/// about that person **on Matrix** does not govern an event labelled
/// `whatsapp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectNetwork {
    /// The subject is attributable to exactly one network, by something that
    /// identifies *them* rather than by an ordering over rooms.
    One(Network),
    /// Nothing attributes the subject to a single network: they are a member
    /// of portals of several networks and are a ghost of none of them. The
    /// caller publishes nothing — a guessed network reads the wrong consent
    /// row — and says which networks it saw.
    Ambiguous(Vec<Network>),
    /// No shared room attributes the subject at all: they share no observed
    /// room, or only portals of a bridge this version does not support.
    Unattributable,
}

/// Attributes a subject to a network from their own identity first, and from
/// the rooms they share with the Sensor only when their identity says nothing.
///
/// `shared_room_networks` is [`resolve`]'s answer for each joined room the
/// subject is also joined to, in any order — rooms that resolve to nothing are
/// simply left out. The order is deliberately irrelevant: that is the defect.
///
/// The precedence, and why each step is a property of the subject:
///
/// 1. **A ghost of a known network.** The localpart
///    `@<network>_<id>` *is* the subject's identifier on that network — the
///    same string [`ghost_network_identifier`] mints the contact's phone
///    number from. Nothing about which rooms they are in can improve on it.
/// 2. **A member of a room no bridge marked.** A bridge puts only its own
///    ghosts in only its own portals, so a subject sharing an *unbridged* room
///    with the Sensor is a Matrix account in its own right; `matrix` is the
///    network they are on (ADR 0009), and their membership of somebody's
///    portal is an invitation rather than an identity on that network. This
///    inverts [`resolve`]'s precedence on purpose: for a room's traffic a
///    portal wins over the native fallback, and for a *subject* it must not.
/// 3. **One network across every portal they share.** A puppet whose bridge
///    does not use a `<network>_` localpart template (the `username_template`
///    is each bridge's own) is attributed by the portals that hold it — and
///    since a bridge only fills its own portals, they agree.
/// 4. **Otherwise, nothing.** Portals of two networks holding a subject that
///    is a ghost of neither: the Sensor cannot say, and says so.
pub fn subject_network(localpart: &str, shared_room_networks: &[Network]) -> SubjectNetwork {
    if let Some(network) = Network::from_ghost_localpart(localpart) {
        return SubjectNetwork::One(network);
    }
    if shared_room_networks.contains(&Network::Matrix) {
        return SubjectNetwork::One(Network::Matrix);
    }
    let mut distinct: Vec<Network> = Vec::new();
    for network in shared_room_networks {
        if !distinct.contains(network) {
            distinct.push(*network);
        }
    }
    match distinct.len() {
        0 => SubjectNetwork::Unattributable,
        1 => SubjectNetwork::One(distinct[0]),
        _ => {
            distinct.sort_unstable_by_key(|network| network.as_str());
            SubjectNetwork::Ambiguous(distinct)
        }
    }
}

/// The contact's native network identifier, derived from a ghost localpart
/// `@<network>_<id>:<server>` whose prefix matches the event's network.
/// On phone-based networks (WhatsApp, SMS) mautrix mints the ghost from
/// the phone number with its leading `+` stripped, so an all-digit id gets
/// it restored (`whatsapp_33612345678` → `+33612345678`). Every other id
/// passes through unchanged — in particular numeric Telegram and Discord
/// user ids (`telegram_123456789` → `123456789`), which are not phone
/// numbers, and Signal ids, which mautrix-signal mints from the account's
/// ACI UUID rather than its phone number. `None` when the localpart is not
/// a ghost of this network — a plain Matrix user in a portal room has no
/// derivable identifier, and neither has a native Matrix contact: on that
/// network the Matrix user id is the identifier, and it is already the
/// event's `subject`.
pub fn ghost_network_identifier(network: Network, localpart: &str) -> Option<String> {
    let (prefix, identifier) = localpart.split_once('_')?;
    if Network::from_bridge_id(prefix) != Some(network) || identifier.is_empty() {
        return None;
    }
    let phone_based = matches!(network, Network::Whatsapp | Network::Sms);
    if phone_based && identifier.bytes().all(|byte| byte.is_ascii_digit()) {
        Some(format!("+{identifier}"))
    } else {
        Some(identifier.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The contract is the one authority for the network values (ADR 0033,
    /// #268): this enum is a copy, and a copy is tested against what it
    /// copies. A value added to `definitions/network.schema.json` fails here
    /// until the enum knows it; a variant added here fails until the contract
    /// does. Order included — `ALL` is the contract's order.
    #[test]
    fn the_contract_is_the_authority_for_the_network_values() {
        let authority = twalk_test_harness::contract_definition_values("network")
            .expect("the contract's network definition");
        let copy: Vec<&str> = Network::ALL.iter().map(Network::as_str).collect();
        assert_eq!(copy, authority, "sensor/src/network.rs disagrees with the contract");
        for value in &authority {
            assert_eq!(
                Network::from_contract_value(value).map(|network| network.as_str()),
                Some(value.as_str()),
                "{value} is in the contract and not parsed here"
            );
        }
    }

    #[test]
    fn bridge_ids_map_to_contract_networks() {
        assert_eq!(Network::from_bridge_id("whatsapp"), Some(Network::Whatsapp));
        assert_eq!(Network::from_bridge_id("telegram"), Some(Network::Telegram));
        assert_eq!(Network::from_bridge_id("signal"), Some(Network::Signal));
        assert_eq!(Network::from_bridge_id("discord"), Some(Network::Discord));
        assert_eq!(Network::from_bridge_id("sms"), Some(Network::Sms));
    }

    #[test]
    fn transports_fold_into_their_network() {
        assert_eq!(Network::from_bridge_id("gmessages"), Some(Network::Sms));
    }

    #[test]
    fn contract_values_reject_transports() {
        assert_eq!(
            Network::from_contract_value("whatsapp"),
            Some(Network::Whatsapp)
        );
        assert_eq!(Network::from_contract_value("sms"), Some(Network::Sms));
        assert_eq!(Network::from_contract_value("gmessages"), None);
        assert_eq!(Network::from_contract_value("irc"), None);
    }

    #[test]
    fn matrix_is_a_contract_network_but_never_a_bridge_id() {
        // The outbound and consent paths read the contract value (#18): a
        // decision or an approved reply on `matrix` must parse.
        assert_eq!(
            Network::from_contract_value("matrix"),
            Some(Network::Matrix)
        );
        assert_eq!(Network::Matrix.as_str(), "matrix");
        // No bridge reports `matrix` about itself, and `@matrix_x:server` is
        // a plain user, not a ghost.
        assert_eq!(Network::from_bridge_id("matrix"), None);
        assert_eq!(Network::from_ghost_localpart("matrix_alice"), None);
    }

    #[test]
    fn unknown_bridge_ids_yield_no_network() {
        assert_eq!(Network::from_bridge_id("irc"), None);
    }

    #[test]
    fn ghost_localparts_carry_the_network_prefix() {
        assert_eq!(
            Network::from_ghost_localpart("whatsapp_33612345678"),
            Some(Network::Whatsapp)
        );
        assert_eq!(
            Network::from_ghost_localpart("gmessages_33612345678"),
            Some(Network::Sms)
        );
    }

    #[test]
    fn plain_users_have_no_ghost_prefix() {
        assert_eq!(Network::from_ghost_localpart("bot_alpha"), None);
        assert_eq!(Network::from_ghost_localpart("sensor"), None);
    }

    #[test]
    fn bridge_state_wins_over_the_ghost_prefix() {
        let content = json!({ "protocol": { "id": "signal" }, "channel": { "id": "abc" } });
        assert_eq!(
            resolve(&[content], "whatsapp_33612345678"),
            Some(Network::Signal)
        );
    }

    #[test]
    fn ghost_prefix_is_the_fallback() {
        assert_eq!(
            resolve(&[], "whatsapp_33612345678"),
            Some(Network::Whatsapp)
        );
    }

    #[test]
    fn bridge_transports_fold_through_resolve() {
        let content = json!({ "protocol": { "id": "gmessages" } });
        assert_eq!(resolve(&[content], "bot_alpha"), Some(Network::Sms));
    }

    #[test]
    fn bridge_software_protocol_ids_map_to_their_network() {
        // mautrix-gmessages refines the id per conversation type;
        // mautrix-discord (legacy bridge) reports `discordgo`.
        for (id, network) in [
            ("gmessages-sms", Network::Sms),
            ("gmessages-rcs", Network::Sms),
            ("discordgo", Network::Discord),
        ] {
            let content = json!({ "protocol": { "id": id } });
            assert_eq!(resolve(&[content], "bot_alpha"), Some(network), "{id}");
        }
    }

    #[test]
    fn the_network_section_is_a_parent_portal_not_a_network() {
        // In mautrix, `network` names a parent portal (a Discord guild, a
        // Telegram forum): only `protocol.id` identifies the network.
        let content = json!({ "protocol": { "id": "irc" }, "network": { "id": "whatsapp" } });
        assert_eq!(resolve(&[content], "bot_alpha"), None);
    }

    #[test]
    fn the_first_marker_naming_a_known_network_wins() {
        let unknown = json!({ "protocol": { "id": "irc" } });
        let telegram = json!({ "protocol": { "id": "telegram" } });
        let signal = json!({ "protocol": { "id": "signal" } });
        assert_eq!(
            resolve(&[unknown, telegram, signal], "whatsapp_33612345678"),
            Some(Network::Telegram)
        );
    }

    #[test]
    fn an_unmarked_room_with_a_plain_sender_is_native_matrix_traffic() {
        // No m.bridge state event, no ghost prefix: the user's own Matrix
        // account, published as `matrix` rather than skipped (ADR 0009).
        assert_eq!(resolve(&[], "bot_alpha"), Some(Network::Matrix));
    }

    #[test]
    fn a_marked_room_naming_no_known_network_resolves_to_nothing() {
        // A bridge did mark this room, so it is a portal — of a network this
        // version does not support. Publishing it as native Matrix traffic
        // would mislabel bridged traffic, so it resolves to nothing and the
        // caller skips it, exactly as before native Matrix existed.
        let junk = json!({ "unrelated": true });
        assert_eq!(resolve(&[junk], "bot_alpha"), None);
        let irc = json!({ "protocol": { "id": "irc" } });
        assert_eq!(resolve(&[irc], "bot_alpha"), None);
    }

    #[test]
    fn a_marked_room_still_wins_over_native_matrix() {
        // The bridge attribution of a portal room is untouched by #18.
        let content = json!({ "protocol": { "id": "whatsapp" } });
        assert_eq!(resolve(&[content], "bot_beta"), Some(Network::Whatsapp));
        // And so is the ghost fallback in an unmarked room.
        assert_eq!(
            resolve(&[], "whatsapp_33612345678"),
            Some(Network::Whatsapp)
        );
    }

    #[test]
    fn a_ghosts_network_is_its_own_localpart_whatever_rooms_it_is_in() {
        // Issue #150's keystone: the answer comes from the identity, so no
        // ordering over rooms can change it — not even a room whose m.bridge
        // marker says otherwise, which is a portal a ghost has no business
        // being in and is not a reason to relabel the ghost.
        for rooms in [
            vec![Network::Whatsapp],
            vec![Network::Signal, Network::Whatsapp],
            vec![Network::Whatsapp, Network::Signal],
            vec![Network::Matrix, Network::Signal],
            vec![],
        ] {
            assert_eq!(
                subject_network("whatsapp_33612345678", &rooms),
                SubjectNetwork::One(Network::Whatsapp),
                "{rooms:?}"
            );
        }
        // And a transport ghost folds into its network, as everywhere else.
        assert_eq!(
            subject_network("gmessages_33612345678", &[Network::Whatsapp]),
            SubjectNetwork::One(Network::Sms)
        );
    }

    #[test]
    fn a_native_matrix_contact_in_a_portal_stays_on_matrix() {
        // The defect #150 reports. A real Matrix user whose own rooms carry no
        // m.bridge state, who is also a member of a bridged portal: `resolve`
        // reads that portal's protocol id, and the old handler stopped at the
        // first such room in id order. Sharing an unbridged room is positive
        // evidence that the subject is an account in their own right — a
        // bridge never puts its ghosts in one — so `matrix` wins here, the
        // reverse of `resolve`'s precedence for a room's own traffic.
        assert_eq!(
            subject_network("alice", &[Network::Whatsapp, Network::Matrix]),
            SubjectNetwork::One(Network::Matrix)
        );
        assert_eq!(
            subject_network("alice", &[Network::Matrix, Network::Whatsapp]),
            SubjectNetwork::One(Network::Matrix),
            "and the order the rooms came in is not part of the answer"
        );
    }

    #[test]
    fn a_prefixless_puppet_is_attributed_by_the_portals_that_hold_it() {
        // A bridge's `username_template` is its own, so a puppet's localpart
        // need not carry a network prefix. Its portals still agree, because a
        // bridge only fills its own.
        assert_eq!(
            subject_network("bot_beta", &[Network::Whatsapp, Network::Whatsapp]),
            SubjectNetwork::One(Network::Whatsapp)
        );
    }

    #[test]
    fn two_networks_and_no_ghost_prefix_is_no_answer_rather_than_a_guess() {
        // Consent is looked up by (subject, network): an arbitrary pick reads
        // the wrong row, so the Sensor publishes nothing and names what it
        // saw. Deterministic and sorted, so the log line and the test agree.
        assert_eq!(
            subject_network("alice", &[Network::Whatsapp, Network::Signal]),
            SubjectNetwork::Ambiguous(vec![Network::Signal, Network::Whatsapp])
        );
        assert_eq!(
            subject_network("alice", &[Network::Signal, Network::Whatsapp]),
            SubjectNetwork::Ambiguous(vec![Network::Signal, Network::Whatsapp])
        );
    }

    #[test]
    fn a_subject_no_room_attributes_is_not_attributed() {
        // No shared observed room, or only portals of a bridge this version
        // does not know: the same answer as before #150, and the same
        // behaviour — nothing is published.
        assert_eq!(
            subject_network("alice", &[]),
            SubjectNetwork::Unattributable
        );
        assert_eq!(
            subject_network("irc_alice", &[]),
            SubjectNetwork::Unattributable,
            "an `irc_` prefix is a ghost of a bridge this version cannot name, and calling it \
             native Matrix traffic would mislabel bridged traffic"
        );
    }

    #[test]
    fn ghost_identifiers_restore_the_stripped_phone_prefix() {
        assert_eq!(
            ghost_network_identifier(Network::Whatsapp, "whatsapp_33612345678"),
            Some("+33612345678".to_owned())
        );
        // A transport ghost folds into its network first.
        assert_eq!(
            ghost_network_identifier(Network::Sms, "gmessages_33612345678"),
            Some("+33612345678".to_owned())
        );
    }

    #[test]
    fn sms_ghost_identifiers_restore_the_stripped_phone_prefix() {
        assert_eq!(
            ghost_network_identifier(Network::Sms, "sms_33612345678"),
            Some("+33612345678".to_owned())
        );
    }

    #[test]
    fn numeric_telegram_ids_are_not_phone_numbers() {
        assert_eq!(
            ghost_network_identifier(Network::Telegram, "telegram_123456789"),
            Some("123456789".to_owned())
        );
    }

    #[test]
    fn numeric_discord_ids_are_not_phone_numbers() {
        assert_eq!(
            ghost_network_identifier(Network::Discord, "discord_80351110224678912"),
            Some("80351110224678912".to_owned())
        );
    }

    #[test]
    fn signal_ids_are_never_given_a_phone_prefix() {
        // mautrix-signal mints ghosts from the account's ACI UUID…
        assert_eq!(
            ghost_network_identifier(
                Network::Signal,
                "signal_1b3e8a4c-2d5f-4c6a-9e7b-0f1a2b3c4d5e"
            ),
            Some("1b3e8a4c-2d5f-4c6a-9e7b-0f1a2b3c4d5e".to_owned())
        );
        // …so an all-digit id is not a phone number the bridge stripped.
        assert_eq!(
            ghost_network_identifier(Network::Signal, "signal_33612345678"),
            Some("33612345678".to_owned())
        );
    }

    #[test]
    fn non_phone_ghost_identifiers_pass_through_unchanged() {
        assert_eq!(
            ghost_network_identifier(Network::Signal, "signal_abc-def"),
            Some("abc-def".to_owned())
        );
        assert_eq!(
            ghost_network_identifier(Network::Telegram, "telegram_alice"),
            Some("alice".to_owned())
        );
        // A non-digit id on a phone-based network is not a phone number.
        assert_eq!(
            ghost_network_identifier(Network::Whatsapp, "whatsapp_lid-12345"),
            Some("lid-12345".to_owned())
        );
    }

    #[test]
    fn ghost_identifiers_require_a_ghost_of_the_event_network() {
        // The prefix must match the event's network…
        assert_eq!(
            ghost_network_identifier(Network::Telegram, "whatsapp_33612345678"),
            None
        );
        // …a plain Matrix user has no derivable identifier…
        assert_eq!(
            ghost_network_identifier(Network::Whatsapp, "bot_alpha"),
            None
        );
        // …and an empty identifier is no identifier.
        assert_eq!(
            ghost_network_identifier(Network::Whatsapp, "whatsapp_"),
            None
        );
    }

    #[test]
    fn native_matrix_contacts_have_no_separate_network_identifier() {
        // On the Matrix network the Matrix user id *is* the identifier, and
        // it is already the event's subject: there is nothing to derive.
        assert_eq!(ghost_network_identifier(Network::Matrix, "alice"), None);
        assert_eq!(
            ghost_network_identifier(Network::Matrix, "matrix_alice"),
            None
        );
    }
}
