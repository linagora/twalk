//! W3C Trace Context on inbound HTTP: the Gateway continues the caller's
//! trace when it carries one, and originates one otherwise, so every request
//! it answers — and everything it will later publish on the bus — belongs to
//! a trace.
//!
//! The Sensor originates its `traceparent` from the deterministic event id
//! (`sensor/src/normalize.rs::originate_traceparent`); an inbound HTTP
//! request has no such id, so the Gateway carves its ids out of a SHA-256
//! digest of the process id, the clock and a per-process counter. Same shape,
//! same lowercase-hex `00-<32>-<16>-01` form, no trace backend either: the
//! header exists so the components downstream can continue the trace.

use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The trace context to use for a request: the inbound `traceparent` when it
/// is a well-formed one, a fresh one otherwise. A malformed header is
/// replaced rather than propagated — passing garbage on would corrupt the
/// trace of every component downstream.
pub fn propagate(inbound: Option<&str>) -> String {
    match inbound {
        Some(value) if is_valid(value) => value.to_owned(),
        _ => originate(),
    }
}

/// Whether a header value is a `traceparent` this version of the spec
/// defines: version `00`, a non-zero 32-hex trace id, a non-zero 16-hex
/// parent id, and two hex flag digits.
pub fn is_valid(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 4 {
        return false;
    }
    let [version, trace_id, parent_id, flags] = [parts[0], parts[1], parts[2], parts[3]];
    let hex = |value: &str, len: usize| {
        value.len() == len
            && value
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    };
    version == "00"
        && hex(trace_id, 32)
        && trace_id.bytes().any(|byte| byte != b'0')
        && hex(parent_id, 16)
        && parent_id.bytes().any(|byte| byte != b'0')
        && hex(flags, 2)
}

/// A fresh sampled `traceparent`: `00-<32 hex trace id>-<16 hex span id>-01`.
pub fn originate() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let seed = format!(
        "{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let hex: String = Sha256::digest(seed.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("00-{}-{}-01", &hex[0..32], &hex[32..48])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn a_valid_inbound_trace_context_is_continued() {
        assert_eq!(propagate(Some(SAMPLE)), SAMPLE);
        // An unsampled but well-formed context is still the caller's.
        let unsampled = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00";
        assert_eq!(propagate(Some(unsampled)), unsampled);
    }

    #[test]
    fn a_missing_or_malformed_trace_context_is_replaced() {
        for inbound in [
            None,
            Some(""),
            Some("not-a-traceparent"),
            // Wrong version, short ids, uppercase hex, all-zero ids: the
            // cases the spec calls invalid.
            Some("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
            Some("00-4bf92f3577b34da6a3ce929d0e0e473-00f067aa0ba902b7-01"),
            Some("00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01"),
            Some("00-00000000000000000000000000000000-00f067aa0ba902b7-01"),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01"),
        ] {
            let originated = propagate(inbound);
            assert_ne!(Some(originated.as_str()), inbound, "{inbound:?}");
            assert!(
                is_valid(&originated),
                "the replacement is a valid traceparent: {originated}"
            );
        }
    }

    #[test]
    fn originated_contexts_are_unique_per_request() {
        let first = originate();
        let second = originate();
        assert_ne!(
            first, second,
            "two requests must not land in the same trace"
        );
    }
}
