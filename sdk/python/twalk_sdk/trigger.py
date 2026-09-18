"""The event a persona was woken by, as the persona reads it.

A thin, read-only view over the raw CloudEvents envelope: the SDK hands the
whole event over (nothing is hidden from a persona that has passed the
consent gate) with named accessors for what the contract guarantees, so a
persona author reads ``message.body`` instead of
``event["data"]["body"]`` and cannot mistype a path.

Every accessor tolerates absence, including ones the schema marks required:
a reduced event (a revoked sender's message, ADR 0012) has no body, an
unknown ``network`` value is forward-compatible by design (a network the
contract adds later must not crash a persona), and an event that reaches a
persona at all has already passed the consent gate.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, List, Mapping, Optional

MESSAGE_RECEIVED_TYPE = "fr.linagora.twalk.inbound.message.received.v1"


@dataclass(frozen=True)
class Trigger:
    """The envelope half: what any persona event must carry back."""

    event: Mapping[str, Any]

    def _attribute(self, name: str) -> Optional[str]:
        value = self.event.get(name)
        return value if isinstance(value, str) else None

    @property
    def event_id(self) -> str:
        """The trigger's CloudEvents id: the natural key of everything the
        persona publishes about it, and the ``subject`` of those events."""
        return self._attribute("id") or ""

    @property
    def event_type(self) -> str:
        return self._attribute("type") or ""

    @property
    def network(self) -> Optional[str]:
        return self._attribute("network")

    @property
    def consent(self) -> Optional[str]:
        return self._attribute("consent")

    @property
    def traceparent(self) -> Optional[str]:
        """The W3C trace the persona's own events continue."""
        return self._attribute("traceparent")

    @property
    def sender(self) -> Optional[str]:
        """The trigger's ``subject``: for an inbound message, the sender's
        Matrix ID."""
        return self._attribute("subject")

    @property
    def source(self) -> Optional[str]:
        """The trigger's ``source``: for an inbound message, the portal room."""
        return self._attribute("source")

    @property
    def data(self) -> Mapping[str, Any]:
        data = self.event.get("data")
        return data if isinstance(data, Mapping) else {}


@dataclass(frozen=True)
class InboundMessage(Trigger):
    """An ``inbound.message.received`` trigger."""

    @property
    def body(self) -> Optional[str]:
        """The message text, or ``None`` when the event carries none."""
        value = self.data.get("body")
        return value if isinstance(value, str) else None

    @property
    def text_format(self) -> str:
        """The body's content type (``text/plain`` unless the source said
        otherwise). Named ``text_format`` because ``format`` is a builtin."""
        value = self.data.get("format")
        return value if isinstance(value, str) else "text/plain"

    @property
    def attachments(self) -> List[Mapping[str, Any]]:
        value = self.data.get("attachments")
        if not isinstance(value, list):
            return []
        return [item for item in value if isinstance(item, Mapping)]

    @property
    def sender_display_name(self) -> Optional[str]:
        contact = self.data.get("contact")
        if not isinstance(contact, Mapping):
            return None
        value = contact.get("display_name")
        return value if isinstance(value, str) else None

    @property
    def is_reply(self) -> bool:
        return isinstance(self.data.get("reply_to"), Mapping)
