"""The consent gate.

This is the module the whole SDK exists for. Twalk's promise is that no
message is processed without the sender's consent, and the spec puts the
gate *here* rather than in each persona: JetStream cannot filter by message
header, so a consumer receives every inbound event and has to decide — and
a persona author who forgets to decide would breach the promise silently.
The SDK decides first, before any persona code runs (see
``twalk_sdk.persona``).

Two properties matter, and both are tested:

* The decision reads the envelope's own ``consent`` extension — the
  top-level CloudEvents attribute — and nothing else. It never looks inside
  ``data``, so it cannot come to depend on a field that may not be there: a
  revoked sender's message arrives with no body, no excerpt and no
  attachment reference at all (ADR 0012), and the gate drops it before that
  shape could ever matter.
* Anything that is not exactly ``granted`` is refused. Not a denylist of
  ``pending`` and ``revoked``: an event whose consent extension is missing,
  misspelled, or a value a future contract adds is refused too, because the
  safe default when consent cannot be read is to not process the message.
"""

from __future__ import annotations

from typing import Any, Mapping, Optional

#: The one consent state a persona may process events under (``CONTEXT.md``:
#: "Personas must not process events whose consent is not `granted`").
GRANTED = "granted"


def consent_of(event: Mapping[str, Any]) -> Optional[str]:
    """The event's ``consent`` extension, or ``None`` when it has none.

    A non-string value counts as absent: the contract's extension is a
    string, and a producer that sent something else has told us nothing we
    can act on.
    """
    value = event.get("consent")
    return value if isinstance(value, str) else None


def is_granted(event: Mapping[str, Any]) -> bool:
    """Whether a persona may process this event at all."""
    return consent_of(event) == GRANTED
