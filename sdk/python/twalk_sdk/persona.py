"""The persona process: subscribe, gate, think, suggest.

This is the loop every persona runs, so that a persona author writes only
the part that is theirs — what to suggest — and gets the rest by
construction:

1. a **durable** pull consumer on ``inbound.message.received``, so a
   restart resumes where it stopped instead of losing events;
2. the **consent gate** (:mod:`twalk_sdk.consent`), applied before the
   author's handler is called and before the LLM client is ever touched: a
   ``pending`` or ``revoked`` event produces no event and no completion
   request, and no persona code runs on it;
3. ``persona.thinking.emitted`` the moment processing starts, so oversight
   can show activity in real time;
4. ``persona.suggest.produced`` from whatever the handler returns, with the
   contract's deterministic id, ``Nats-Msg-Id`` set, and the trigger's
   ``network``, ``consent`` and trace carried through.

The persona talks to the bus itself (ADR 0008): the Hermes runtime starts
it as a process and supervises it, but never sits between it and the bus.

What a persona author writes:

```python
from twalk_sdk import Persona, Suggestion, system, user

persona = Persona.from_env()


@persona.on_inbound_message
async def draft(message, context):
    if not message.body:
        return None
    reply = await context.llm.complete(
        [system("Draft a reply."), user(message.body)]
    )
    return Suggestion(body=reply)


if __name__ == "__main__":
    persona.run()
```
"""

from __future__ import annotations

import asyncio
import json
import logging
import signal
from dataclasses import dataclass
from typing import Any, Awaitable, Callable, Dict, Optional

import nats
import nats.errors
import nats.js.errors
from nats.aio.msg import Msg
from nats.js import JetStreamContext
from nats.js.api import AckPolicy, ConsumerConfig, DeliverPolicy

from .config import Config
from .consent import GRANTED, consent_of, is_granted
from .envelope import FIRST_ATTEMPT, Suggestion, nats_headers, suggest_event, thinking_event
from .llm import Llm
from .trigger import MESSAGE_RECEIVED_TYPE, InboundMessage

logger = logging.getLogger("twalk_sdk")

#: How long one pull waits for an event before the loop checks whether it
#: was asked to stop. Short enough that SIGTERM is honoured promptly, long
#: enough that an idle persona is not a busy loop.
FETCH_TIMEOUT_SECONDS = 2.0

#: How long a failed event waits before JetStream redelivers it. A failure
#: here is usually the LLM endpoint being briefly unavailable, so the retry
#: is worth having — and bounded by ``MAX_DELIVER``, because a message that
#: fails deterministically must not be retried forever.
RETRY_DELAY_SECONDS = 5
MAX_DELIVER = 3

#: How long a persona waits for the bus and its stream to exist before
#: giving up. A persona starts beside NATS in a compose deployment, so it
#: may well come up first.
STARTUP_ATTEMPTS = 60
STARTUP_DELAY_SECONDS = 1.0


@dataclass
class Context:
    """What the SDK lends a persona for the duration of one event."""

    config: Config
    llm: Llm
    logger: logging.Logger = logger


Handler = Callable[[InboundMessage, Context], Awaitable[Optional[Suggestion]]]


class Persona:
    """A persona process, configured and ready to run."""

    def __init__(self, config: Config, llm: Optional[Llm] = None) -> None:
        self.config = config
        self.llm = llm or Llm(config.llm)
        self._handler: Optional[Handler] = None
        self._stop = asyncio.Event()

    @classmethod
    def from_env(cls) -> "Persona":
        return cls(Config.from_env())

    def on_inbound_message(self, handler: Handler) -> Handler:
        """Registers the persona's handler for inbound messages.

        Called with events whose consent is ``granted`` and nothing else.
        Returning a :class:`Suggestion` publishes it; returning ``None``
        means "nothing to suggest here" and is the right answer for a
        message this persona has no business replying to.
        """
        if self._handler is not None:
            raise RuntimeError("a persona registers one inbound-message handler")
        self._handler = handler
        return handler

    def run(self) -> None:
        """Runs the persona until it is asked to stop. The process entry
        point."""
        logging.basicConfig(
            level=self.config.log_level.upper(),
            format="%(asctime)s %(levelname)s %(name)s %(message)s",
        )
        asyncio.run(self.serve())

    async def serve(self) -> None:
        if self._handler is None:
            raise RuntimeError(
                "this persona registered no handler: decorate one with "
                "@persona.on_inbound_message"
            )
        # Re-created here: the event must belong to the loop that runs.
        self._stop = asyncio.Event()
        self._install_signal_handlers()

        logger.info(
            "persona starting persona_id=%s source=%s nats=%s stream=%s subject_prefix=%s "
            "consumer=%s model=%s",
            self.config.persona_id,
            self.config.source,
            self.config.nats_url,
            self.config.stream,
            self.config.subject_prefix,
            self.config.durable_name,
            self.config.llm.model,
        )
        connection = await self._connect()
        jetstream = connection.jetstream()
        await self._await_stream(jetstream)
        subscription = await self._subscribe(jetstream)
        logger.info(
            "persona ready subject=%s", self.config.subject(MESSAGE_RECEIVED_TYPE)
        )
        try:
            await self._consume(jetstream, subscription)
        finally:
            await self.llm.aclose()
            await connection.drain()
            logger.info("persona stopped persona_id=%s", self.config.persona_id)

    def stop(self) -> None:
        """Asks the loop to finish the event in flight and return."""
        self._stop.set()

    def _install_signal_handlers(self) -> None:
        """SIGTERM is how a container is asked to stop; an event in flight
        is finished and acked before the process exits, so nothing is
        processed twice."""
        loop = asyncio.get_running_loop()
        for received in (signal.SIGINT, signal.SIGTERM):
            try:
                loop.add_signal_handler(received, self.stop)
            except (NotImplementedError, RuntimeError):  # pragma: no cover
                pass

    async def _connect(self) -> Any:
        last_error: Optional[Exception] = None
        for attempt in range(STARTUP_ATTEMPTS):
            try:
                return await nats.connect(
                    self.config.nats_url,
                    name=f"twalk-persona-{self.config.persona_id}",
                    max_reconnect_attempts=-1,
                    reconnect_time_wait=1,
                )
            except Exception as error:  # nats raises several unrelated types
                last_error = error
                if attempt == 0:
                    logger.info("waiting for the bus at %s", self.config.nats_url)
                await asyncio.sleep(STARTUP_DELAY_SECONDS)
        raise RuntimeError(
            f"the bus at {self.config.nats_url} never answered: {last_error}"
        )

    async def _await_stream(self, jetstream: JetStreamContext) -> None:
        """Waits for the stream to exist rather than creating it.

        A persona is a consumer, not the bus's operator: the stream belongs
        to the deployment (the Sensor ensures it, and the Hermes runtime
        will), so a persona that created one would risk creating it with
        the wrong subjects.
        """
        for attempt in range(STARTUP_ATTEMPTS):
            try:
                await jetstream.stream_info(self.config.stream)
                return
            except nats.js.errors.NotFoundError:
                if attempt == 0:
                    logger.info("waiting for stream %s", self.config.stream)
                await asyncio.sleep(STARTUP_DELAY_SECONDS)
        raise RuntimeError(f"stream {self.config.stream} does not exist")

    async def _subscribe(
        self, jetstream: JetStreamContext
    ) -> JetStreamContext.PullSubscription:
        subject = self.config.subject(MESSAGE_RECEIVED_TYPE)
        return await jetstream.pull_subscribe(
            subject,
            durable=self.config.durable_name,
            stream=self.config.stream,
            config=ConsumerConfig(
                durable_name=self.config.durable_name,
                filter_subject=subject,
                # Explicit acks: an event is acked once the persona is done
                # with it, so a crash mid-processing redelivers it.
                ack_policy=AckPolicy.EXPLICIT,
                # A fresh consumer starts at the beginning of the stream:
                # the events that arrived before this persona was ever
                # activated are still the user's messages.
                deliver_policy=DeliverPolicy.ALL,
                max_deliver=MAX_DELIVER,
            ),
        )

    async def _consume(
        self,
        jetstream: JetStreamContext,
        subscription: JetStreamContext.PullSubscription,
    ) -> None:
        while not self._stop.is_set():
            try:
                messages = await subscription.fetch(1, timeout=FETCH_TIMEOUT_SECONDS)
            except (nats.errors.TimeoutError, asyncio.TimeoutError):
                continue
            except nats.errors.Error as error:
                logger.warning("pull failed, retrying: %s", error)
                await asyncio.sleep(STARTUP_DELAY_SECONDS)
                continue
            for message in messages:
                await self._handle(jetstream, message)

    async def _handle(self, jetstream: JetStreamContext, message: Msg) -> None:
        try:
            event = json.loads(message.data)
        except (ValueError, UnicodeDecodeError):
            # Not a CloudEvent: redelivery would produce the same failure,
            # so it is terminated rather than retried.
            logger.error("dropped a message that is not JSON, sequence=%s", _sequence(message))
            await message.term()
            return
        if not isinstance(event, dict):
            logger.error("dropped a message that is not a CloudEvents envelope")
            await message.term()
            return

        # THE CONSENT GATE. Before the handler, before the LLM, before
        # anything reads the message: an event a persona may not process is
        # acked and forgotten. Nothing below this line runs for it.
        if not is_granted(event):
            logger.info(
                "dropped an event whose consent is not %s event_id=%s consent=%s",
                GRANTED,
                event.get("id"),
                consent_of(event),
            )
            await message.ack()
            return

        trigger = InboundMessage(event)
        try:
            await self._publish(
                jetstream,
                thinking_event(
                    persona_id=self.config.persona_id,
                    source=self.config.source,
                    trigger=trigger,
                    model=self.llm.model,
                ),
            )
            assert self._handler is not None  # serve() refuses to start without one
            suggestion = await self._handler(
                trigger, Context(config=self.config, llm=self.llm)
            )
            if suggestion is None:
                logger.info(
                    "no suggestion for event_id=%s network=%s",
                    trigger.event_id,
                    trigger.network,
                )
            else:
                await self._publish(
                    jetstream,
                    suggest_event(
                        persona_id=self.config.persona_id,
                        source=self.config.source,
                        trigger=trigger,
                        suggestion=suggestion,
                        attempt=FIRST_ATTEMPT,
                    ),
                )
        except Exception as error:
            # The event is not acked: JetStream redelivers it after a delay
            # (bounded by MAX_DELIVER), because the usual cause is an
            # endpoint that was briefly away.
            logger.error(
                "failed to process event_id=%s, retrying in %ss: %s",
                trigger.event_id,
                RETRY_DELAY_SECONDS,
                error,
            )
            await message.nak(delay=RETRY_DELAY_SECONDS)
            return
        await message.ack()

    async def _publish(self, jetstream: JetStreamContext, event: Dict[str, Any]) -> None:
        subject = self.config.subject(event["type"])
        await jetstream.publish(
            subject,
            json.dumps(event, ensure_ascii=False).encode("utf-8"),
            headers=nats_headers(event),
            # Naming the stream makes a misrouted publish fail loudly here
            # instead of landing somewhere no consumer reads.
            stream=self.config.stream,
        )
        logger.info(
            "published %s id=%s subject=%s", event["type"], event["id"], subject
        )


def _sequence(message: Msg) -> Optional[int]:
    try:
        return message.metadata.sequence.stream
    except Exception:  # pragma: no cover - metadata is absent off JetStream
        return None
