# The assistant persona's image.
#
# Personas ship as container images: the SDK and its dependencies install
# inside, because the host has no pip (PEP 668) and no venv module — and
# because a persona is a separate process talking to the bus (ADR 0008), so
# an image is what the deployment wants anyway. The Hermes runtime starts
# this image's process; a third-party persona will ship exactly the same
# way.
#
# The build context is the repository root, not this directory: the image
# carries the SDK from sdk/python/. Build it from the root with
#   docker build -f hermes/personas/assistant/Dockerfile .
# (the test suite does this through compose; see hermes/tests/).

FROM python:3.12-slim

# The SDK's dependencies first, so that editing the persona or the SDK does
# not re-resolve them.
COPY sdk/python/requirements.txt /tmp/requirements.txt
RUN pip install --no-cache-dir --root-user-action=ignore -r /tmp/requirements.txt \
    && rm /tmp/requirements.txt

WORKDIR /app
COPY sdk/python/twalk_sdk /app/twalk_sdk
COPY hermes/personas/assistant/assistant.py /app/assistant.py

# Unbuffered: the persona's logs are how an operator sees what it is doing,
# and a buffered stdout hides them until the process exits.
ENV PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1

RUN useradd --system --create-home persona
USER persona

ENTRYPOINT ["python", "/app/assistant.py"]
