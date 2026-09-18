"""The contract's own fixtures, loaded from `contracts/cloudevents/v1/`.

The SDK's unit tests assert against the contract itself rather than against
hand-written copies: the fixtures are the worked examples every component
is checked against, so a divergence between the SDK's envelopes and the
contract shows up here.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict

CONTRACT_DIR = Path(__file__).resolve().parents[3] / "contracts" / "cloudevents" / "v1"


def fixture(type_name: str) -> Dict[str, Any]:
    return json.loads((CONTRACT_DIR / "fixtures" / f"{type_name}.json").read_text("utf-8"))


def variant_fixture(type_name: str, variant: str) -> Dict[str, Any]:
    path = CONTRACT_DIR / "fixtures" / "variants" / type_name / f"{variant}.json"
    return json.loads(path.read_text("utf-8"))
