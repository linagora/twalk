//! The Gateway's own HTTP description, served by the Gateway (ticket #63).
//!
//! The Companion is built in its own lot, by another agent, from a
//! TypeScript client generated against this document — the same property the
//! CloudEvents contract gives the bus: components built in parallel without
//! coordination, and a field that changes breaks a build instead of an
//! onboarding screen.
//!
//! `companion-gateway/openapi.yaml` is the source of truth and the file a
//! reviewer diffs; this module embeds it with [`include_str!`], so the bytes
//! the origin serves at `GET /openapi.yaml` are that file and cannot drift
//! from it — there is no generation step and nothing to regenerate. What
//! *can* drift is the description against the handlers, and that is what
//! `tests/openapi.rs` exists to refuse: it drives the running binary through
//! every operation the document declares and fails when a status, an error
//! code, a response member or an authentication requirement disagrees.
//!
//! The description is public: it is what the Companion's build points a
//! generator at, it is served before anyone can sign in, and it holds no
//! secret — only the shape of a surface whose every authenticated route
//! refuses an unauthenticated caller anyway.

use axum::http::header;
use axum::response::{IntoResponse, Response};

/// The description, embedded at build time from the committed file.
///
/// `include_str!` makes the file a compile-time dependency of the crate: a
/// build with a malformed path fails, and editing the description rebuilds
/// the binary.
pub const DESCRIPTION: &str = include_str!("../openapi.yaml");

/// The media type of a YAML document (RFC 9512). Not `text/yaml`, which that
/// RFC deprecates, and not `application/openapi+yaml`, which nothing
/// registers.
pub const CONTENT_TYPE: &str = "application/yaml";

/// `GET /openapi.yaml` — the description of this very binary.
pub async fn description() -> Response {
    (
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        // A long-lived cache would be wrong: the description changes with
        // the binary, and a client generator asking this origin wants the
        // running Gateway's own surface.
        [(header::CACHE_CONTROL, "no-cache")],
        DESCRIPTION,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_description_is_the_committed_file() {
        // include_str! guarantees it; this test states the guarantee, and
        // fails if someone replaces the embedding with a copy.
        let committed =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/openapi.yaml"))
                .expect("the description is committed next to the crate");
        assert_eq!(
            DESCRIPTION, committed,
            "the served description must be the committed file, byte for byte"
        );
        assert!(
            DESCRIPTION.starts_with("# The Companion Gateway's HTTP description"),
            "the description keeps its header comment: {:?}",
            &DESCRIPTION[..DESCRIPTION.len().min(80)]
        );
    }
}
