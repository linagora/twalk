//! The clerk (*le greffier*): writes onto the owner's Buzz relay what the
//! bus says — one forum post per suggestion in `approbations`, one line per
//! posted reply in `journal`, and only what calls for a look in `activite`
//! — under a Nostr key of its own, holding no state, deleting a
//! suggestion's post when it expires (ticket #265, ADR 0035).
//!
//! The relay is the clerk's own memory: what it already posted is found by
//! querying its own posts and reading the reference line each one carries,
//! never by keeping a store of its own. See `.scratch/clerk/plan.md` for
//! the task-by-task build and `docs/architecture/adr/0035-*.md` for why.

pub mod config;
pub mod consumers;
pub mod events;
pub mod metrics;
pub mod reference;
pub mod relay;
pub mod text;
