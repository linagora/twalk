//! Reading the gestures on the clerk's own `approbations` posts (#284): a
//! ✅ from the owner's key approves as written, a reply in the thread
//! approves with that text, a ❌ refuses, and anything else decides
//! nothing.
//!
//! Pure: this module takes the events the relay answered and says what
//! they mean, and the loop in [`crate::consumers`] does what they mean.
//! Three rulings are the ones to know.
//!
//! **Who decides is a key, not a role.** A Buzz channel's members can all
//! react, and the relay records every reaction alike; what makes a ✅ the
//! owner's is that it was signed by the key `CLERK_OWNER_PUBKEY` names.
//! Every decision is therefore tagged `by_owner`, and a stranger's is
//! **kept rather than dropped** ([`Triage::strangers`]): the loop answers
//! it in the thread, because a member who ticked a post and saw nothing
//! happen would conclude the clerk is broken, and a silence is the one
//! failure this project keeps shipping.
//!
//! **What counts is narrow.** A reaction (kind 7) whose content is one of
//! [`APPROVE`] or [`REFUSE`] — the two spellings of each, with or without
//! the variation selector a client may or may not append — and a
//! **direct** reply (kind 45003) to the post, whose non-empty text is the
//! reply to send in the persona's place. Another emoji is a comment, not a
//! decision; a nested reply answers a comment and not the post
//! ([`crate::relay::is_direct_reply`]); an empty reply says nothing. A
//! reply whose whole text is one of the two emoji is that gesture, because
//! an owner on a phone who types ✅ under the post meant the same thing as
//! one who reacted with it.
//!
//! **The oldest owner's gesture is the one acted on** ([`triage`]). When
//! the owner reacted ✅ and then replied with a correction before the clerk
//! looked, the ✅ is what they decided first and what they would expect to
//! have gone out; the reply is the next tick's, and by then the post is
//! gone. The loop hands this module only the gestures it has not yet
//! answered, so a gesture spent on a refusal is not the oldest for ever.
//!
//! Nothing here formats the owner's reply text: [`Gesture`]'s `Debug`
//! prints its length, never its words, so a decision in a log line cannot
//! carry what the owner wrote.

use std::fmt;

use nostr::Event;

use crate::relay::{is_direct_reply, targets, KIND_REACTION};

/// The reactions that approve a suggestion as written: `✅` (U+2705) and
/// `✔️` (U+2714 with the variation selector), the two a keyboard offers.
pub const APPROVE: [&str; 2] = ["✅", "✔️"];
/// The reactions that refuse it: `❌` (U+274C) and `✖️` (U+2716 with the
/// variation selector).
pub const REFUSE: [&str; 2] = ["❌", "✖️"];

/// The emoji variation selector (U+FE0F) a client may append to `✔` or
/// `✖` or leave off; the comparison ignores it either way.
const VARIATION_SELECTOR: char = '\u{FE0F}';

/// What one gesture on a post means.
#[derive(Clone, PartialEq, Eq)]
pub enum Gesture {
    /// Send the suggestion as the persona wrote it.
    Approve,
    /// Send this text instead: the owner's direct reply in the thread.
    ApproveEdited(String),
    /// Do not send it; the post is deleted and nobody else is told.
    Refuse,
}

impl fmt::Debug for Gesture {
    /// The edited text is the owner's reply to a contact and belongs in no
    /// log line, so it is printed by its length alone.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Gesture::Approve => write!(f, "Approve"),
            Gesture::ApproveEdited(text) => {
                write!(f, "ApproveEdited(<{} chars>)", text.chars().count())
            }
            Gesture::Refuse => write!(f, "Refuse"),
        }
    }
}

impl Gesture {
    /// The text to send in the persona's place, when the owner wrote one.
    pub fn edited_text(&self) -> Option<&str> {
        match self {
            Gesture::ApproveEdited(text) => Some(text),
            Gesture::Approve | Gesture::Refuse => None,
        }
    }
}

/// One decision read off one post: what it means, which relay event it is
/// (the id every thread answer is keyed on), whether the owner's key signed
/// it, which key did (hex, for a log line about a stranger's — a public
/// key is not a contact), and when the relay dates it — the order
/// [`triage`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub gesture: Gesture,
    pub gesture_id: String,
    pub by_owner: bool,
    pub by: String,
    pub created_at: u64,
}

/// The decisions on the post `post_id`, oldest first (ties by event id, so
/// two events the relay dated to the same second are read in one order on
/// every tick): every reaction whose content is in [`APPROVE`] or
/// [`REFUSE`] and every direct reply with a non-empty content, each tagged
/// `by_owner = author == owner_pubkey`. Anything else on the post —
/// another emoji, a nested reply, an empty reply, an event that targets
/// another post — is not a decision.
///
/// `events` is what [`crate::relay::Relay::gestures_on`] answered, and the
/// caller has dropped the clerk's own events from it: the clerk's thread
/// answers are direct replies too, and read here they would be a
/// stranger's edited approval that the clerk then answers, for ever.
pub fn decisions_on(post_id: &str, owner_pubkey: &str, events: &[Event]) -> Vec<Decision> {
    let mut decisions: Vec<Decision> = events
        .iter()
        .filter(|event| targets(event, post_id))
        .filter_map(|event| {
            let gesture = gesture_of(event, post_id)?;
            let by = event.pubkey.to_hex();
            Some(Decision {
                gesture,
                gesture_id: event.id.to_hex(),
                by_owner: by == owner_pubkey,
                by,
                created_at: event.created_at.as_secs(),
            })
        })
        .collect();
    decisions.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.gesture_id.cmp(&b.gesture_id))
    });
    decisions
}

/// What one event on `post_id` means, or `None` when it is not a decision.
fn gesture_of(event: &Event, post_id: &str) -> Option<Gesture> {
    if event.kind.as_u16() == KIND_REACTION {
        return emoji_gesture(&event.content);
    }
    if !is_direct_reply(event, post_id) {
        return None;
    }
    let text = event.content.trim();
    if text.is_empty() {
        return None;
    }
    Some(emoji_gesture(text).unwrap_or_else(|| Gesture::ApproveEdited(text.to_owned())))
}

/// The gesture a content spells when it is exactly one of the four emoji,
/// whitespace and the variation selector aside.
fn emoji_gesture(content: &str) -> Option<Gesture> {
    let spelled = bare(content);
    if APPROVE.iter().any(|emoji| bare(emoji) == spelled) {
        Some(Gesture::Approve)
    } else if REFUSE.iter().any(|emoji| bare(emoji) == spelled) {
        Some(Gesture::Refuse)
    } else {
        None
    }
}

/// `content` trimmed and without a trailing variation selector.
fn bare(content: &str) -> &str {
    content.trim().trim_end_matches(VARIATION_SELECTOR)
}

/// What the loop does with one post's decisions: the one it acts on, and
/// the strangers' it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triage {
    /// The oldest decision by the owner, if any: the one carried to the
    /// Companion Gateway or settled locally.
    pub act: Option<Decision>,
    /// Every decision by a key that is not the owner's, oldest first, each
    /// to be answered "only the owner approves" — whether or not the owner
    /// also decided, because the stranger does not know that.
    pub strangers: Vec<Decision>,
}

/// Splits the decisions on one post (oldest first, as [`decisions_on`]
/// gives them) into the one to act on and the ones to answer.
pub fn triage(decisions: Vec<Decision>) -> Triage {
    let mut act = None;
    let mut strangers = Vec::new();
    for decision in decisions {
        if !decision.by_owner {
            strangers.push(decision);
        } else if act.is_none() {
            act = Some(decision);
        }
    }
    Triage { act, strangers }
}

#[cfg(test)]
mod tests {
    use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp};

    use super::*;
    use crate::relay::KIND_FORUM_COMMENT;

    fn post_id(n: usize) -> String {
        format!("{:0>64x}", n + 1)
    }

    /// A signed event of `kind` with `tags`, by `keys`, dated `at`.
    fn event_of(keys: &Keys, kind: u16, tags: Vec<Vec<String>>, content: &str, at: u64) -> Event {
        EventBuilder::new(Kind::Custom(kind), content)
            .tags(
                tags.iter()
                    .map(|t| Tag::parse(t.iter().map(String::as_str)).unwrap()),
            )
            .custom_created_at(Timestamp::from(at))
            .sign_with_keys(keys)
            .unwrap()
    }

    fn reaction(keys: &Keys, post: &str, content: &str, at: u64) -> Event {
        event_of(
            keys,
            KIND_REACTION,
            vec![vec!["e".to_owned(), post.to_owned()]],
            content,
            at,
        )
    }

    /// A direct reply to `post`, the way Buzz's own builder writes one.
    fn reply(keys: &Keys, post: &str, content: &str, at: u64) -> Event {
        event_of(
            keys,
            KIND_FORUM_COMMENT,
            vec![
                vec!["h".to_owned(), "approbations".to_owned()],
                vec![
                    "e".to_owned(),
                    post.to_owned(),
                    String::new(),
                    "reply".to_owned(),
                ],
            ],
            content,
            at,
        )
    }

    /// A nested reply: root `post`, parent `parent`.
    fn nested_reply(keys: &Keys, post: &str, parent: &str, content: &str, at: u64) -> Event {
        event_of(
            keys,
            KIND_FORUM_COMMENT,
            vec![
                vec!["h".to_owned(), "approbations".to_owned()],
                vec![
                    "e".to_owned(),
                    post.to_owned(),
                    String::new(),
                    "root".to_owned(),
                ],
                vec![
                    "e".to_owned(),
                    parent.to_owned(),
                    String::new(),
                    "reply".to_owned(),
                ],
            ],
            content,
            at,
        )
    }

    fn owner() -> (Keys, String) {
        let keys = Keys::generate();
        let hex = keys.public_key().to_hex();
        (keys, hex)
    }

    #[test]
    fn a_check_by_the_owner_is_approve() {
        let (keys, owner) = owner();
        let post = post_id(0);
        for emoji in APPROVE {
            let event = reaction(&keys, &post, emoji, 10);
            let decisions = decisions_on(&post, &owner, std::slice::from_ref(&event));
            assert_eq!(
                decisions,
                vec![Decision {
                    gesture: Gesture::Approve,
                    gesture_id: event.id.to_hex(),
                    by_owner: true,
                    by: owner.clone(),
                    created_at: 10,
                }],
                "{emoji}"
            );
        }
        // The variation selector a client may leave off, and whitespace a
        // client may add, do not change what was meant.
        for spelled in ["✔", " ✅ ", "✔\u{FE0F}", "✅\u{FE0F}"] {
            let event = reaction(&keys, &post, spelled, 10);
            let decisions = decisions_on(&post, &owner, &[event]);
            assert_eq!(decisions.len(), 1, "{spelled:?}");
            assert_eq!(decisions[0].gesture, Gesture::Approve, "{spelled:?}");
        }
    }

    #[test]
    fn a_cross_by_the_owner_is_refuse() {
        let (keys, owner) = owner();
        let post = post_id(0);
        for emoji in REFUSE.iter().chain(["✖"].iter()) {
            let event = reaction(&keys, &post, emoji, 10);
            let decisions = decisions_on(&post, &owner, &[event]);
            assert_eq!(decisions.len(), 1, "{emoji}");
            assert_eq!(decisions[0].gesture, Gesture::Refuse, "{emoji}");
            assert!(decisions[0].by_owner);
        }
    }

    #[test]
    fn a_direct_reply_by_the_owner_is_approve_edited_with_its_text() {
        let (keys, owner) = owner();
        let post = post_id(0);
        let event = reply(&keys, &post, "Merci, à 20h alors.\n", 10);
        let decisions = decisions_on(&post, &owner, std::slice::from_ref(&event));
        assert_eq!(
            decisions,
            vec![Decision {
                gesture: Gesture::ApproveEdited("Merci, à 20h alors.".to_owned()),
                gesture_id: event.id.to_hex(),
                by_owner: true,
                by: owner.clone(),
                created_at: 10,
            }]
        );
        assert_eq!(
            decisions[0].gesture.edited_text(),
            Some("Merci, à 20h alors.")
        );

        // A reply that carries no marker on its `e` tag, as a client that
        // writes none does, is a direct reply too.
        let bare = event_of(
            &keys,
            KIND_FORUM_COMMENT,
            vec![vec!["e".to_owned(), post.clone()]],
            "Oui",
            11,
        );
        let decisions = decisions_on(&post, &owner, &[bare]);
        assert_eq!(
            decisions[0].gesture,
            Gesture::ApproveEdited("Oui".to_owned())
        );

        // A reply whose whole text is one of the emoji is that gesture, not
        // a text to send.
        let typed = reply(&keys, &post, "✅", 12);
        let decisions = decisions_on(&post, &owner, &[typed]);
        assert_eq!(decisions[0].gesture, Gesture::Approve);
        let typed = reply(&keys, &post, " ❌ ", 12);
        let decisions = decisions_on(&post, &owner, &[typed]);
        assert_eq!(decisions[0].gesture, Gesture::Refuse);
    }

    #[test]
    fn a_nested_reply_is_not_a_decision() {
        let (keys, owner) = owner();
        let post = post_id(0);
        let comment = post_id(1);
        let nested = nested_reply(&keys, &post, &comment, "et pourquoi pas 21h ?", 10);
        assert!(decisions_on(&post, &owner, std::slice::from_ref(&nested)).is_empty());
        // It answers the comment, and a comment is not a post of the
        // clerk's: no decision on it either.
        assert!(decisions_on(&comment, &owner, &[nested]).is_empty());
    }

    #[test]
    fn another_emoji_is_not_a_decision() {
        let (keys, owner) = owner();
        let post = post_id(0);
        for content in ["👍", "🙂", "✅✅", "ok", "", "❤️"] {
            let event = reaction(&keys, &post, content, 10);
            assert!(
                decisions_on(&post, &owner, &[event]).is_empty(),
                "{content:?}"
            );
        }
    }

    #[test]
    fn an_empty_reply_is_not_a_decision() {
        let (keys, owner) = owner();
        let post = post_id(0);
        for content in ["", "   ", "\n\t"] {
            let event = reply(&keys, &post, content, 10);
            assert!(
                decisions_on(&post, &owner, &[event]).is_empty(),
                "{content:?}"
            );
        }
    }

    #[test]
    fn a_check_by_a_stranger_is_flagged_not_dropped() {
        let (_, owner) = owner();
        let stranger = Keys::generate();
        let post = post_id(0);
        let event = reaction(&stranger, &post, "✅", 10);
        let decisions = decisions_on(&post, &owner, std::slice::from_ref(&event));
        assert_eq!(
            decisions,
            vec![Decision {
                gesture: Gesture::Approve,
                gesture_id: event.id.to_hex(),
                by_owner: false,
                by: stranger.public_key().to_hex(),
                created_at: 10,
            }]
        );
        let triage = triage(decisions);
        assert_eq!(triage.act, None);
        assert_eq!(triage.strangers.len(), 1);
        assert_eq!(triage.strangers[0].gesture_id, event.id.to_hex());
    }

    #[test]
    fn a_gesture_on_another_post_is_not_a_decision_on_this_one() {
        let (keys, owner) = owner();
        let post = post_id(0);
        let other = post_id(1);
        let on_other = reaction(&keys, &post, "✅", 10);
        assert!(decisions_on(&other, &owner, &[on_other]).is_empty());
    }

    #[test]
    fn triage_takes_the_owners_oldest() {
        let (keys, owner) = owner();
        let stranger = Keys::generate();
        let post = post_id(0);
        let strangers_first = reaction(&stranger, &post, "✅", 1);
        let owners_check = reaction(&keys, &post, "✅", 2);
        let owners_reply = reply(&keys, &post, "plutôt 21h", 3);
        let owners_cross = reaction(&keys, &post, "❌", 4);
        // Handed in any order: the relay's is not promised.
        let events = [
            owners_cross.clone(),
            owners_reply.clone(),
            strangers_first.clone(),
            owners_check.clone(),
        ];
        let decisions = decisions_on(&post, &owner, &events);
        assert_eq!(
            decisions.iter().map(|d| d.created_at).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );

        let triage = triage(decisions);
        let act = triage.act.expect("the owner decided");
        assert_eq!(act.gesture, Gesture::Approve);
        assert_eq!(act.gesture_id, owners_check.id.to_hex());
        assert_eq!(act.created_at, 2);
        assert_eq!(triage.strangers.len(), 1);
        assert_eq!(triage.strangers[0].created_at, 1);
        assert_eq!(triage.strangers[0].gesture_id, strangers_first.id.to_hex());
    }

    #[test]
    fn triage_of_nothing_is_nothing() {
        let triage = triage(Vec::new());
        assert_eq!(triage.act, None);
        assert!(triage.strangers.is_empty());
    }

    #[test]
    fn the_clerks_own_answer_would_read_as_a_decision_so_the_caller_drops_it() {
        // What the loop must never hand this module: the clerk's own thread
        // answer is a direct reply with text, and read as a gesture it is a
        // stranger's edited approval — which the clerk would answer, and
        // answer again. The filter is the loop's; this pins why.
        let (_, owner) = owner();
        let clerk = Keys::generate();
        let post = post_id(0);
        let answer = event_of(
            &clerk,
            KIND_FORUM_COMMENT,
            vec![
                vec!["h".to_owned(), "approbations".to_owned()],
                vec![
                    "e".to_owned(),
                    post.clone(),
                    String::new(),
                    "reply".to_owned(),
                ],
                vec!["r".to_owned(), format!("twalk:gesture:{}", post_id(5))],
            ],
            "Seul le propriétaire approuve.",
            10,
        );
        let decisions = decisions_on(&post, &owner, &[answer]);
        assert_eq!(decisions.len(), 1);
        assert!(!decisions[0].by_owner);
    }

    #[test]
    fn a_decisions_debug_never_carries_the_owners_text() {
        let marker = "MARKER-the-owners-reply-4c1e";
        let decision = Decision {
            gesture: Gesture::ApproveEdited(marker.to_owned()),
            gesture_id: post_id(0),
            by_owner: true,
            by: post_id(1),
            created_at: 10,
        };
        let printed = format!("{decision:?}");
        assert!(!printed.contains(marker), "{printed}");
        assert!(printed.contains("ApproveEdited(<28 chars>)"), "{printed}");
    }
}
