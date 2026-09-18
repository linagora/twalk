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

This module also holds the **trigger-type gate** (:func:`triggers_a_persona`):
whether an event may wake a persona at all, which is the question that comes
before "may this be processed?" — and the one the consent gate structurally
cannot answer for the user's own messages (ADR 0018).
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, List, Mapping, Optional

MESSAGE_RECEIVED_TYPE = "fr.linagora.twalk.inbound.message.received.v1"

#: A message the **user** sent, from their own phone (ADR 0018). It is on the
#: bus so that a persona can know a conversation has already been answered,
#: and it is never a trigger: suggesting a reply to it means answering the
#: operator — or, in Signal's Note to Self, answering nobody at all.
OUTBOUND_MESSAGE_SENT_TYPE = "fr.linagora.twalk.outbound.message.sent.v1"

#: The event types a persona may be woken by. An allowlist, for the same
#: reason the consent gate is one: a type added to the contract later must
#: not start triggering personas because nobody thought to exclude it.
PERSONA_TRIGGER_TYPES = frozenset({MESSAGE_RECEIVED_TYPE})


def type_of(event: Mapping[str, Any]) -> Optional[str]:
    """The event's ``type`` attribute, or ``None`` when it has none.

    A non-string value counts as absent, like an unreadable consent state:
    the contract's attribute is a string, and a producer that sent something
    else has told us nothing we can act on.
    """
    value = event.get("type")
    return value if isinstance(value, str) else None


def triggers_a_persona(event: Mapping[str, Any]) -> bool:
    """Whether a persona may be woken by this event at all.

    The second gate, beside the consent gate and for the same reason: an
    author cannot forget it. It exists because of one event in particular —
    ``outbound.message.sent``, the user's own message — which the consent
    gate structurally cannot stop. That event carries **no consent extension
    at all** (ADR 0018: the extension is a contact's decision, and there is
    no contact in it), so a gate that reads consent has nothing to read; and
    under the alternative the ADR rejected, where the user's own traffic
    stayed an ``inbound.message.received``, the sender would have been the
    user, whose own consent is the most granting in the system. Either way
    the consent gate says yes to answering the operator. So this one says no
    first, on the type, which is the only attribute that tells the two apart.
    """
    return type_of(event) in PERSONA_TRIGGER_TYPES


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
