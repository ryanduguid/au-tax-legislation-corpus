"""Live Federal Register capture through its public, injected-session seam."""

from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from fadden.capture_register import RegisterExchange, capture_register_run


CURRENT_URL = (
    "https://api.prod.legislation.gov.au/v1/versions?%24top=1&"
    "%24filter=titleId%20eq%20%27C2004A00467%27%20and%20isCurrent%20eq%20true&"
    "%24select=titleId%2Cstart%2CcompilationNumber%2CregisterId%2CisCurrent%2Cstatus%2CregisteredAt"
)
CURRENT_BODY = (
    b'{"@odata.context":"https://api.prod.legislation.gov.au/v1/'
    b'$metadata#Versions(titleId,start,compilationNumber,registerId,isCurrent,status,registeredAt)",'
    b'"value":[{"titleId":"C2004A00467","start":"2026-06-30T00:00:00",'
    b'"isCurrent":true,"status":"InForce","registerId":"C2026C00290",'
    b'"registeredAt":"2026-07-14T15:44:31.0377770","compilationNumber":"32"}]}'
)
CONTENT_TYPE = "application/json; odata.metadata=minimal; odata.streaming=true; charset=utf-8"


def json_bytes(value: object) -> bytes:
    """Hand the fixture through the documented JSON byte representation."""
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def sha256_id(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


class MemorySession:
    """Deterministic replacement for the one slow external boundary."""

    observed_at = "2026-08-29T00:00:00Z"

    def __init__(self, exchanges: dict[str, list[RegisterExchange]]) -> None:
        self.exchanges = {url: list(items) for url, items in exchanges.items()}
        self.urls: list[str] = []

    def get(self, url: str) -> RegisterExchange:
        self.urls.append(url)
        if url not in self.exchanges or not self.exchanges[url]:
            raise AssertionError(f"unexpected Register request: {url}")
        return self.exchanges[url].pop(0)


class LiveRegisterCaptureTests(unittest.TestCase):
    def test_single_unchanged_title_writes_complete_immutable_graph(self) -> None:
        """A missing query, evidence byte or cross-file binding must fail this case."""
        manifest = [
            {
                "id": "C2004A00467",
                "name": "A New Tax System (Australian Business Number) Act 1999",
                "collection": "Act",
                "versionStart": "2026-06-30",
                "compilationNumber": "32",
                "retrieved": "2026-08-04",
                "sourceUrl": (
                    "https://www.legislation.gov.au/C2004A00467/"
                    "2026-06-30/2026-06-30/text/original/epub"
                ),
                "unrelatedRichField": {"ignored": True},
            }
        ]
        exchange = RegisterExchange(
            checked_at="2026-08-29T00:00:01Z",
            status=200,
            headers={
                "Content-Type": CONTENT_TYPE,
                "OData-Version": "4.0",
                "Server": "must-not-be-retained",
            },
            body=CURRENT_BODY,
            attempts=1,
        )
        session = MemorySession({CURRENT_URL: [exchange]})

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes(manifest))
            destination = root / "capture"

            paths = capture_register_run(manifest_path, destination, session=session)

            self.assertEqual(
                paths,
                {
                    "baseline": destination / "monitor-baseline.json",
                    "capture": destination / "register-capture.json",
                    "observation": destination / "register-observation.json",
                },
            )
            self.assertEqual(session.urls, [CURRENT_URL])

            baseline = {
                "corpus": "Commonwealth tax statutes and legislative instruments",
                "retrieved": "2026-08-04",
                "source": "Federal Register of Legislation",
                "source_api": "https://api.prod.legislation.gov.au/v1/",
                "titles": [
                    {
                        "register_id": "C2004A00467",
                        "name": "A New Tax System (Australian Business Number) Act 1999",
                        "collection": "Act",
                        "compilation_number": "32",
                        "compilation_date": "2026-06-30",
                        "version_is_current": True,
                        "current_version_start": None,
                        "retrieved": "2026-08-04",
                        "source_url": (
                            "https://www.legislation.gov.au/C2004A00467/"
                            "2026-06-30/2026-06-30/text/original/epub"
                        ),
                        "register_page": (
                            "https://www.legislation.gov.au/C2004A00467/latest/text"
                        ),
                    }
                ],
            }
            baseline_content = json_bytes(baseline)
            self.assertEqual(paths["baseline"].read_bytes(), baseline_content)

            body_digest = sha256_id(CURRENT_BODY)
            body_hex = body_digest.removeprefix("sha256:")
            evidence_path = destination / "evidence" / f"sha256-{body_hex}.json"
            self.assertEqual(evidence_path.read_bytes(), CURRENT_BODY)

            result = {
                "register_id": "C2004A00467",
                "collection": "Act",
                "checked_at": "2026-08-29T00:00:01Z",
                "state": "UNCHANGED",
                "error_category": None,
                "requests": [
                    {
                        "role": "current",
                        "url": CURRENT_URL,
                        "checked_at": "2026-08-29T00:00:01Z",
                        "http_status": 200,
                        "transport_error_category": None,
                        "attempt_count": 1,
                        "response_headers": {
                            "content-type": CONTENT_TYPE,
                            "odata-version": "4.0",
                        },
                        "response_length": len(CURRENT_BODY),
                        "response_sha256": body_digest,
                        "evidence_path": f"evidence/sha256-{body_hex}.json",
                    }
                ],
            }
            capture = {
                "schema_version": "au-tax-register-capture.v1",
                "mode": "live",
                "observed_at": "2026-08-29T00:00:00Z",
                "source_api": "https://api.prod.legislation.gov.au/v1/",
                "baseline_sha256": sha256_id(baseline_content),
                "expected_register_ids": ["C2004A00467"],
                "complete": True,
                "results": [result],
            }
            capture_content = json_bytes(capture)
            self.assertEqual(paths["capture"].read_bytes(), capture_content)
            self.assertEqual(
                set(capture),
                {
                    "schema_version",
                    "mode",
                    "observed_at",
                    "source_api",
                    "baseline_sha256",
                    "expected_register_ids",
                    "complete",
                    "results",
                },
            )

            result_digest = sha256_id(json_bytes(result))
            observation = {
                "schema_version": "au-tax-register-observation.v4",
                "mode": "live",
                "observed_at": "2026-08-29T00:00:00Z",
                "scope_id": "au-primary-tax-legislation.v4",
                "baseline_sha256": sha256_id(baseline_content),
                "capture_sha256": sha256_id(capture_content),
                "expected_register_ids": ["C2004A00467"],
                "complete": True,
                "run_status": "VERIFIED",
                "observations": [
                    {
                        "register_id": "C2004A00467",
                        "collection": "Act",
                        "state": "UNCHANGED",
                        "evidence_id": (
                            "frl:C2004A00467:"
                            f"{result_digest.removeprefix('sha256:')[:32]}"
                        ),
                        "observed_compilation_number": None,
                        "observed_compilation_date": None,
                        "observed_register_document_id": None,
                        "current_version_start": None,
                        "evidence_url": (
                            "https://www.legislation.gov.au/C2004A00467/latest/text"
                        ),
                        "checked_at": "2026-08-29T00:00:01Z",
                        "error_category": None,
                        "capture_result_sha256": result_digest,
                        "primary_response_sha256": body_digest,
                        "primary_response_media_type": "application/json",
                    }
                ],
            }
            observation_content = paths["observation"].read_bytes()
            self.assertEqual(observation_content, json_bytes(observation))
            self.assertEqual(
                set(json.loads(observation_content)),
                {
                    "schema_version",
                    "mode",
                    "observed_at",
                    "scope_id",
                    "baseline_sha256",
                    "capture_sha256",
                    "expected_register_ids",
                    "complete",
                    "run_status",
                    "observations",
                },
            )


if __name__ == "__main__":
    unittest.main()
