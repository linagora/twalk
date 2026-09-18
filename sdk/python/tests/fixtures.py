"""The contract's own fixtures, loaded from `contracts/cloudevents/v1/`.

The SDK's unit tests assert against the contract itself rather than against
hand-written copies: the fixtures are the worked examples every component
is checked against, so a divergence between the SDK's envelopes and the
contract shows up here.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, List

CONTRACT_DIR = Path(__file__).resolve().parents[3] / "contracts" / "cloudevents" / "v1"


def fixture(type_name: str) -> Dict[str, Any]:
    return json.loads((CONTRACT_DIR / "fixtures" / f"{type_name}.json").read_text("utf-8"))


def variant_fixture(type_name: str, variant: str) -> Dict[str, Any]:
    path = CONTRACT_DIR / "fixtures" / "variants" / type_name / f"{variant}.json"
    return json.loads(path.read_text("utf-8"))


def fixture_types() -> List[str]:
    """Every contract type that has a fixture, straight from the directory.

    The enumeration a test uses instead of a hand-written list, so that a
    type added to the contract is covered the day it lands rather than the
    day somebody remembers it (issue #147). `variants/` is not a type — it
    holds the conditional shapes of types listed here.
    """
    return sorted(
        path.stem for path in (CONTRACT_DIR / "fixtures").glob("*.json")
    )
