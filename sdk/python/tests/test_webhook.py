"""The seam to Hermes, field by field — and absence by absence.

This is the test ADR 0032 asks for and the one issue #206 calls decisive
about the body: "asserted field by field, including that the message body is
absent unless the template names it". Two assertions carry it.

:meth:`TheTemplate.test_the_body_is_exactly_these_named_fields` pins the key
set with ``==`` rather than checking that the fields it wants are present,
because a template that grew a field would pass the second kind of check
for ever.

:meth:`TheTemplate.test_nothing_identifying_anybody_crosses_the_seam` takes
the other direction, which is the one that matters: it builds a trigger
whose every withheld value is a distinctive marker, serialises the body that
would really be sent, and searches those bytes for each marker. That is the
habit `CONTRIBUTING.md` records — "assert the absence of things as
deliberately as their presence" — and it is the only kind of assertion that
survives somebody adding a convenience field: Hermes's memory absorbs
whatever it is shown and a body copied into ``MEMORY.md`` never expires, so
a leak here is permanent on a machine Twalk may not own.

The rest is the configuration that has to refuse: plaintext without an
operator saying so, a URL that is not a route, a seam with no secret.
"""

from __future__ import annotations

import copy
import hashlib
import hmac
import json
import unittest

from fixtures import fixture, variant_fixture

from twalk_sdk.config import Config, ConfigError
from twalk_sdk.envelope import suggest_id
from twalk_sdk.trigger import InboundMessage
from twalk_sdk.webhook import (
    ALLOW_INSECURE_URL_VARIABLE,
    MESSAGE_RECEIVED_EVENT,
    TEMPLATE_VERSION,
    HermesSeam,
    SeamError,
    delivery_id,
    encode_body,
    reference,
    signed_headers,
    webhook_body,
)

ROUTE_URL = "https://hermes.example.org:8644/webhooks/twalk-messages"
SECRET = "a-secret-only-the-route-and-this-persona-hold"

#: Every value below is a marker: it appears in the trigger and must appear
#: nowhere in the body. They are the things a person is identified by, plus
#: the two kinds of words that belong to somebody other than the sender.
SENDER = "@whatsapp_33612345678:example.com"
ROOM_SOURCE = "matrix://matrix.example.com/!abcXYZ123:example.com"
DISPLAY_NAME = "Aïcha Benali"
PHONE = "+33612345678"
QUOTED_EXCERPT = "QUOTED-WORDS-THAT-BELONG-TO-SOMEBODY-ELSE"
ATTACHMENT_CAPTION = "CAPTION-SOMEBODY-ELSE-WROTE"
ATTACHMENT_MXC = "mxc://matrix.example.com/QWxpY2VQaG90bzIwMjYwOTE3"
DECRYPTION_KEY = "aWF6-32KGYaC3A_FEUCk1Bt0JA37zP0wrStgmdCaW-0"
TRACEPARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"


def trigger_event() -> dict:
    """The contract's own inbound fixture, with a quoted excerpt and a
    distinctive attachment caption added — the two third-party words the
    plain fixture happens not to carry (its caption repeats the body, so a
    search for it would find the body and prove nothing)."""
    event = copy.deepcopy(fixture("inbound.message.received"))
    event["data"]["reply_to"] = {
        "event_id": "$quoted:example.com",
        "body": QUOTED_EXCERPT,
        "sender": "@whatsapp_33699999999:example.com",
    }
    event["data"]["attachments"][0]["caption"] = ATTACHMENT_CAPTION
    return event


class TheTemplate(unittest.TestCase):
    def body(self, **overrides) -> dict:
        arguments = {
            "trigger": InboundMessage(trigger_event()),
            "persona_id": "assistant",
            "attempt": 1,
            "user_language": "fr",
        }
        arguments.update(overrides)
        return webhook_body(**arguments)

    def test_the_body_is_exactly_these_named_fields(self) -> None:
        self.assertEqual(
            self.body(),
            {
                "event_type": MESSAGE_RECEIVED_EVENT,
                "template_version": TEMPLATE_VERSION,
                "reference": (
                    "TWALK-REF:assistant:"
                    "20be32e73506b9104a6a1bf76fc2d2a15cbd2b8a0a421833be01f865ca9886d0:1"
                ),
                "network": "whatsapp",
                "received_at": "2026-09-17T10:00:00Z",
                "message": "On décale à 20h ?",
                "format": "text/plain",
                "quotes_an_earlier_message": True,
                "has_attachments": True,
                "user_language": "fr",
            },
        )

    def mail(self) -> dict:
        return self.body(trigger=InboundMessage(variant_fixture(
            "inbound.message.received", "email"
        )))

    def test_a_mails_body_is_exactly_these_named_fields(self) -> None:
        """The closed list again, for the shape that has eleven members.

        Pinned with ``==`` like the bridged one and for the same reason: the
        mail path is the one that grew a member (#362), so it is the path
        where a twelfth could be added without anybody noticing."""
        self.assertEqual(
            self.mail(),
            {
                "event_type": MESSAGE_RECEIVED_EVENT,
                "template_version": TEMPLATE_VERSION,
                "reference": (
                    "TWALK-REF:assistant:"
                    "8d5959c3b0bcd54ea70f7d744f0264d84eccfb2054a08102c0ff19f5705f3d3d:1"
                ),
                "network": "email",
                "received_at": "2026-09-21T08:15:03Z",
                "message": (
                    "Bonjour Michel,\n\nOn se voit toujours lundi pour le "
                    "point hebdo ?\n\nAlice"
                ),
                "format": "text/plain",
                "quotes_an_earlier_message": True,
                "has_attachments": True,
                "user_language": "fr",
                "title": "Re: Point hebdo",
            },
        )

    def test_nothing_identifying_anybody_crosses_beside_a_subject(self) -> None:
        """A subject line crossing is not a door held open for the name
        attached to it, nor for the addresses and message ids a mail carries
        that a bridged message does not."""
        sent = encode_body(self.mail()).decode("utf-8")
        for marker, what in (
            ("Alice Martin", "the contact's display name"),
            ("alice@example.org", "the sender's address"),
            ("9b8c7d6e-1", "the mail's own message id"),
            ("c9d8e7f6-5a4b", "the thread the mail belongs to"),
            ("direct", "who else was on the mail, which is the deployment's business"),
        ):
            self.assertNotIn(
                marker,
                sent,
                f"{what} reached Hermes; its memory never expires what it is shown",
            )

    def test_a_message_with_no_subject_carries_no_member_at_all(self) -> None:
        """A bridged message has no subject, and a revoked sender's mail
        carries none either. An absent member says that; an empty string
        says there was a subject and it was blank, which is a different
        message and a lie about this one (#359's rule, other door)."""
        self.assertNotIn("title", self.body())

    def test_nothing_identifying_anybody_crosses_the_seam(self) -> None:
        sent = encode_body(self.body()).decode("utf-8")
        for marker, what in (
            (SENDER, "the sender's Matrix ID"),
            (ROOM_SOURCE, "the portal room the message arrived in"),
            ("!abcXYZ123", "the portal room id"),
            (DISPLAY_NAME, "the contact's display name"),
            (PHONE, "the network's own identifier for the contact"),
            (QUOTED_EXCERPT, "a quoted excerpt, which belongs to its author"),
            (ATTACHMENT_CAPTION, "an attachment's caption"),
            (ATTACHMENT_MXC, "an attachment's location"),
            (DECRYPTION_KEY, "an attachment's decryption material"),
            (TRACEPARENT, "the deployment's own trace"),
            ("granted", "the consent state, which is the deployment's business"),
        ):
            self.assertNotIn(
                marker,
                sent,
                f"{what} reached Hermes; its memory never expires what it is shown",
            )

    def test_the_raw_event_does_not_cross_the_seam(self) -> None:
        """The negative the acceptance criterion states in so many words: the
        template names the message and nothing names the envelope."""
        body = self.body()
        for attribute in ("id", "specversion", "subject", "source", "data", "consent"):
            self.assertNotIn(attribute, body)

    def test_a_message_with_no_quote_and_no_attachment_says_so(self) -> None:
        event = trigger_event()
        event["data"]["reply_to"] = None
        event["data"]["attachments"] = []
        body = self.body(trigger=InboundMessage(event))
        self.assertIs(body["quotes_an_earlier_message"], False)
        self.assertIs(body["has_attachments"], False)

    def test_no_user_language_is_null_and_not_a_default(self) -> None:
        """ADR 0016's fallback has no value when the user set none, and the
        seam says so rather than choosing English on Hermes's behalf."""
        self.assertIsNone(self.body(user_language=None)["user_language"])

    def test_the_attempt_is_in_the_reference_and_nowhere_else(self) -> None:
        body = self.body(attempt=2)
        self.assertTrue(body["reference"].endswith(":2"))
        self.assertNotIn("attempt", body)


class TheDeliveryId(unittest.TestCase):
    def test_it_is_the_id_of_the_suggestion_this_wake_will_produce(self) -> None:
        """Hermes deduplicates on the delivery id for an hour and the bus
        deduplicates on ``Nats-Msg-Id``; one key for both, or a redelivered
        trigger is billed twice and drafted twice."""
        self.assertEqual(
            delivery_id("assistant", "a" * 64, 1), suggest_id("assistant", "a" * 64, 1)
        )

    def test_another_attempt_is_another_delivery(self) -> None:
        self.assertNotEqual(
            delivery_id("assistant", "a" * 64, 1),
            delivery_id("assistant", "a" * 64, 2),
        )


class TheSignature(unittest.TestCase):
    def test_it_is_hermes_generic_v2_over_timestamp_dot_body(self) -> None:
        """Recomputed here the way Hermes's adapter does it, from its own
        source: ``hexdigest(secret, "<timestamp>." + body)``. V2 and not V1
        because V1 signs the body alone, so a captured request replays for
        ever."""
        body = b'{"message":"hello"}'
        headers = signed_headers(
            body, secret=SECRET, timestamp=1_789_000_000, delivery="d" * 64
        )
        expected = hmac.new(
            SECRET.encode("utf-8"),
            b"1789000000." + body,
            hashlib.sha256,
        ).hexdigest()
        self.assertEqual(headers["X-Webhook-Signature-V2"], expected)
        self.assertEqual(headers["X-Webhook-Timestamp"], "1789000000")
        self.assertEqual(headers["X-Request-ID"], "d" * 64)
        self.assertNotIn("X-Webhook-Signature", headers)

    def test_it_signs_the_bytes_that_are_sent(self) -> None:
        """One encoder, because a body serialised twice with different
        separators signs one thing and sends another — which Hermes reports
        as an invalid signature and no amount of re-reading the secret
        explains."""
        body = encode_body(webhook_body(
            trigger=InboundMessage(trigger_event()),
            persona_id="assistant",
            user_language=None,
        ))
        self.assertEqual(body, encode_body(json.loads(body)))


class TheSeamsConfiguration(unittest.TestCase):
    def test_no_url_is_no_seam_and_not_an_error(self) -> None:
        """Every deployment before ADR 0032 is in this state, and it is not a
        failure: the persona reasons with the model it was given."""
        self.assertIsNone(HermesSeam.from_env({}))

    def test_https_and_a_route_and_a_secret_is_a_seam(self) -> None:
        seam = HermesSeam.from_env(
            {
                "TWALK_HERMES_WEBHOOK_URL": ROUTE_URL,
                "TWALK_HERMES_WEBHOOK_SECRET": SECRET,
            }
        )
        assert seam is not None
        self.assertEqual(seam.route, "twalk-messages")
        self.assertEqual(seam.health_url, "https://hermes.example.org:8644/health")
        self.assertFalse(seam.is_plaintext)

    def test_plaintext_is_refused_unless_the_operator_named_it(self) -> None:
        plaintext = {
            "TWALK_HERMES_WEBHOOK_URL": "http://127.0.0.1:8644/webhooks/twalk-messages",
            "TWALK_HERMES_WEBHOOK_SECRET": SECRET,
        }
        with self.assertRaises(SeamError) as refusal:
            HermesSeam.from_env(plaintext)
        self.assertIn(ALLOW_INSECURE_URL_VARIABLE, str(refusal.exception))

        allowed = dict(plaintext, **{ALLOW_INSECURE_URL_VARIABLE: "true"})
        seam = HermesSeam.from_env(allowed)
        assert seam is not None
        self.assertTrue(seam.is_plaintext)

    def test_a_url_that_is_not_a_route_is_refused(self) -> None:
        for url in (
            "https://hermes.example.org:8644",
            "https://hermes.example.org:8644/",
            "https://hermes.example.org:8644/health",
            "https://hermes.example.org:8644/webhooks/",
            "hermes.example.org:8644/webhooks/twalk-messages",
        ):
            with self.subTest(url=url):
                with self.assertRaises(SeamError):
                    HermesSeam.from_env(
                        {
                            "TWALK_HERMES_WEBHOOK_URL": url,
                            "TWALK_HERMES_WEBHOOK_SECRET": SECRET,
                        }
                    )

    def test_a_seam_with_no_secret_is_refused(self) -> None:
        with self.assertRaises(SeamError) as refusal:
            HermesSeam.from_env({"TWALK_HERMES_WEBHOOK_URL": ROUTE_URL})
        self.assertIn("INSECURE_NO_AUTH", str(refusal.exception))

    def test_the_persona_refuses_to_start_on_a_half_configured_seam(self) -> None:
        """A seam is configuration, so it is validated where the rest is: on
        the first line, not on the first message."""
        environment = {
            "TWALK_PERSONA_ID": "assistant",
            "TWALK_HERMES_DOMAIN": "twalk.example.org",
            "TWALK_LLM_BASE_URL": "http://llm.example.org/v1",
            "TWALK_LLM_MODEL": "qwen",
            "TWALK_HERMES_WEBHOOK_URL": ROUTE_URL,
        }
        with self.assertRaises(ConfigError):
            Config.from_env(environment)

        configured = Config.from_env(
            dict(environment, TWALK_HERMES_WEBHOOK_SECRET=SECRET)
        )
        assert configured.hermes is not None
        self.assertEqual(configured.hermes.url, ROUTE_URL)

    def test_no_seam_leaves_the_configuration_as_it_was(self) -> None:
        configured = Config.from_env(
            {
                "TWALK_PERSONA_ID": "assistant",
                "TWALK_HERMES_DOMAIN": "twalk.example.org",
                "TWALK_LLM_BASE_URL": "http://llm.example.org/v1",
                "TWALK_LLM_MODEL": "qwen",
            }
        )
        self.assertIsNone(configured.hermes)


class TheReference(unittest.TestCase):
    def test_it_carries_the_three_facts_the_gateway_needs(self) -> None:
        self.assertEqual(
            reference("assistant", "b" * 64, 3), f"TWALK-REF:assistant:{'b' * 64}:3"
        )

    def test_it_carries_no_fact_about_a_person(self) -> None:
        token = reference("assistant", "b" * 64, 1)
        for marker in (SENDER, DISPLAY_NAME, PHONE, ROOM_SOURCE):
            self.assertNotIn(marker, token)


if __name__ == "__main__":
    unittest.main()
