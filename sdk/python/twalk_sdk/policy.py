"""The suggestion policy: how long a draft stays approvable, and what does
*not* count as another attempt at one.

A suggestion is a reply a persona proposes and never sends (``CONTEXT.md``).
It exists to be read, edited or refused by a human, and the two numbers that
say how it ages are decisions rather than mechanics, so they live here
instead of being spelled out at each publish site:

* ``expires_at`` — after which a suggestion should be considered stale. The
  contract makes the field optional; this SDK always sets it, because the
  approval path refuses an expired suggestion (issue #24) and an absent
  expiry is an approval that can be given at any later date. An operator may
  widen or narrow the window, never remove it.

* ``attempt`` — the suggestion's number *for its trigger*, starting at 1 and
  part of the contract's id natural key
  (``sha256(persona_id:trigger_event_id:attempt)``). It counts suggestions
  that exist, not deliveries that were tried: JetStream is at-least-once, so
  the same trigger reaches a persona again after a crash, a NAK or a
  redelivery, and a counter that moved with the delivery would show the user
  two drafts of one message — one of them from a run that failed halfway.
  A replay therefore recomputes the same id and collapses on the bus, which
  is the whole point of the deterministic key.

The expiry runs from **when the suggestion was produced**, not from the
trigger's own time. A persona's consumer starts at the beginning of the
stream (ADR 0013: activation is a consent decision, so a persona activated
today reads the messages that arrived before it existed), and a trigger can
therefore be arbitrarily old: keyed off the trigger, a persona's first
suggestions would arrive already expired.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timedelta

from .envelope import rfc3339

#: One hour, the default window a suggestion stays approvable for. Long
#: enough that the user gets to it after a meeting, short enough that
#: "à tout de suite" is not still approvable tomorrow.
DEFAULT_SUGGESTION_TTL_SECONDS = 3600


@dataclass(frozen=True)
class SuggestionPolicy:
    """How a persona's suggestions age. Pure, so it is tested without a bus."""

    ttl_seconds: int = DEFAULT_SUGGESTION_TTL_SECONDS

    def __post_init__(self) -> None:
        if isinstance(self.ttl_seconds, bool) or not isinstance(self.ttl_seconds, int):
            raise ValueError(
                "a suggestion's lifetime is a whole number of seconds, got "
                f"{self.ttl_seconds!r}"
            )
        if self.ttl_seconds < 1:
            raise ValueError(
                "a suggestion's lifetime must be at least a second: there is "
                "no 'never expires', because an approval that can be given at "
                f"any later date is the thing the expiry exists to prevent "
                f"(got {self.ttl_seconds})"
            )

    def expires_at(self, produced_at: datetime) -> str:
        """The contract's ``expires_at`` for a suggestion produced at
        ``produced_at``."""
        return rfc3339(produced_at + timedelta(seconds=self.ttl_seconds))
