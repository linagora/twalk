"""assistant — the reference Twalk persona.

It reads the user's incoming messages and drafts a reply for them to
approve. It never sends: a suggestion becomes a reply only when a human
approval references it, which is the Hermes runtime's job (issue #23) and
the Sensor's outbound path after that.

Everything that must not be got wrong is the SDK's (`sdk/python/twalk_sdk`):
the consent gate runs before this file's handler is ever called, the
`thinking` event is published the moment processing starts, the returned
suggestion becomes a contract-valid `persona.suggest.produced` with the
contract's deterministic id, and the trigger's network, consent and trace
are carried through. What is left here is the persona's own judgement — the
prompt, and when to stay quiet — which is the point: a persona author
writes this much and no more.
"""

from __future__ import annotations

from typing import Optional

from twalk_sdk import Context, InboundMessage, Persona, Suggestion, language_name
from twalk_sdk.hermes import HandedToHermes
from twalk_sdk.llm import system, user

#: Short, and deliberately about restraint: what the model returns is shown
#: to the user as a ready-to-send reply, so anything but the reply itself
#: (a preamble, an explanation, quotes around it) is noise the user has to
#: delete before approving.
#:
#: It stays in the code, versioned and reviewed, rather than in
#: configuration: the prompt *is* the persona's behaviour, and two
#: deployments running this file must behave the same way (ADR 0015). The
#: language instruction is the conversation's, not the user's — a
#: suggestion exists to be sent to someone else, so a French user answering
#: an English contact must not be handed French (ADR 0016). What the user's
#: own language is for is the sentence below, and only that.
SYSTEM_PROMPT = (
    "You draft replies to personal messages on behalf of the user. "
    "Answer with the reply text only: no preamble, no explanation, no quotes. "
    "Write in the same language as the message you are replying to. "
    "Keep it short and in the tone of a personal conversation. "
    "Never invent facts, commitments or times the message does not support; "
    "if the message needs information you do not have, draft a reply that asks for it."
)

#: The **fallback** ADR 0016 legislates for, appended only when the user's
#: own language is configured — which it may not be
#: (`twalk_sdk.language`). It comes after the instruction above and never
#: replaces it: a message whose language the model *can* tell is answered in
#: that language, and the preference governs the ambiguous case alone. The
#: examples are the ambiguous cases that actually occur — the first real
#: suggestion this product produced answered "Test received." to a French
#: speaker who had written `test` (issue #164).
LANGUAGE_FALLBACK = (
    "When the message is too short or ambiguous to tell — a greeting, a single "
    "word, an emoji, a link — write in {language}, which is the user's own "
    "language."
)

#: The completion budget, and it is a **ceiling rather than a target**: what
#: keeps a suggestion short is the prompt above, not this number.
#:
#: It was 300 — generous for a reply, and nothing for a model that reasons
#: before it answers, which is now the common case. Such a model spends the
#: whole budget on `reasoning_content` and answers `finish_reason: "length"`
#: with no content at all: on the reference deployment, against Qwen behind
#: LiteLLM, that was every message (issue #162). The SDK now names that
#: outcome and refuses the trigger instead of retrying it, which makes the
#: failure legible and free; this makes it not happen.
#:
#: An operator who wants another number sets one: `TWALK_LLM_PARAMS` is
#: merged last, so `{"max_tokens": 4000}` overrides this (ADR 0015).
MAX_TOKENS = 2000

#: Low, not zero: the same message should get the same draft on a retry.
TEMPERATURE = 0.2

persona = Persona.from_env()


def system_prompt(user_language: Optional[str]) -> str:
    """The persona's instructions for one deployment.

    The prompt is code (ADR 0015); the one thing about it that is
    configuration is the name of the language to fall back to, and it is
    absent when the user has set no preference — in which case this persona
    says nothing about the ambiguous case rather than inventing an answer
    for it. The SDK has already said so in the log at startup.
    """
    if not user_language:
        return SYSTEM_PROMPT
    return f"{SYSTEM_PROMPT} {LANGUAGE_FALLBACK.format(language=language_name(user_language))}"


@persona.on_inbound_message
async def draft_reply(
    message: InboundMessage, context: Context
) -> Optional[Suggestion] | HandedToHermes:
    """Drafts one reply to one message the user consented to.

    Called only for events whose consent is `granted` — the SDK's gate has
    already dropped everything else, and no message content has been read
    at that point.

    Two ways to a draft, and the deployment chooses by configuration. With no
    seam to Hermes (`TWALK_HERMES_WEBHOOK_URL` unset) this persona reasons
    with the model endpoint the operator named, which is what it has always
    done and what every deployment before ADR 0032 does. With a seam, the
    message is handed to Hermes instead and the suggestion comes back through
    the Companion Gateway: Hermes has memory, skills and a calendar, and a
    persona that also called a model would be asking two brains the same
    question and publishing whichever answered first.
    """
    text = (message.body or "").strip()
    if not text:
        # v0.1 answers text. An attachment-only message (or one whose body
        # the Sensor could not read) is skipped quietly: no suggestion is a
        # valid outcome, and a persona that guesses at a photo would be
        # worse than one that says nothing.
        context.logger.info(
            "no text to reply to, skipping event_id=%s network=%s",
            message.event_id,
            message.network,
        )
        return None

    if context.hermes is not None:
        # The whole body is the SDK's (`twalk_sdk.webhook`): this persona
        # chooses *whether* to wake Hermes and cannot choose what crosses,
        # which is the same arrangement as the consent gate and for the same
        # reason. The prompt is not here either — it is the route's, on the
        # Hermes host, because ADR 0032 has the Companion configure the seam
        # and never the agent.
        return await context.hermes.wake(
            message,
            persona_id=context.config.persona_id,
            user_language=context.config.user_language,
        )

    reply = await context.llm.complete(
        [system(system_prompt(context.config.user_language)), user(text)],
        temperature=TEMPERATURE,
        max_tokens=MAX_TOKENS,
    )
    return Suggestion(body=reply)


if __name__ == "__main__":
    persona.run()
