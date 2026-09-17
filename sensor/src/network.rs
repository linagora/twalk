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
}

impl Network {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Whatsapp => "whatsapp",
            Self::Telegram => "telegram",
            Self::Signal => "signal",
            Self::Discord => "discord",
            Self::Sms => "sms",
        }
    }

    /// Maps a bridge-reported protocol id (from an `m.bridge` state event, or
    /// a ghost prefix) to a contract network. Transport ids like `gmessages`
    /// fold into their user-facing network.
    pub fn from_bridge_id(id: &str) -> Option<Self> {
        match id {
            "whatsapp" => Some(Self::Whatsapp),
            "telegram" => Some(Self::Telegram),
            "signal" => Some(Self::Signal),
            "discord" => Some(Self::Discord),
            "sms" => Some(Self::Sms),
            "gmessages" => Some(Self::Sms),
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
            _ => None,
        }
    }
}

/// Resolves the network of an observed event: the content of the room's
/// `m.bridge` state event (the mautrix mechanism) wins; the ghost naming
/// convention is the fallback. `None` when neither yields a network.
pub fn resolve(bridge_content: Option<&Value>, sender_localpart: &str) -> Option<Network> {
    if let Some(id) = bridge_content
        .and_then(|content| content.pointer("/network/id"))
        .and_then(Value::as_str)
    {
        if let Some(network) = Network::from_bridge_id(id) {
            return Some(network);
        }
    }
    Network::from_ghost_localpart(sender_localpart)
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
/// derivable identifier.
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
        assert_eq!(Network::from_contract_value("whatsapp"), Some(Network::Whatsapp));
        assert_eq!(Network::from_contract_value("sms"), Some(Network::Sms));
        assert_eq!(Network::from_contract_value("gmessages"), None);
        assert_eq!(Network::from_contract_value("irc"), None);
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
        let content = json!({ "network": { "id": "signal" }, "protocol": { "id": "signal" } });
        assert_eq!(
            resolve(Some(&content), "whatsapp_33612345678"),
            Some(Network::Signal)
        );
    }

    #[test]
    fn ghost_prefix_is_the_fallback() {
        assert_eq!(
            resolve(None, "whatsapp_33612345678"),
            Some(Network::Whatsapp)
        );
    }

    #[test]
    fn bridge_transports_fold_through_resolve() {
        let content = json!({ "network": { "id": "gmessages" } });
        assert_eq!(resolve(Some(&content), "bot_alpha"), Some(Network::Sms));
    }

    #[test]
    fn nothing_resolved_without_bridge_state_or_prefix() {
        assert_eq!(resolve(None, "bot_alpha"), None);
        let junk = json!({ "unrelated": true });
        assert_eq!(resolve(Some(&junk), "bot_alpha"), None);
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
        assert_eq!(ghost_network_identifier(Network::Whatsapp, "bot_alpha"), None);
        // …and an empty identifier is no identifier.
        assert_eq!(ghost_network_identifier(Network::Whatsapp, "whatsapp_"), None);
    }
}
