"""The contract's ``persona.*`` envelopes, built so they cannot come out
malformed.

A persona author never writes a CloudEvents envelope by hand. They hand
over a :class:`Suggestion`; the SDK computes the deterministic id from the
contract's natural key, copies the trigger reference, the ``network`` and
``consent`` extensions and the trace, caps the strings the schemas cap, and
stamps the time. Everything the schema pins to one value is a constant
here.

The ids are the reason this module is pure and separately tested: a
persona's replay safety is its id discipline, and
``sha256(persona_id:trigger_event_id)`` is not something to recompute by
hand in every persona.
"""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Dict, Optional

from .disclosure import DISCLOSURE_MAX_CHARS, SENTENCES
from .trigger import Trigger

THINKING_TYPE = "fr.linagora.twalk.persona.thinking.emitted.v1"
SUGGEST_TYPE = "fr.linagora.twalk.persona.suggest.produced.v1"

SCHEMA_BASE = "https://schemas.twalk.dev/cloudevents/v1"
SPEC_VERSION = "1.0"
DATA_CONTENT_TYPE = "application/json"

#: The attempt number of a persona's first suggestion for a trigger, and
#: the only one v0.1 produces: the contract's natural key counts
#: suggestions that exist, not deliveries that were tried, so a redelivered
#: trigger is re-keyed to the same number and collapses on the bus rather
#: than becoming a second draft of one message (see
#: :mod:`twalk_sdk.policy`). A path that deliberately produces another
#: suggestion for a trigger — a human asking for a redraft, through the
#: runtime's approval surface — increments it explicitly, which is why
#: :func:`suggest_event` takes the attempt rather than counting anything.
FIRST_ATTEMPT = 1

#: The schemas' own caps, applied on the way out: a chatty model must not be
#: able to make a persona publish an event the contract refuses.
#:
#: The body's cap is the schema's 65 536 **less the disclosure and its
#: newline**: the Companion Gateway appends ``"\n" + disclosure`` to the body
#: at approval (ADR 0031), ``final.body`` on ``persona.reply.approved`` is
#: capped at 65 536 too, and a body the schema accepts here must not become
#: one the schema refuses there. The Gateway applies the same arithmetic to
#: an edited body.
MAX_BODY_CHARS = 65536 - 1 - DISCLOSURE_MAX_CHARS

#: The cap on a suggestion's :attr:`Suggestion.context` — what the persona
#: says the contact asked (#334). Short on purpose: it is read at a glance
#: above a proposed reply, and the shorter it is the less of somebody's
#: message it can become.
MAX_CONTEXT_CHARS = 280

#: The cap on the contact's display name as the context carries it.
MAX_CONTACT_CHARS = 120

#: What a context may never carry, because a model writes it and an
#: approval surface — Buzz among them, which is somebody else's relay —
#: shows it. An address is the contact's own identifier, a filename is
#: theirs too, and a link is a way to carry either. Refused on
#: construction rather than trimmed: a summary that had to be cut to be
#: publishable is one the persona should write again.
_ADDRESS = re.compile(
    r"""(?xi)
    \b[\w.+-]+@[\w-]+\.[\w.-]+       # an e-mail address
  | \bhttps?://                       # a link
  | \b(?:mailto|tel|sms):             # a URI that names a person
  | \+\d[\d\s().-]{7,}\d              # a telephone number
    """
)
_FILENAME = re.compile(
    r"(?i)\b[\w-]+\.(?:pdf|docx?|xlsx?|pptx?|odt|ods|csv|txt|zip|rar|png|jpe?g|gif|heic|mp[34]|mov)\b"
)
MAX_RATIONALE_CHARS = 2048

SUGGESTION_FORMATS = ("text/plain", "text/markdown", "text/html")


class EnvelopeError(ValueError):
    """The persona asked for an envelope the contract has no shape for.

    **Not transient**, and :mod:`twalk_sdk.persona` reads exactly that: the
    envelope is built from the trigger and the configuration, neither of
    which a redelivery changes, so the same trigger would fail the same way
    on every attempt. The one case that reaches a running persona is a
    trigger published before #269, which carries no ``connection`` — a fresh
    consumer starts at the beginning of the stream (ADR 0013) and meets every
    one of them, and three deliveries of each with an ERROR line apiece
    would be the account of nothing. One line, terminated, is.
    """

    transient = False


@dataclass(frozen=True)
class Suggestion:
    """What a persona proposes: a reply for the user to approve, never send.

    ``body`` is the reply in its canonical text form. ``confidence`` and
    ``rationale`` are optional and exist for oversight display only — a
    confidence is not a correctness claim.

    ``language`` is for an author who **knows** what language the reply is
    in — a persona that only ever writes French, or one whose model was
    asked for a structured answer that names it. Left ``None``, which is
    what the reference persona does, the SDK asks the model after the
    handler returns (:meth:`twalk_sdk.llm.Llm.language_of`). ``disclosure``
    is the sentence that language selects, and it is the SDK's to fill in
    (:mod:`twalk_sdk.disclosure`): the process loop sets it from the
    language, whatever the handler put there, so that what a contact is
    told is always one of the contract's own sentences — a value that is
    not one of them is refused on construction, for the same reason an
    author cannot forget the consent gate.
    """

    body: str
    format: str = "text/plain"
    confidence: Optional[float] = None
    rationale: Optional[str] = None
    language: Optional[str] = None
    disclosure: Optional[str] = None
    context: Optional[str] = None

    def __post_init__(self) -> None:
        if self.format not in SUGGESTION_FORMATS:
            raise EnvelopeError(
                f"format must be one of {SUGGESTION_FORMATS}, got {self.format!r}"
            )
        if self.confidence is not None and not 0.0 <= self.confidence <= 1.0:
            raise EnvelopeError(
                f"confidence must be between 0 and 1, got {self.confidence!r}"
            )
        if self.context is not None:
            _refuse_unless_a_summary(self.context)
        if self.disclosure is not None and self.disclosure not in SENTENCES.values():
            raise EnvelopeError(
                "disclosure must be one of the contract's sentences "
                "(contracts/disclosure/v1/sentences.json, ADR 0031) — it is "
                "selected by language, never written; set `language` instead, "
                f"got {self.disclosure!r}"
            )


def _refuse_unless_a_summary(context: str) -> None:
    """Refuses a context that is not a summary (#334).

    The rule it enforces is short because the rule it *cannot* enforce is
    the important one: a persona must say what was asked and never quote
    what was written, and no expression can tell one from the other. What
    is checkable is checked — a cap, no address, no attachment name, no
    link — so that the failure a model is most likely to produce is the
    one that never reaches an approval surface.
    """
    if not context.strip():
        raise EnvelopeError("a context that says nothing is left out, not sent empty")
    if len(context) > MAX_CONTEXT_CHARS:
        raise EnvelopeError(
            f"context is {len(context)} characters and the contract caps it at "
            f"{MAX_CONTEXT_CHARS}: say what was asked in a sentence or two"
        )
    if _ADDRESS.search(context):
        raise EnvelopeError(
            "context must carry no address or link: an approval surface shows it, "
            "and one of them is somebody else's relay"
        )
    if _FILENAME.search(context):
        raise EnvelopeError(
            "context must name no attachment: say that there are attachments, "
            "never what they are called"
        )


def sha256_hex(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def deterministic_id(*parts: Any) -> str:
    """The contract's id scheme: sha256 of the natural key's parts joined by
    ``:``, lowercase hex."""
    return sha256_hex(":".join(str(part) for part in parts))


def thinking_id(persona_id: str, trigger_event_id: str) -> str:
    """``sha256(persona_id:trigger_event_id)``.

    A persona starts processing a given trigger once, so a retry recomputes
    the same id and the bus deduplicates it.
    """
    return deterministic_id(persona_id, trigger_event_id)


def suggest_id(persona_id: str, trigger_event_id: str, attempt: int) -> str:
    """``sha256(persona_id:trigger_event_id:attempt)``.

    Several suggestions may exist for one trigger; each attempt keeps its
    own id while staying replay-safe.
    """
    return deterministic_id(persona_id, trigger_event_id, attempt)


def dataschema_of(event_type: str) -> str:
    """The URI of the schema an event type validates against."""
    name = event_type.removeprefix("fr.linagora.twalk.").rsplit(".", 1)[0]
    return f"{SCHEMA_BASE}/{name}.schema.json"


def utc_now() -> datetime:
    """The current instant, in UTC.

    One function, because a suggestion's ``time`` and its ``expires_at``
    have to be two views of the *same* instant: computed from two calls to
    the clock, the window the operator configured would be off by however
    long the lines between them took.
    """
    return datetime.now(timezone.utc)


def rfc3339(moment: datetime) -> str:
    """An instant as the contract's ``date-time``, to the second, in UTC."""
    return moment.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def now_rfc3339() -> str:
    """The current time as the contract's ``date-time``, to the second."""
    return rfc3339(utc_now())


def cap_chars(value: str, max_chars: int) -> str:
    return value if len(value) <= max_chars else value[:max_chars]


def _base_event(
    *,
    event_type: str,
    event_id: str,
    source: str,
    trigger: Trigger,
    time: Optional[str],
) -> Dict[str, Any]:
    """The half of the envelope every ``persona.*`` event shares.

    ``network`` and ``consent`` are copied from the trigger — they describe
    the message being reasoned about, not a state the persona decides — and
    the trace is continued from it, so that one message's trace links
    sensor to persona to approval to outbound.
    """
    if not trigger.event_id:
        raise EnvelopeError("the trigger event has no id to key this event on")
    if not trigger.network or not trigger.consent:
        raise EnvelopeError(
            "a message-flow event must carry the trigger's network and "
            f"consent extensions (network={trigger.network!r}, "
            f"consent={trigger.consent!r})"
        )
    if not trigger.connection:
        raise EnvelopeError(
            "a message-flow event must carry the trigger's connection (ADR 0033): "
            "the trigger names none, and a persona never derives one"
        )
    event: Dict[str, Any] = {
        "specversion": SPEC_VERSION,
        "id": event_id,
        "source": source,
        "type": event_type,
        "time": time or now_rfc3339(),
        "subject": trigger.event_id,
        "datacontenttype": DATA_CONTENT_TYPE,
        "dataschema": dataschema_of(event_type),
    }
    if trigger.traceparent:
        event["traceparent"] = trigger.traceparent
    event["network"] = trigger.network
    # Copied, never derived (ADR 0033): the perimeter the trigger arrived on
    # is the perimeter the persona's answer belongs to.
    event["connection"] = trigger.connection
    event["consent"] = trigger.consent
    return event


def _trigger_reference(trigger: Trigger) -> Dict[str, str]:
    return {"event_id": trigger.event_id, "event_type": trigger.event_type}


def thinking_event(
    *,
    persona_id: str,
    source: str,
    trigger: Trigger,
    model: Optional[str] = None,
    time: Optional[str] = None,
) -> Dict[str, Any]:
    """A ``persona.thinking.emitted`` envelope for a trigger."""
    event = _base_event(
        event_type=THINKING_TYPE,
        event_id=thinking_id(persona_id, trigger.event_id),
        source=source,
        trigger=trigger,
        time=time,
    )
    data: Dict[str, Any] = {
        "persona_id": persona_id,
        "trigger": _trigger_reference(trigger),
    }
    if model:
        data["model"] = model
    event["data"] = data
    return event


def suggest_event(
    *,
    persona_id: str,
    source: str,
    trigger: Trigger,
    suggestion: Suggestion,
    attempt: int = FIRST_ATTEMPT,
    time: Optional[str] = None,
    expires_at: Optional[str] = None,
) -> Dict[str, Any]:
    """A ``persona.suggest.produced`` envelope for a trigger.

    ``expires_at`` is the suggestion policy's answer, not this function's:
    when a suggestion goes stale is a decision (:mod:`twalk_sdk.policy`,
    which the process loop reads from the operator's configuration), while
    this module only knows how to write it down. Left out, the envelope
    carries none — which the contract allows and the SDK's own loop never
    does.
    """
    if attempt < 1:
        raise EnvelopeError(f"attempt starts at 1, got {attempt}")
    event = _base_event(
        event_type=SUGGEST_TYPE,
        event_id=suggest_id(persona_id, trigger.event_id, attempt),
        source=source,
        trigger=trigger,
        time=time,
    )
    data: Dict[str, Any] = {
        "persona_id": persona_id,
        "trigger": _trigger_reference(trigger),
        "suggestion": {
            "body": cap_chars(suggestion.body, MAX_BODY_CHARS),
            "format": suggestion.format,
        },
        "attempt": attempt,
    }
    if suggestion.context:
        # Who is answered and what they asked, written by the one component
        # allowed to read the message, at the moment it reads it (#334,
        # resolving #160). The display name comes off the trigger's own
        # envelope rather than from the model: the persona summarises, it
        # does not name.
        context: Dict[str, Any] = {"summary": suggestion.context}
        contact = trigger.sender_display_name
        if contact:
            context["contact"] = cap_chars(contact, MAX_CONTACT_CHARS)
        data["context"] = context
    if suggestion.confidence is not None:
        data["confidence"] = suggestion.confidence
    if suggestion.rationale:
        data["rationale"] = cap_chars(suggestion.rationale, MAX_RATIONALE_CHARS)
    if expires_at:
        data["expires_at"] = expires_at
    if suggestion.disclosure:
        # A field of its own, never inside the body (ADR 0031): the body is
        # the user's to edit and the sentence is not. Omitted when the loop
        # set none, which the schema allows and the SDK's own loop never
        # does — a suggestion with no sentence is one it refused to publish.
        data["disclosure"] = suggestion.disclosure
    event["data"] = data
    return event


def nats_headers(event: Dict[str, Any]) -> Dict[str, str]:
    """The headers a ``persona.*`` event publishes with.

    ``Nats-Msg-Id`` is the deduplication anchor — the deterministic id, so a
    replay collapses on the bus instead of showing the user the same
    suggestion twice. ``network``, ``consent`` and ``traceparent`` are
    duplicated from the envelope because JetStream filters subjects and
    headers, not payloads: a consumer must be able to select on them
    without deserializing the event.
    """
    headers = {
        "Nats-Msg-Id": event["id"],
        "network": event["network"],
        "connection": event["connection"],
        "consent": event["consent"],
    }
    traceparent = event.get("traceparent")
    if traceparent:
        headers["traceparent"] = traceparent
    return headers
