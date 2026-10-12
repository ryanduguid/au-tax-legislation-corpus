"""Fuzz the privacy patterns the corpus build runs over every extracted Act."""

from __future__ import annotations

import re
import sys
from pathlib import Path

# The fuzzing job installs only Atheris, so import the package from the checkout.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from fadden import pii_patterns  # noqa: E402

DIGEST = re.compile(r"[0-9a-f]{64}")
KINDS = {kind for kind, _ in pii_patterns.CONTACT_PATTERNS}


def test_one_input(data: bytes) -> None:
    text = data.decode("utf-8", errors="replace")
    # Every pattern must finish on arbitrary text; one that backtracks without
    # bound shows up here as a timeout. Explicit raises, because python -O
    # strips assert statements.
    names, numbers = pii_patterns.private_person_registration_details(text)
    if not all(name in text for name in names) or not all(n in text for n in numbers):
        raise AssertionError("a reported match is not in the text")
    for kind, digest in pii_patterns.contact_fingerprints(text):
        # A fingerprint names its kind and carries a digest, never the identifier.
        if kind not in KINDS or not DIGEST.fullmatch(digest):
            raise AssertionError(f"malformed fingerprint {(kind, digest)!r}")


if __name__ == "__main__":
    import atheris

    atheris.instrument_all()
    atheris.Setup(sys.argv, test_one_input)
    atheris.Fuzz()
