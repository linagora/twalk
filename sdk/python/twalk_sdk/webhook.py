"""The seam to Hermes, decided: what crosses, and how it is signed.

Pure and stdlib-only, like :mod:`twalk_sdk.completion` and for the same
reason — this is the part of the seam that must be tested field by field
without a network, because what it *leaves out* is the property ADR 0032
turns on.

Three decisions live here.

**A narrow template of named fields, never the raw event.** Hermes keeps
memories: whatever it is shown may be copied into ``MEMORY.md``, which
nothing expires, so a raw event crossing this seam would make ADR 0028's
seven days meaningless on another machine's disk. :func:`webhook_body`
therefore builds a fixed, small dictionary out of an
:class:`~twalk_sdk.trigger.InboundMessage` and nothing else — a persona
author cannot add a field to it, which is the same reason the consent gate
lives in the SDK rather than in a persona's handler. What it names is the
message's *words* and the shape of the conversation around them; what it
withholds is everybody's **identity**: the sender's Matrix ID, their
display name, the network's own identifier for them (a phone number), the
portal room, the quoted excerpt that belongs to whoever wrote it
(ADR 0012), and an attachment's name or decryption material. A reply can be
drafted without any of those. ``tests/test_webhook.py`` asserts the whole
key set and then searches the serialised body for each withheld value, so
that adding a field is a deliberate act with a failing test in front of it.

**TLS by configuration, not by coincidence.** The HMAC authenticates the
sender and not the content — Nous Research's own documentation says so — so
it provides no confidentiality whatever, and the two machines are one
machine only today. An ``http://`` URL is refused at startup by default;
an operator who really is on one loopback interface says so by name
(:data:`ALLOW_INSECURE_URL_VARIABLE`) and gets a warning on every start.

**Idempotency keyed on the suggestion, not on the delivery.** Hermes caches
a delivery id for an hour and skips a repeat (its webhook adapter's
documented behaviour), and the bus deduplicates on ``Nats-Msg-Id``. Those
two must agree or a redelivered trigger costs a second agent run and offers
the user a second draft of one message. So the delivery id *is* the
deterministic id of the suggestion this wake will produce —
``sha256(persona_id:trigger_event_id:attempt)``, the contract's own natural
key (:func:`twalk_sdk.envelope.suggest_id`) — which is the same discipline
:mod:`twalk_sdk.policy` already applies to the bus, applied to the seam.
"""

from __future__ import annotations

import hashlib
import hmac
import json
from dataclasses import dataclass
from typing import Any, Dict, Mapping, Optional
from urllib.parse import urlsplit, urlunsplit

from .envelope import FIRST_ATTEMPT, suggest_id
from .trigger import InboundMessage

WEBHOOK_URL_VARIABLE = "TWALK_HERMES_WEBHOOK_URL"
WEBHOOK_SECRET_VARIABLE = "TWALK_HERMES_WEBHOOK_SECRET"
TIMEOUT_VARIABLE = "TWALK_HERMES_TIMEOUT_SECONDS"
ALLOW_INSECURE_URL_VARIABLE = "TWALK_HERMES_ALLOW_INSECURE_URL"

#: The ``event_type`` every wake carries, so that Hermes's route can filter
#: on it (its webhook adapter reads the event type from ``event_type`` in the
#: body for a generic sender) and so that a route which later serves a second
#: kind of wake can tell them apart without guessing from the fields.
MESSAGE_RECEIVED_EVENT = "twalk.message.received"

#: The template's own version, in the body. Hermes's route is configured on
#: the Hermes host, which Twalk may not own (ADR 0032), so the two sides are
#: deployed separately and a prompt template written for version 1 must be
#: able to *see* that it is being handed version 2.
TEMPLATE_VERSION = 1

#: The token that brings the answer home. It is deliberately a single opaque
#: string rather than three fields: it travels through Hermes's *prompt*, is
#: echoed back inside the text of the run's user message, and the Companion
#: Gateway finds it with one pattern. Three fields scattered through a prompt
#: would be three chances for a model to reformat one of them.
REFERENCE_PREFIX = "TWALK-REF"

#: How long a wake waits for Hermes to accept it. Short: the adapter answers
#: ``202 Accepted`` before the agent runs, so this measures Hermes's
#: front door and never its reasoning.
DEFAULT_TIMEOUT_SECONDS = 10.0

#: Hermes's webhook adapter serves its own liveness at ``/health`` on the
#: same origin, which is what makes "Hermes is not there" a fact a persona
#: can state at startup rather than discover on the first message.
HEALTH_PATH = "/health"

#: Hermes's webhook routes live under this path. Checked at startup so that a
#: URL pointing at the origin, or at the adapter's health check, is refused
#: with the reason rather than producing a 404 per message for ever.
ROUTE_PATH_PREFIX = "/webhooks/"


class SeamError(ValueError):
    """The environment does not describe a usable seam to Hermes."""


@dataclass(frozen=True)
class HermesSeam:
    """Where Hermes is, and the secret that authenticates this sender to it.

    Configuration rather than a persona's choice, and injected by the
    persona runtime like the model endpoint is (ADR 0015): a persona never
    fetches it, because the credential that would read it from the Companion
    Gateway is the credential that opens the consent snapshot.
    """

    url: str
    secret: str
    timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS
    #: Set only by an operator who has decided that this hop is inside one
    #: host. It does not make plaintext safe; it records that somebody
    #: accepted it, and :mod:`twalk_sdk.hermes` says so on every start.
    allow_insecure_url: bool = False

    def __post_init__(self) -> None:
        parts = urlsplit(self.url)
        if parts.scheme not in ("http", "https") or not parts.netloc:
            raise SeamError(
                f"{WEBHOOK_URL_VARIABLE} must be an absolute http(s) URL of one "
                f"of Hermes's webhook routes, got {self.url!r}"
            )
        if not parts.path.startswith(ROUTE_PATH_PREFIX) or len(
            parts.path
        ) <= len(ROUTE_PATH_PREFIX):
            raise SeamError(
                f"{WEBHOOK_URL_VARIABLE} must name a route — Hermes serves them "
                f"under {ROUTE_PATH_PREFIX}<route> — got the path "
                f"{parts.path!r}. The origin alone, or /health, answers no "
                f"webhook."
            )
        if parts.scheme != "https" and not self.allow_insecure_url:
            raise SeamError(
                f"{WEBHOOK_URL_VARIABLE} is {self.url!r}, which is plaintext. "
                f"The HMAC signature authenticates this sender and not the "
                f"content, so it gives the message no confidentiality at all, "
                f"and the user's messages cross this hop (ADR 0032). Use https, "
                f"or set {ALLOW_INSECURE_URL_VARIABLE}=true to record that this "
                f"hop is inside one host."
            )
        if not self.secret:
            raise SeamError(
                f"{WEBHOOK_SECRET_VARIABLE} is required: Hermes refuses a route "
                f"with no secret, and its INSECURE_NO_AUTH escape hatch is for "
                f"its own tests and never for Twalk's configuration."
            )
        if self.timeout_seconds <= 0:
            raise SeamError(
                f"{TIMEOUT_VARIABLE} must be a positive number of seconds, got "
                f"{self.timeout_seconds!r}"
            )

    @property
    def route(self) -> str:
        """The route's name, as Hermes knows it. For the logs: an operator
        reading ``route=twalk-messages`` can find it in Hermes's config."""
        return urlsplit(self.url).path[len(ROUTE_PATH_PREFIX) :]

    @property
    def health_url(self) -> str:
        """The adapter's own liveness, on the same origin as the route."""
        parts = urlsplit(self.url)
        return urlunsplit((parts.scheme, parts.netloc, HEALTH_PATH, "", ""))

    @property
    def is_plaintext(self) -> bool:
        return urlsplit(self.url).scheme != "https"

    @classmethod
    def from_env(cls, env: Mapping[str, str]) -> Optional["HermesSeam"]:
        """The seam as the environment describes it, or ``None``.

        ``None`` is a supported state and the one every deployment before
        this ticket is in: a persona with no seam configured reasons with the
        model endpoint it was given and never speaks to anything outside the
        deployment. The URL is what turns the seam on; everything else is
        then required, because a half-configured seam that silently does
        nothing is the failure this project keeps removing.
        """
        url = (env.get(WEBHOOK_URL_VARIABLE) or "").strip()
        if not url:
            return None
        timeout_raw = (env.get(TIMEOUT_VARIABLE) or "").strip()
        try:
            timeout = float(timeout_raw) if timeout_raw else DEFAULT_TIMEOUT_SECONDS
        except ValueError as error:
            raise SeamError(
                f"{TIMEOUT_VARIABLE} must be a number of seconds, got "
                f"{timeout_raw!r}"
            ) from error
        return cls(
            url=url,
            secret=(env.get(WEBHOOK_SECRET_VARIABLE) or "").strip(),
            timeout_seconds=timeout,
            allow_insecure_url=_is_true(env.get(ALLOW_INSECURE_URL_VARIABLE)),
        )


def _is_true(value: Optional[str]) -> bool:
    return (value or "").strip().lower() in ("1", "true", "yes", "on")


def reference(persona_id: str, trigger_event_id: str, attempt: int) -> str:
    """The token the Companion Gateway reads the answer's provenance from.

    ``TWALK-REF:<persona_id>:<trigger event id>:<attempt>`` — three facts
    the Gateway needs to build the suggestion envelope the contract
    describes, and no fact about a person. It is not a capability: the
    Gateway validates every part of it against its own journal and the bus
    before it publishes anything, so a wrong reference produces a refusal
    and never a suggestion about somebody else's message.
    """
    return f"{REFERENCE_PREFIX}:{persona_id}:{trigger_event_id}:{attempt}"


def delivery_id(persona_id: str, trigger_event_id: str, attempt: int) -> str:
    """Hermes's idempotency key for this wake: the suggestion's own id.

    See the module docstring — the bus and Hermes must deduplicate on one
    key or a redelivery is billed twice and drafted twice.
    """
    return suggest_id(persona_id, trigger_event_id, attempt)


def webhook_body(
    *,
    trigger: InboundMessage,
    persona_id: str,
    attempt: int = FIRST_ATTEMPT,
    user_language: Optional[str] = None,
) -> Dict[str, Any]:
    """The narrow template, and the only thing that crosses the seam.

    Every value is either the message's own words, the shape of the
    conversation around it, or a reference Twalk gave itself. Nothing here
    identifies the contact, the room, or the deployment's other
    conversations — see the module docstring for why each absence is
    deliberate, and ``tests/test_webhook.py`` for the assertion that keeps
    them absent.
    """
    return {
        "event_type": MESSAGE_RECEIVED_EVENT,
        "template_version": TEMPLATE_VERSION,
        "reference": reference(persona_id, trigger.event_id, attempt),
        # The *kind* of connection, never the connection (ADR 0033): a
        # persona's tone differs between an SMS and a Telegram message, and
        # which account it arrived on is nobody's business outside the
        # deployment.
        "network": trigger.network,
        "received_at": trigger.event.get("time"),
        "message": trigger.body or "",
        "format": trigger.text_format,
        # Two booleans rather than the things themselves. A quoted excerpt
        # belongs to whoever wrote it (ADR 0012) and an attachment's name is
        # a filename somebody chose, so neither crosses — but a model that
        # does not know it is missing context invents it, which is worse
        # than a model told there is context it cannot see.
        "quotes_an_earlier_message": trigger.is_reply,
        "has_attachments": bool(trigger.attachments),
        # ADR 0016's fallback, so that Hermes can obey it: the language to
        # write in *only* when the message is too short to tell. ``None``
        # when the user has set no preference, which is a state and not a
        # default.
        "user_language": user_language,
    }


def signed_headers(
    body: bytes,
    *,
    secret: str,
    timestamp: int,
    delivery: str,
) -> Dict[str, str]:
    """The headers Hermes's generic V2 signature check expects.

    ``X-Webhook-Signature-V2`` is the hex HMAC-SHA256 of
    ``<timestamp>.<body>`` and ``X-Webhook-Timestamp`` is unix seconds,
    which the adapter requires to be within five minutes of its own clock.
    V2 rather than V1 deliberately: V1 signs the body alone, so a captured
    request replays for ever, and Hermes logs a deprecation warning for it.

    ``X-Request-ID`` is the idempotency key (the adapter reads
    ``X-GitHub-Delivery`` first, then this, then falls back to a timestamp —
    and a timestamp fallback would mean no idempotency at all).
    """
    signed = f"{timestamp}.".encode("utf-8") + body
    digest = hmac.new(secret.encode("utf-8"), signed, hashlib.sha256).hexdigest()
    return {
        "Content-Type": "application/json",
        "X-Webhook-Timestamp": str(timestamp),
        "X-Webhook-Signature-V2": digest,
        "X-Request-ID": delivery,
    }


def encode_body(body: Mapping[str, Any]) -> bytes:
    """The bytes that are signed and sent.

    One function because the signature covers exactly these bytes: a body
    serialised twice with different separators signs one thing and sends
    another, which Hermes reports as an invalid signature and no amount of
    reading the secret explains.
    """
    return json.dumps(body, ensure_ascii=False, sort_keys=True).encode("utf-8")
