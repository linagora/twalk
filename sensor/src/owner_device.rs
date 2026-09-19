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

use std::collections::HashMap;
use std::time::{Duration, Instant};

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

/// What the homeserver answered to a join request, reduced to the facts that
/// decide whether asking again can change anything.
///
/// `status` is `None` when there was no HTTP answer at all — the connection
/// was refused, timed out, or dropped — which is the one case that is
/// unambiguously the network's and not the room's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinAnswer {
    pub status: Option<u16>,
    pub errcode: Option<String>,
    pub message: String,
}

/// Why a join failed, sorted by what the Sensor should do next (issue #237).
///
/// Before this every failure was retried at sync frequency, for ever, at
/// `ERROR`: seventy-eight attempts in five minutes for one room that could
/// never be joined, which is a log nobody reads and the line that matters —
/// a portal that *should* have joined — arriving in the middle of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinFailure {
    /// Every member has left, so the homeserver has no server it could join
    /// through — its words are "no servers that are in the room". Nothing can
    /// make such a room joinable again: the conversation, if it still exists,
    /// lives in another room now (ADR 0029). The invitation is meaningless and
    /// is **rejected**, so the homeserver records the outcome (`leave`), nothing
    /// is retried after a restart either, and the register reads the fact
    /// without a log.
    Orphaned,
    /// The homeserver refused on the facts of the invitation — the device is
    /// banned, the room version is one it cannot join, the room is gone. Only a
    /// new membership event changes those facts, and one would arrive as a new
    /// invitation, so this one is not asked about again in this process. The
    /// invitation is left where it is: a refusal this project cannot name
    /// precisely is not grounds to throw the user's invitation away.
    Refused,
    /// No answer, a server error, or a rate limit: the room may be fine and
    /// the homeserver was not, so it is asked again — with backoff, never at
    /// sync frequency.
    Transient,
}

impl JoinFailure {
    /// Whether the invitation should be rejected on the homeserver, so that the
    /// outcome outlives this process.
    pub fn rejects_the_invitation(&self) -> bool {
        matches!(self, Self::Orphaned)
    }

    /// The metric's label value and the word in the log line.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Orphaned => "orphaned",
            Self::Refused => "refused",
            Self::Transient => "transient",
        }
    }
}

/// Sorts a failed join by what the homeserver answered — never by how many
/// times it has failed, because "it keeps failing" is exactly what a permanent
/// condition and a long outage have in common.
pub fn join_failure(answer: &JoinAnswer) -> JoinFailure {
    let Some(status) = answer.status else {
        return JoinFailure::Transient;
    };
    let errcode = answer.errcode.as_deref().unwrap_or("");
    if status == 429 || errcode == "M_LIMIT_EXCEEDED" || status >= 500 {
        return JoinFailure::Transient;
    }
    if answer.message.contains("no servers that are in the room") {
        return JoinFailure::Orphaned;
    }
    JoinFailure::Refused
}

/// The delay before the `attempt`-th retry of a transient failure: doubling
/// from `base`, never above `cap`. The first retry waits `base`.
pub fn backoff(attempt: u32, base: Duration, cap: Duration) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    base.saturating_mul(1u32 << exponent).min(cap)
}

/// What the owner's device remembers about the invitations it could not
/// accept, per room, for the life of the process.
///
/// A room is unknown here until a join fails. After a transient failure it is
/// **waiting**, and not asked about until its delay has passed; after a refusal
/// it is **given up**, and not asked about at all. A room the homeserver no
/// longer lists as invited is forgotten (`keep_only`), so a fresh invitation to
/// the same room — after a ban lifted, say — starts from nothing.
#[derive(Debug)]
pub struct JoinRetries {
    base: Duration,
    cap: Duration,
    rooms: HashMap<String, RetryState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryState {
    Waiting { attempts: u32, due: Instant },
    GivenUp,
}

impl JoinRetries {
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap,
            rooms: HashMap::new(),
        }
    }

    /// Whether a join should be attempted for this room now.
    pub fn is_due(&self, room: &str, now: Instant) -> bool {
        match self.rooms.get(room) {
            None => true,
            Some(RetryState::Waiting { due, .. }) => *due <= now,
            Some(RetryState::GivenUp) => false,
        }
    }

    /// Records a transient failure and returns how long this room now waits.
    pub fn failed(&mut self, room: &str, now: Instant) -> Duration {
        let attempts = match self.rooms.get(room) {
            Some(RetryState::Waiting { attempts, .. }) => attempts + 1,
            _ => 1,
        };
        let delay = backoff(attempts, self.base, self.cap);
        self.rooms.insert(
            room.to_owned(),
            RetryState::Waiting {
                attempts,
                due: now + delay,
            },
        );
        delay
    }

    /// How many times this room's join has failed transiently so far.
    pub fn attempts(&self, room: &str) -> u32 {
        match self.rooms.get(room) {
            Some(RetryState::Waiting { attempts, .. }) => *attempts,
            _ => 0,
        }
    }

    /// Records that this room will not be asked about again in this process.
    pub fn gave_up(&mut self, room: &str) {
        self.rooms.insert(room.to_owned(), RetryState::GivenUp);
    }

    /// Forgets a room that joined.
    pub fn succeeded(&mut self, room: &str) {
        self.rooms.remove(room);
    }

    /// Forgets every room `still_invited` does not hold, so an invitation that
    /// was withdrawn and sent again is a new one.
    pub fn keep_only(&mut self, still_invited: impl Fn(&str) -> bool) {
        self.rooms.retain(|room, _| still_invited(room));
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

    fn answer(status: Option<u16>, errcode: Option<&str>, message: &str) -> JoinAnswer {
        JoinAnswer {
            status,
            errcode: errcode.map(str::to_owned),
            message: message.to_owned(),
        }
    }

    #[test]
    fn a_room_everybody_left_is_orphaned_and_its_invitation_is_rejected() {
        // Word for word what Synapse answered on the reference deployment,
        // seventy-eight times in five minutes (issue #237).
        let synapse = answer(
            Some(404),
            Some("M_UNKNOWN"),
            "Can't join remote room because no servers that are in the room have been provided.",
        );
        assert_eq!(join_failure(&synapse), JoinFailure::Orphaned);
        assert!(JoinFailure::Orphaned.rejects_the_invitation());
    }

    #[test]
    fn a_failure_is_sorted_by_what_the_homeserver_answered_and_not_by_how_often() {
        // No answer at all: the network's, so the room is asked about again.
        assert_eq!(
            join_failure(&answer(None, None, "connection refused")),
            JoinFailure::Transient
        );
        // The homeserver itself in trouble, or rate-limiting: again later.
        assert_eq!(
            join_failure(&answer(Some(502), Some("M_UNKNOWN"), "Bad Gateway")),
            JoinFailure::Transient
        );
        assert_eq!(
            join_failure(&answer(
                Some(429),
                Some("M_LIMIT_EXCEEDED"),
                "Too Many Requests"
            )),
            JoinFailure::Transient
        );
        // Refused on the facts: not asked about again, but not thrown away
        // either — only the orphan's invitation is rejected.
        let banned = answer(
            Some(403),
            Some("M_FORBIDDEN"),
            "You are banned from this room",
        );
        assert_eq!(join_failure(&banned), JoinFailure::Refused);
        assert!(!JoinFailure::Refused.rejects_the_invitation());
        assert_eq!(
            join_failure(&answer(
                Some(400),
                Some("M_UNSUPPORTED_ROOM_VERSION"),
                "Your homeserver does not support the features required to join this room"
            )),
            JoinFailure::Refused
        );
        assert!(!JoinFailure::Transient.rejects_the_invitation());
    }

    #[test]
    fn a_transient_failure_backs_off_and_is_never_asked_at_sync_frequency() {
        let base = Duration::from_secs(5);
        let cap = Duration::from_secs(600);
        assert_eq!(backoff(1, base, cap), Duration::from_secs(5));
        assert_eq!(backoff(2, base, cap), Duration::from_secs(10));
        assert_eq!(backoff(5, base, cap), Duration::from_secs(80));
        assert_eq!(
            backoff(20, base, cap),
            cap,
            "capped, and no overflow far past the cap"
        );

        let mut retries = JoinRetries::new(base, cap);
        let t0 = Instant::now();
        let room = "!portal:twalk.localhost";
        assert!(
            retries.is_due(room, t0),
            "a room nothing is known about is asked about"
        );
        assert_eq!(retries.failed(room, t0), Duration::from_secs(5));
        assert!(!retries.is_due(room, t0), "not on the very next sync");
        assert!(!retries.is_due(room, t0 + Duration::from_secs(4)));
        assert!(retries.is_due(room, t0 + Duration::from_secs(5)));
        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(retries.failed(room, t1), Duration::from_secs(10));
        assert_eq!(retries.attempts(room), 2);
        assert!(!retries.is_due(room, t1 + Duration::from_secs(9)));
        assert!(retries.is_due(room, t1 + Duration::from_secs(10)));
        // The cause cleared: the room is forgotten, and a later failure of the
        // same room starts the schedule over.
        retries.succeeded(room);
        assert_eq!(retries.attempts(room), 0);
        assert_eq!(retries.failed(room, t1), Duration::from_secs(5));
    }

    #[test]
    fn a_refused_room_is_asked_about_once_per_process_and_a_fresh_invitation_starts_over() {
        let mut retries = JoinRetries::new(Duration::from_secs(5), Duration::from_secs(600));
        let now = Instant::now();
        let room = "!banned:twalk.localhost";
        retries.gave_up(room);
        assert!(!retries.is_due(room, now));
        assert!(
            !retries.is_due(room, now + Duration::from_secs(86_400)),
            "never, not just later"
        );
        // The invitation was withdrawn and sent again: the homeserver no longer
        // lists the old one, so the room is forgotten and the new one is judged
        // on its own answer.
        retries.keep_only(|_| false);
        assert!(retries.is_due(room, now));
        // But while the invitation is still the same one, it stays given up.
        retries.gave_up(room);
        retries.keep_only(|invited| invited == room);
        assert!(!retries.is_due(room, now));
    }
}
