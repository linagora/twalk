//! The owner's own device: the identity Twalk **acts** as, beside the identity
//! it observes with (ADR 0025, ADR 0034, issue #123).
//!
//! A mautrix bridge relays to its network only what the **logged-in user's own
//! Matrix account** sends. A message from `@sensor:` is not a message from the
//! user, so the bridge ignores it — without a log line, which is how the
//! outbound half of this product reported itself healthy while nothing left
//! the deployment. The answer ADR 0025 decided is a second Matrix client: a
//! device of the owner's own account, held beside the Sensor's own session.
//!
//! One identity observes and one identity acts, and **they are never
//! confused**. Everything the register, the consent model and ADR 0024 rest on
//! is the observing identity's: the owner's device joins portal rooms and posts
//! approved replies, it publishes nothing, it reads no history, and it is never
//! what makes a conversation `observing`.
//!
//! This module holds the two decisions that are policy rather than plumbing —
//! which invitations the owner's device may accept, and what a posted reply
//! reached — and nothing that does I/O; `main.rs` wires them to matrix-sdk.

use crate::bridge_bot::BridgeBots;

/// Subdirectory of `SENSOR_STATE_DIR` the owner-device's own stores live in.
///
/// Never shared with the Sensor's own stores, and not because of tidiness: the
/// crypto store is bound to **one device** (matrix-sdk refuses to open one
/// belonging to another, `CryptoStoreError::MismatchedAccount`), and these are
/// two devices of two different accounts. The state store is separate for the
/// same reason it is per-session everywhere else — its rooms, its sync token.
pub const STORE_SUBDIR: &str = "owner-device";

/// What the owner's device may do with one pending invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invitation {
    /// A portal of a configured bridge: accept it, so the owner is a **joined**
    /// member of the room their conversation lives in and the bridge relays
    /// what they send there.
    JoinPortal,
    /// Refused, with the reason to log and count.
    Refuse(Refusal),
}

/// Why an invitation to the owner's device was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The inviter is not one of the bridge bots this deployment named. The
    /// ordinary case, and the safe one: anybody on any homeserver may invite
    /// the owner's account into a room.
    InviterIsNotABridgeBot,
    /// `SENSOR_BRIDGE_BOTS` is empty, so no invitation can ever be recognised
    /// as a portal's. Refused like any other, and worth its own reason because
    /// the symptom — the owner joins nothing, every reply reaches nobody — has
    /// one configuration line behind it.
    NoBridgeBotsConfigured,
}

impl Refusal {
    /// The metric's label value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InviterIsNotABridgeBot => "inviter_is_not_a_bridge_bot",
            Self::NoBridgeBotsConfigured => "no_bridge_bots_configured",
        }
    }
}

/// Whether the owner's device may accept an invitation, decided **on the
/// inviter alone**.
///
/// The reasoning is the whole of this function and it is a security argument,
/// so it is written here rather than left to a reader. Three things arrive with
/// an invitation and two of them are the *inviter's* to choose: the room id,
/// the room's name, and its state — including the `m.bridge` marker that says
/// "this is a WhatsApp portal". Anybody with an account anywhere may create a
/// room, write whatever `m.bridge` marker they like into it, and invite
/// `@michel:`. So the marker cannot be the gate: joining on it would let a
/// stranger put a device of the user's own account into a room of their
/// choosing, and that device *posts messages*.
///
/// What is **not** the inviter's to choose is who the homeserver records as the
/// sender of the `m.room.member` invite. That is the one authenticated fact in
/// the invitation, and it is the gate: an exact match against the bridge bots
/// the deployment named (`SENSOR_BRIDGE_BOTS`), each one this deployment's own
/// appservice identity. A portal of a configured bridge is invited by that
/// bridge's bot; nothing else is joined, whatever it claims about itself.
///
/// `SENSOR_BRIDGE_BOTS` and not `SENSOR_ALLOWED_INVITERS`, for the reason issue
/// #152 separated the two lists in the first place: that one answers a
/// different question (who may put the **Sensor** in a room) and also names the
/// operator, whose own invitations are not portals and must not be a way to
/// place their device anywhere.
///
/// The marker is still read afterwards, to log which network the portal is of.
/// Corroboration in a log line is a different thing from a gate.
pub fn invitation(inviter: &str, bridge_bots: &BridgeBots) -> Invitation {
    if bridge_bots.is_empty() {
        return Invitation::Refuse(Refusal::NoBridgeBotsConfigured);
    }
    if bridge_bots.contains(inviter) {
        Invitation::JoinPortal
    } else {
        Invitation::Refuse(Refusal::InviterIsNotABridgeBot)
    }
}

/// What the homeserver answered a join with, reduced to the two facts that
/// decide whether trying again can ever change the answer.
///
/// Plain data rather than matrix-sdk's error type, so the decision below can
/// be made — and tested — from what was *answered*, which is the only thing
/// the decision is allowed to rest on (issue #237: attempt counts alone told
/// the loop nothing, and it ran a permanent refusal at sync frequency).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinAnswer {
    /// The HTTP status, when the request reached a homeserver at all.
    pub status: Option<u16>,
    /// The Matrix `errcode` (`M_FORBIDDEN`, `M_UNKNOWN`…), when the body had one.
    pub errcode: Option<String>,
}

/// Whether a failed join is worth trying again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinFailure {
    /// The homeserver said no in a way that will not change: the room is gone
    /// or unreachable, the invitation is no longer valid, the request itself
    /// is refused. Said once, counted once, never retried in this process.
    Permanent,
    /// Nothing about the room was decided — the request was throttled, the
    /// server or the network failed. Retried, with a delay that grows.
    Transient,
}

/// Decides, from what the homeserver answered, whether a join can ever succeed.
///
/// The one this was written for is `404 M_UNKNOWN "Can't join remote room
/// because no servers that are in the room have been provided"`: an orphan
/// portal every member has left, which the homeserver has no route into and
/// never will. Treating it as a blip is what produced seventy-eight attempts
/// in five minutes, at `ERROR`, for one room — a log an operator stops
/// reading, in which the *next* real failure arrives unread.
///
/// The rule is on the **status class**, and the errcode only refines it:
///
/// - `429` is a throttle, whatever its body says, and clears on its own;
/// - every other `4xx` is the homeserver saying this request, for this room,
///   as this user, is refused — not found, not invited any more, banned,
///   restricted and ungrantable, malformed. Nothing this loop does changes any
///   of that, so trying again is noise;
/// - `5xx` and no status at all (the request never got an answer) say nothing
///   about the room, so they are retried.
///
/// The human-readable `error` string is deliberately not consulted: it is
/// prose, it differs between homeservers, and a decision that greps it is a
/// decision that silently stops working on the next Synapse release.
pub fn join_failure(answer: &JoinAnswer) -> JoinFailure {
    match answer.status {
        Some(429) => JoinFailure::Transient,
        // A throttle can also arrive as `M_LIMIT_EXCEEDED` behind a proxy that
        // rewrote the status; it is a throttle all the same.
        Some(status)
            if (400..500).contains(&status)
                && answer.errcode.as_deref() != Some("M_LIMIT_EXCEEDED") =>
        {
            JoinFailure::Permanent
        }
        _ => JoinFailure::Transient,
    }
}

/// How long to wait before the `attempt`-th retry of a transiently failed join
/// (the first retry is attempt 1).
///
/// Doubles from thirty seconds and stops growing at thirty minutes: a portal
/// whose homeserver is down for an afternoon is picked up within half an hour
/// of its return, and a portal that fails transiently for ever costs the log
/// two lines an hour rather than sixteen a minute. The base is the sync
/// timeout, because a retry sooner than that cannot have new information.
pub fn retry_delay(attempt: u32) -> std::time::Duration {
    const BASE_SECS: u64 = 30;
    const CAP_SECS: u64 = 30 * 60;
    let doubled = BASE_SECS.saturating_mul(1u64 << attempt.saturating_sub(1).min(16));
    std::time::Duration::from_secs(doubled.min(CAP_SECS))
}

/// What a reply the Sensor has just posted reached — the distinction issue #216
/// is about, which was invisible before this: an event id existed, a stream
/// position existed, every component reported itself healthy, and the contact
/// received nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The contact. Either the reply was posted by a device of the owner's own
    /// account into a room the owner is a **joined** member of — which is what
    /// a bridge relays — or the room is not a portal at all, so no bridge
    /// stands between the room and the person reading it.
    Contact,
    /// Nobody. The reply was posted by `@sensor:` into a portal room: Synapse
    /// accepted it and returned an event id, and the bridge ignored it, because
    /// it relays only the logged-in user's own account (issue #123). The
    /// message exists in a Matrix room the contact cannot see.
    Nobody,
}

impl Reach {
    /// The value carried on the bus and in the metric's label.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contact => "contact",
            Self::Nobody => "nobody",
        }
    }

    /// True when the contact receives the reply. The name is the question a
    /// consumer is really asking, and it is deliberately not `== Contact`
    /// spelled out at every call site.
    pub fn reaches_the_contact(&self) -> bool {
        matches!(self, Self::Contact)
    }
}

/// What a posted reply reached, from the two facts that decide it.
///
/// `by_the_owners_device` is true only when the reply was sent by the owner's
/// own device, which the Sensor lets happen only for a room that device is a
/// **joined** member of — so "posted by the owner's device" already carries
/// "the owner is in the room", which is the half of the answer the bridge cares
/// about.
///
/// `the_room_is_a_portal` is whether a bridge stands between the room and the
/// contact. A room no bridge marked is native Matrix traffic (ADR 0009): the
/// room *is* the conversation, so a message in it has reached the person
/// reading it, under the Sensor's identity rather than the user's. That
/// remaining difference — who the contact sees it from — is ADR 0019's
/// question and not this one; what #216 asks is whether the message arrived at
/// all.
pub fn reach(by_the_owners_device: bool, the_room_is_a_portal: bool) -> Reach {
    match (by_the_owners_device, the_room_is_a_portal) {
        (true, _) => Reach::Contact,
        (false, false) => Reach::Contact,
        (false, true) => Reach::Nobody,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHATSAPP_BOT: &str = "@whatsappbot:twalk.localhost";
    const SIGNAL_BOT: &str = "@signalbot:twalk.localhost";
    const OWNER: &str = "@michel:twalk.localhost";
    const STRANGER: &str = "@mallory:elsewhere.example";

    fn bots() -> BridgeBots {
        BridgeBots::new([WHATSAPP_BOT.to_owned(), SIGNAL_BOT.to_owned()])
    }

    #[test]
    fn a_configured_bridges_bot_places_the_owners_device_in_its_portal() {
        assert_eq!(invitation(WHATSAPP_BOT, &bots()), Invitation::JoinPortal);
        assert_eq!(invitation(SIGNAL_BOT, &bots()), Invitation::JoinPortal);
    }

    #[test]
    fn nobody_else_may_place_a_device_of_the_users_account_anywhere() {
        // The room id, the room's name and its `m.bridge` marker are all the
        // inviter's to choose, so a stranger claiming to be a WhatsApp portal
        // is exactly the input this refuses. The inviter is the only
        // authenticated fact in an invitation.
        assert_eq!(
            invitation(STRANGER, &bots()),
            Invitation::Refuse(Refusal::InviterIsNotABridgeBot)
        );
        // The operator's own invitations are not portals either: their list is
        // SENSOR_ALLOWED_INVITERS, which answers who may put the *Sensor* in a
        // room, and reusing it here would make it a way to place the user's
        // own device in any room at all.
        assert_eq!(
            invitation(OWNER, &bots()),
            Invitation::Refuse(Refusal::InviterIsNotABridgeBot)
        );
    }

    #[test]
    fn a_deployment_that_named_no_bridge_bot_joins_nothing_and_says_which_line_it_is() {
        // The symptom is "every reply reaches nobody", which is the defect
        // itself; the cause is one unset variable, so it gets its own reason
        // rather than being counted as a stranger's invitation.
        assert_eq!(
            invitation(WHATSAPP_BOT, &BridgeBots::new(Vec::<String>::new())),
            Invitation::Refuse(Refusal::NoBridgeBotsConfigured)
        );
    }

    fn answer(status: Option<u16>, errcode: Option<&str>) -> JoinAnswer {
        JoinAnswer {
            status,
            errcode: errcode.map(str::to_owned),
        }
    }

    #[test]
    fn an_orphan_portal_is_given_up_on_from_what_synapse_answered() {
        // Issue #237, verbatim from the reference deployment: every member had
        // left, the homeserver had no server to join through, and it said so
        // with a 404 — seventy-eight times in five minutes, because the loop
        // read none of it.
        assert_eq!(
            join_failure(&answer(Some(404), Some("M_UNKNOWN"))),
            JoinFailure::Permanent
        );
        // The invitation was withdrawn, or the owner was banned: not this
        // device's to change either.
        assert_eq!(
            join_failure(&answer(Some(403), Some("M_FORBIDDEN"))),
            JoinFailure::Permanent
        );
        assert_eq!(
            join_failure(&answer(Some(404), Some("M_NOT_FOUND"))),
            JoinFailure::Permanent
        );
        assert_eq!(
            join_failure(&answer(Some(400), Some("M_UNABLE_TO_GRANT_JOIN"))),
            JoinFailure::Permanent
        );
        // A 4xx with no parsable body is still the homeserver refusing.
        assert_eq!(
            join_failure(&answer(Some(404), None)),
            JoinFailure::Permanent
        );
    }

    #[test]
    fn a_throttle_a_server_error_and_no_answer_at_all_are_retried() {
        assert_eq!(
            join_failure(&answer(Some(429), Some("M_LIMIT_EXCEEDED"))),
            JoinFailure::Transient
        );
        // The same throttle with its status rewritten by a proxy in front.
        assert_eq!(
            join_failure(&answer(Some(400), Some("M_LIMIT_EXCEEDED"))),
            JoinFailure::Transient
        );
        assert_eq!(
            join_failure(&answer(Some(502), None)),
            JoinFailure::Transient
        );
        assert_eq!(
            join_failure(&answer(Some(500), Some("M_UNKNOWN"))),
            JoinFailure::Transient
        );
        // The request never reached a homeserver: nothing about the room is known.
        assert_eq!(join_failure(&answer(None, None)), JoinFailure::Transient);
    }

    #[test]
    fn the_retry_delay_doubles_from_the_sync_timeout_and_stops_at_half_an_hour() {
        use std::time::Duration;
        assert_eq!(retry_delay(1), Duration::from_secs(30));
        assert_eq!(retry_delay(2), Duration::from_secs(60));
        assert_eq!(retry_delay(3), Duration::from_secs(120));
        assert_eq!(retry_delay(7), Duration::from_secs(30 * 60));
        assert_eq!(retry_delay(8), Duration::from_secs(30 * 60), "capped");
        assert_eq!(
            retry_delay(u32::MAX),
            Duration::from_secs(30 * 60),
            "no overflow"
        );
        // Attempt 0 is not a retry; treated as the first.
        assert_eq!(retry_delay(0), Duration::from_secs(30));
    }

    #[test]
    fn only_the_sensors_own_post_into_a_portal_reaches_nobody() {
        assert_eq!(
            reach(true, true),
            Reach::Contact,
            "the user's own account in a portal room is what a bridge relays"
        );
        assert_eq!(
            reach(false, true),
            Reach::Nobody,
            "issue #123: the bridge ignores @sensor: without a log line"
        );
        assert_eq!(
            reach(false, false),
            Reach::Contact,
            "native Matrix (ADR 0009) has no bridge to ignore it: the room is the conversation"
        );
        assert_eq!(reach(true, false), Reach::Contact);
        assert!(
            reach(false, true).as_str() == "nobody" && !reach(false, true).reaches_the_contact()
        );
    }
}
