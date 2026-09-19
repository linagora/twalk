"""Waking Hermes: the transport half of the seam.

:mod:`twalk_sdk.webhook` decides *what* crosses and how it is signed, and is
pure. This file owns the socket and nothing else, the same split
:mod:`twalk_sdk.completion` and :mod:`twalk_sdk.llm` already use.

Two behaviours in here are decisions rather than plumbing.

**Hermes being unreachable does not stop the deployment.** This is the
answer the Sensor already gives to the identical question about the
Companion Gateway's consent snapshot, and it is copied rather than
reinvented: probe at startup, and when there is no answer, log at ``ERROR``
naming the URL *and what happens instead*, start anyway, and re-probe in the
background until it comes back. A persona that refused to start would take
a whole deployment down because a machine Twalk may not own is rebooting,
and ADR 0013's "allowed to exist, handed nothing" is the shape this project
has already chosen for that.

**A failure that will not change is not retried.** Four of the answers
Hermes's webhook adapter gives mean the configuration is wrong, not that
the moment was: an invalid signature (the secret), an unknown route (the
URL), a malformed body and a body over the size limit. Retrying those bills
nothing but they do fill a log with the same line for ever and leave the
trigger apparently unprocessed; so they are marked non-transient and
:mod:`twalk_sdk.persona` terminates the delivery with the remedy in the
message, exactly as it does for a model that spent its budget reasoning
(issue #162). A refused connection, a timeout, a ``429`` and a ``5xx`` are
the moment and are retried.

The answer itself never comes back this way. The adapter returns ``202
Accepted`` before the agent has run — it is asynchronous by construction —
and its ``deliver`` targets are chat platforms, so Hermes's answer reaches
the deployment through the Companion Gateway's own endpoint (ADR 0032) and
not through this response. What this call gets back is whether Hermes
accepted the wake, which is worth having as three distinct facts:
accepted, already seen (idempotency), and ignored by the route's own
filters.
"""

from __future__ import annotations

import asyncio
import logging
import time
from dataclasses import dataclass
from typing import Any, Optional

import httpx

from .envelope import FIRST_ATTEMPT
from .trigger import InboundMessage
from .webhook import (
    ALLOW_INSECURE_URL_VARIABLE,
    WEBHOOK_URL_VARIABLE,
    HermesSeam,
    delivery_id,
    encode_body,
    reference,
    signed_headers,
    webhook_body,
)

logger = logging.getLogger("twalk_sdk")

#: How long the background probe waits between attempts while Hermes is
#: away, and the ceiling it backs off to. The point is to end an outage
#: without turning a reboot into a log flood.
PROBE_DELAY_SECONDS = 5.0
PROBE_MAX_DELAY_SECONDS = 60.0

#: The statuses that mean "this request is wrong", not "this moment is".
#: 401 the secret, 404 the route, 400 the body, 413 its size — every one of
#: them a configuration fact that a redelivery reproduces exactly.
NON_TRANSIENT_STATUSES = frozenset({400, 401, 403, 404, 413})

#: How long to wait after Hermes says a route is over its rate limit, when it
#: names no ``Retry-After``.
#:
#: Longer than the loop's ordinary retry, and the difference is the point. A
#: route is rate-limited per minute (30 by default, fixed-window), and a
#: persona activated today reads the bus from the beginning (ADR 0013), so its
#: first minutes are a replay of every message the deployment ever saw. At the
#: ordinary five seconds that replay spends the bus's whole redelivery budget
#: inside one window and the triggers are terminated — a message loses its
#: suggestion because the *previous* messages were busy. Waiting out the
#: window instead costs a minute and loses nothing.
RATE_LIMIT_DELAY_SECONDS = 60.0


class HermesError(RuntimeError):
    """Hermes did not accept the wake.

    ``transient`` is read by :mod:`twalk_sdk.persona`: ``False`` terminates
    the delivery instead of letting the bus redeliver a request whose answer
    cannot change.
    """

    transient = True

    def __init__(self, message: str, *, transient: Optional[bool] = None) -> None:
        super().__init__(message)
        if transient is not None:
            self.transient = transient


class HermesUnreachable(HermesError):
    """Nothing answered at the URL. The moment, not the configuration."""

    transient = True


class HermesRefused(HermesError):
    """Hermes answered, and said no."""


class HermesRateLimited(HermesError):
    """The route is over its requests-per-minute limit.

    Transient, and with a delay of its own: see
    :data:`RATE_LIMIT_DELAY_SECONDS`.
    """

    transient = True

    def __init__(self, message: str, retry_after: float) -> None:
        super().__init__(message)
        self.retry_after = retry_after


@dataclass(frozen=True)
class HandedToHermes:
    """What a persona returns when it woke Hermes instead of drafting itself.

    A third outcome beside a :class:`~twalk_sdk.envelope.Suggestion` and
    ``None``, and it needs to exist: "I have nothing to suggest" and "the
    suggestion is coming from somewhere else, through the Companion
    Gateway" are different facts, and a loop that logged them the same way
    would make an answer that never arrived indistinguishable from a message
    the persona chose to ignore.
    """

    #: The token the Gateway will read the answer's provenance from.
    reference: str
    #: Hermes's idempotency key for this wake, which is also the
    #: deterministic id of the suggestion it will produce.
    delivery: str
    #: ``True`` when Hermes had already seen this delivery id and did not run
    #: the agent again — a redelivered trigger, absorbed.
    already_seen: bool = False
    #: ``True`` when the route's own filters or script dropped it before any
    #: model was called. Not a failure: it is the operator's filter doing
    #: what it was configured to do.
    ignored_by_route: bool = False


class Hermes:
    """Hermes, as a persona reaches it: one signed POST per message."""

    def __init__(
        self, seam: HermesSeam, client: Optional[httpx.AsyncClient] = None
    ) -> None:
        self._seam = seam
        self._client = client or httpx.AsyncClient(timeout=seam.timeout_seconds)
        self._reachable: Optional[bool] = None
        self._probe: Optional[asyncio.Task] = None

    @property
    def seam(self) -> HermesSeam:
        return self._seam

    def say_what_it_is(self) -> None:
        """States the seam at startup, including the part somebody accepted.

        A plaintext hop is a decision an operator made, and a decision made
        once is a decision nobody remembers: it is named on every start, at
        ``WARNING``, with the variable that turns it off.
        """
        logger.info(
            "hermes seam configured url=%s route=%s timeout=%ss",
            self._seam.url,
            self._seam.route,
            self._seam.timeout_seconds,
        )
        if self._seam.is_plaintext:
            logger.warning(
                "the seam to Hermes is plaintext (%s=%s, allowed by %s): the "
                "HMAC authenticates this sender and not the content, so the "
                "user's messages cross this hop with no confidentiality. That "
                "is only acceptable while both ends are one host (ADR 0032).",
                WEBHOOK_URL_VARIABLE,
                self._seam.url,
                ALLOW_INSECURE_URL_VARIABLE,
            )

    async def aclose(self) -> None:
        if self._probe is not None:
            self._probe.cancel()
            try:
                await self._probe
            except (asyncio.CancelledError, Exception):  # pragma: no cover
                pass
        await self._client.aclose()

    async def probe(self) -> bool:
        """Whether Hermes's webhook adapter answers its own liveness check."""
        try:
            answer = await self._client.get(self._seam.health_url)
        except httpx.HTTPError:
            return False
        return answer.status_code == 200

    async def announce_reachability(self) -> bool:
        """Probes once at startup and says what it found.

        Returns whether Hermes answered. A persona does not refuse to start
        on ``False`` — see the module docstring — but the line naming the URL
        is the whole difference between "Hermes does not answer" and "Hermes
        was never configured", which is ADR 0024's rule applied to the one
        component that is not ours.
        """
        reachable = await self.probe()
        self._reachable = reachable
        if reachable:
            logger.info("hermes answered at %s", self._seam.health_url)
            return True
        logger.error(
            "hermes does not answer at %s: this persona is starting anyway and "
            "will retry in the background. Until it answers, every message the "
            "consent gate lets through is redelivered by the bus rather than "
            "drafted, and no suggestion reaches the approval screen.",
            self._seam.health_url,
        )
        return False

    def retry_in_the_background(self) -> None:
        """Starts the loop that ends an outage.

        Started only when the startup probe failed, and it stops itself the
        moment Hermes answers: the retry exists to end an outage and never to
        poll a healthy neighbour.
        """
        if self._reachable or self._probe is not None:
            return
        self._probe = asyncio.get_running_loop().create_task(self._probe_loop())

    async def _probe_loop(self) -> None:
        delay = PROBE_DELAY_SECONDS
        while True:
            await asyncio.sleep(delay)
            if await self.probe():
                self._reachable = True
                logger.info(
                    "hermes answered at %s — the seam is open again",
                    self._seam.health_url,
                )
                self._probe = None
                return
            delay = min(delay * 2, PROBE_MAX_DELAY_SECONDS)

    async def wake(
        self,
        trigger: InboundMessage,
        *,
        persona_id: str,
        attempt: int = FIRST_ATTEMPT,
        user_language: Optional[str] = None,
    ) -> HandedToHermes:
        """Posts one signed wake and returns what Hermes did with it.

        Raises :class:`HermesError` when Hermes did not accept it. The
        message a persona logs for that failure names the URL, because the
        one thing an operator needs is which machine is not answering.
        """
        body = encode_body(
            webhook_body(
                trigger=trigger,
                persona_id=persona_id,
                attempt=attempt,
                user_language=user_language,
            )
        )
        delivery = delivery_id(persona_id, trigger.event_id, attempt)
        headers = signed_headers(
            body,
            secret=self._seam.secret,
            timestamp=int(time.time()),
            delivery=delivery,
        )
        try:
            answer = await self._client.post(
                self._seam.url, content=body, headers=headers
            )
        except httpx.HTTPError as error:
            self._reachable = False
            self.retry_in_the_background()
            raise HermesUnreachable(
                f"hermes did not answer at {self._seam.url}: {error}"
            ) from error

        if answer.status_code == 429:
            raise HermesRateLimited(
                f"hermes refused the wake on {self._seam.url} as over the route's "
                f"rate limit (429). Its default is 30 requests a minute per "
                f"route; a persona reading the bus from the beginning sends more "
                f"than that. Raise `rate_limit` on the route, or accept that a "
                f"backlog drains a window at a time.",
                _retry_after(answer),
            )
        if answer.status_code in NON_TRANSIENT_STATUSES:
            raise HermesRefused(
                _remedy(answer.status_code, self._seam), transient=False
            )
        if answer.status_code >= 400:
            raise HermesRefused(
                f"hermes answered {answer.status_code} at {self._seam.url}: "
                f"{answer.text[:200]!r}"
            )

        self._reachable = True
        status = _status_of(answer)
        return HandedToHermes(
            reference=reference(persona_id, trigger.event_id, attempt),
            delivery=delivery,
            already_seen=status == "duplicate",
            ignored_by_route=status == "ignored",
        )


def _retry_after(answer: httpx.Response) -> float:
    """How long the route says to wait, or this module's own answer.

    ``Retry-After`` in seconds is what a rate limiter conventionally sends;
    the adapter this seam speaks to sends none today, which is exactly why
    there is a default rather than a wait of zero.
    """
    raw = answer.headers.get("Retry-After", "").strip()
    try:
        return max(1.0, float(raw))
    except ValueError:
        return RATE_LIMIT_DELAY_SECONDS


def _status_of(answer: httpx.Response) -> str:
    """The ``status`` member of the adapter's answer, or ``""``.

    ``accepted``, ``duplicate`` and ``ignored`` are three different facts and
    all three are a ``200``-family status, so the body is the only place they
    are distinguishable. A body that is not the JSON the route's contract
    describes is read as no status rather than as a failure: the wake was
    accepted either way.
    """
    try:
        payload: Any = answer.json()
    except ValueError:
        return ""
    if not isinstance(payload, dict):
        return ""
    value = payload.get("status")
    return value if isinstance(value, str) else ""


def _remedy(status: int, seam: HermesSeam) -> str:
    """What to change, for each answer that a redelivery would reproduce."""
    if status in (401, 403):
        return (
            f"hermes refused the signature on {seam.url} ({status}): the secret "
            f"this persona signs with is not the one route {seam.route!r} is "
            f"configured with. Twalk's side is TWALK_HERMES_WEBHOOK_SECRET, set "
            f"on the persona runtime; Hermes's side is the route's own "
            f"'secret'. No retry: the same request gets the same answer."
        )
    if status == 404:
        return (
            f"hermes has no route at {seam.url} (404): route {seam.route!r} is "
            f"not in its config.yaml and not in its webhook_subscriptions.json. "
            f"No retry."
        )
    if status == 413:
        return (
            f"hermes refused the body as too large on {seam.url} (413): its "
            f"route limit is 1 MB by default. No retry — the same message "
            f"produces the same body."
        )
    return (
        f"hermes refused the request on {seam.url} ({status}): the body is not "
        f"the JSON the route expects. No retry."
    )
