//! Hermes: the agent platform's consumer runtime.
//!
//! The runtime hosts personas as separate processes, supervises them, and
//! exposes the approval endpoint that turns a human decision into a
//! `persona.reply.approved` event (see
//! `docs/architecture/adr/0008-hermes-rust-runtime-personas-as-processes.md`).
//!
//! Ticket #23 is the lifecycle: the runtime reads its persona list from the
//! environment, **constructs** each persona's environment, starts one
//! process per persona, restarts a crashed one with backoff, follows the
//! consent stream to learn which personas the user activated, and stops
//! cleanly on SIGTERM. Every transition is a structured log, because that
//! is the only thing an operator has to see what is running.
//!
//! Three properties are worth naming here rather than leaving in a
//! function somewhere, because they are what the ADRs actually decided:
//!
//! 1. **A persona's environment is constructed, never inherited**
//!    ([`environment`]). The runtime holds credentials a persona has no
//!    business holding — starting with the Companion Gateway's service
//!    token, which also opens the consent snapshot, the list of every
//!    contact (ADR 0015). A third-party persona is treated exactly like a
//!    first-party one (ADR 0008), so "it would never look" is not a
//!    control. The child gets the variables the runtime chose and nothing
//!    else.
//! 2. **Activation is a consent decision, and it is not a process switch**
//!    ([`activation`]). A paused persona still runs and receives nothing
//!    (ADR 0013): what activation moves is the persona's durable consumer
//!    on the bus, never its process. Nothing in [`supervisor`] can see the
//!    activation state, which is how that stays true.
//! 3. **A runtime that cannot start something says so** ([`supervisor`]).
//!    A persona whose image will not run is announced as failed after a
//!    bounded number of attempts, rather than restarting quietly forever
//!    and looking like a runtime that is still starting up.
//! 4. **What a persona runs with is the user's choice, read once, and
//!    attributed** ([`settings`], ticket #184). The model and the language
//!    live in the Companion Gateway; the runtime reads them with the service
//!    token, merges them underneath whatever the operator pinned on the host,
//!    and says at startup which value came from where. A Gateway that does
//!    not answer does not stop the runtime — the Sensor's answer to the same
//!    question, copied rather than reinvented.
//!
//! The approval API (#24) and the rest of the platform land in the tickets
//! that follow.

pub mod activation;
pub mod config;
pub mod environment;
pub mod settings;
pub mod supervisor;
