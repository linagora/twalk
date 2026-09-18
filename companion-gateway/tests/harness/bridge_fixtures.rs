//! The captured mautrix answers, loaded from `harness/fixtures/` (#106).
//!
//! # Why this module exists
//!
//! Three bugs in a row were the same bug: the stub bridge answered a document
//! no mautrix bridge produces, ten Gateway tests went green, and a real
//! WhatsApp login failed on a field nobody had ever seen a bridge send. The
//! shapes the Gateway is tested against must therefore not be written by
//! hand. They are recorded from a live bridge, committed as JSON, and served
//! from there — so "the tests pass" and "a real login works" stop being
//! independent facts.
//!
//! `fixtures/README.md` says where each file came from, what was substituted
//! (a QR payload is a credential; the owner's phone number is personal data)
//! and — the part worth reading — which shapes could **not** be captured
//! without a phone.
//!
//! # What a fixture is
//!
//! One file per endpoint per bridge, each holding every answer that endpoint
//! was seen to give, keyed by name:
//!
//! ```json
//! { "endpoint": "…", "bridge": "…", "answers": { "qr": { "status": 200, "body": { … } } } }
//! ```
//!
//! [`answer`] returns one of them. The stub then substitutes only the values
//! a live bridge would have made up fresh — the process id, the transaction
//! id, the QR payload — and sends everything else byte for byte.

#![allow(dead_code)]

use serde_json::Value;

/// Which bridge's captured answers to serve. The two reference bridges do
/// **not** agree with each other — one flow versus two, different step ids,
/// and an unknown flow id that is a 404 on one and a silent fallback to QR on
/// the other — so the stub can wear either face and the Gateway is held to
/// both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bridge {
    Whatsapp,
    Signal,
}

impl Bridge {
    /// The directory name, which is also the bridge's own name.
    pub fn as_str(self) -> &'static str {
        match self {
            Bridge::Whatsapp => "mautrix-whatsapp",
            Bridge::Signal => "mautrix-signal",
        }
    }

    /// The image the answers were captured from, for a test that wants to
    /// say so in its failure message.
    pub fn image(self) -> &'static str {
        match self {
            Bridge::Whatsapp => "dock.mau.dev/mautrix/whatsapp:v26.09",
            Bridge::Signal => "dock.mau.dev/mautrix/signal:v26.09",
        }
    }

    /// The step id of this bridge's QR step, which is namespaced per
    /// connector and which nothing may hard-code.
    pub fn qr_step_id(self) -> &'static str {
        match self {
            Bridge::Whatsapp => "fi.mau.whatsapp.login.qr",
            Bridge::Signal => "fi.mau.signal.login.qr",
        }
    }
}

/// Every fixture file, compiled in so a test binary needs no working
/// directory and no I/O.
const FILES: &[(&str, &str, &str)] = &[
    (
        "mautrix-whatsapp",
        "whoami",
        include_str!("fixtures/mautrix-whatsapp/whoami.json"),
    ),
    (
        "mautrix-whatsapp",
        "login-flows",
        include_str!("fixtures/mautrix-whatsapp/login-flows.json"),
    ),
    (
        "mautrix-whatsapp",
        "logins",
        include_str!("fixtures/mautrix-whatsapp/logins.json"),
    ),
    (
        "mautrix-whatsapp",
        "login-start",
        include_str!("fixtures/mautrix-whatsapp/login-start.json"),
    ),
    (
        "mautrix-whatsapp",
        "login-step",
        include_str!("fixtures/mautrix-whatsapp/login-step.json"),
    ),
    (
        "mautrix-whatsapp",
        "login-cancel",
        include_str!("fixtures/mautrix-whatsapp/login-cancel.json"),
    ),
    (
        "mautrix-whatsapp",
        "logout",
        include_str!("fixtures/mautrix-whatsapp/logout.json"),
    ),
    (
        "mautrix-signal",
        "whoami",
        include_str!("fixtures/mautrix-signal/whoami.json"),
    ),
    (
        "mautrix-signal",
        "login-flows",
        include_str!("fixtures/mautrix-signal/login-flows.json"),
    ),
    (
        "mautrix-signal",
        "logins",
        include_str!("fixtures/mautrix-signal/logins.json"),
    ),
    (
        "mautrix-signal",
        "login-start",
        include_str!("fixtures/mautrix-signal/login-start.json"),
    ),
    (
        "mautrix-signal",
        "login-step",
        include_str!("fixtures/mautrix-signal/login-step.json"),
    ),
    (
        "mautrix-signal",
        "login-cancel",
        include_str!("fixtures/mautrix-signal/login-cancel.json"),
    ),
    (
        "mautrix-signal",
        "logout",
        include_str!("fixtures/mautrix-signal/logout.json"),
    ),
];

/// One captured answer: the status the bridge gave and the body it sent.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub body: Value,
}

/// The named answer from one bridge's fixture for one endpoint.
///
/// Panics rather than returning an error: a missing fixture is a broken test
/// harness, not a test failure, and the panic names what was asked for.
pub fn answer(bridge: Bridge, endpoint: &str, name: &str) -> Answer {
    let (_, _, contents) = FILES
        .iter()
        .find(|(dir, file, _)| *dir == bridge.as_str() && *file == endpoint)
        .unwrap_or_else(|| {
            panic!(
                "no captured answers for {endpoint:?} on {}: add \
                 tests/harness/fixtures/{}/{endpoint}.json and list it in FILES",
                bridge.as_str(),
                bridge.as_str(),
            )
        });
    let parsed: Value = serde_json::from_str(contents)
        .unwrap_or_else(|error| panic!("{}/{endpoint}.json is not JSON: {error}", bridge.as_str()));
    let answer = parsed
        .get("answers")
        .and_then(|answers| answers.get(name))
        .unwrap_or_else(|| {
            panic!(
                "{}/{endpoint}.json has no answer named {name:?}; it has {:?}",
                bridge.as_str(),
                parsed
                    .get("answers")
                    .and_then(Value::as_object)
                    .map(|answers| answers.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default(),
            )
        });
    Answer {
        status: answer
            .get("status")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                panic!(
                    "{}/{endpoint}.json's {name:?} answer names no status",
                    bridge.as_str()
                )
            }) as u16,
        body: answer.get("body").cloned().unwrap_or(Value::Null),
    }
}

/// The body of a named answer, for the common case.
pub fn body(bridge: Bridge, endpoint: &str, name: &str) -> Value {
    answer(bridge, endpoint, name).body
}
