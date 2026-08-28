"""Live Federal Register capture through its public, injected-session seam."""

from __future__ import annotations

import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from fadden.capture_register import (
    CaptureRegisterError,
    RegisterExchange,
    capture_register_run,
)


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
INSTRUMENT_URL = (
    "https://api.prod.legislation.gov.au/v1/versions?%24top=1&"
    "%24filter=titleId%20eq%20%27F2020L01498%27%20and%20isCurrent%20eq%20true&"
    "%24select=titleId%2Cstart%2CcompilationNumber%2CregisterId%2CisCurrent%2Cstatus%2CregisteredAt"
)


def json_bytes(value: object) -> bytes:
    """Hand the fixture through the documented JSON byte representation."""
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def sha256_id(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def manifest_row(**changes: object) -> dict[str, object]:
    row: dict[str, object] = {
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
    }
    row.update(changes)
    return row


def version_body(
    title_id: str,
    start: str,
    compilation_number: str,
    document_id: str,
) -> bytes:
    return json.dumps(
        {
            "@odata.context": (
                "https://api.prod.legislation.gov.au/v1/$metadata#Versions("
                "titleId,start,compilationNumber,registerId,isCurrent,status,registeredAt)"
            ),
            "value": [
                {
                    "titleId": title_id,
                    "start": f"{start}T00:00:00",
                    "isCurrent": True,
                    "status": "InForce",
                    "registerId": document_id,
                    "registeredAt": "2026-08-20T01:02:03.1234567",
                    "compilationNumber": compilation_number,
                }
            ],
        },
        separators=(",", ":"),
    ).encode("utf-8")


def successful_exchange(body: bytes, checked_at: str) -> RegisterExchange:
    return RegisterExchange(
        checked_at=checked_at,
        status=200,
        headers={"Content-Type": CONTENT_TYPE, "OData-Version": "4.0"},
        body=body,
        attempts=1,
    )


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


class LiveRegisterCaptureManifestTests(unittest.TestCase):
    def test_projects_rich_rows_in_canonical_order_with_latest_retrieval(self) -> None:
        """Input order, rich fields and per-title dates must not distort the baseline."""
        act = manifest_row(unrelated={"deep": [1, 2, 3]})
        instrument = manifest_row(
            id="F2020L01498",
            name="A New Tax System (Australian Business Number) Regulations 2020",
            collection="LegislativeInstrument",
            versionStart="2025-10-04",
            compilationNumber="2",
            retrieved="2026-08-03",
            sourceUrl=(
                "https://www.legislation.gov.au/F2020L01498/"
                "2025-10-04/2025-10-04/text/original/epub"
            ),
            version_is_current=False,
            current_version_start="2025-10-05",
            generatedField="ignored",
        )
        session = MemorySession(
            {
                CURRENT_URL: [
                    successful_exchange(CURRENT_BODY, "2026-08-29T00:00:02Z")
                ],
                INSTRUMENT_URL: [
                    successful_exchange(
                        version_body("F2020L01498", "2025-10-04", "2", "F2025C00987"),
                        "2026-08-29T00:00:01Z",
                    )
                ],
            }
        )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes([instrument, act]))
            paths = capture_register_run(manifest_path, root / "capture", session=session)

            baseline = json.loads(paths["baseline"].read_bytes())
            self.assertEqual(baseline["retrieved"], "2026-08-04")
            self.assertEqual(
                [title["register_id"] for title in baseline["titles"]],
                ["C2004A00467", "F2020L01498"],
            )
            self.assertEqual(
                set(baseline["titles"][0]),
                {
                    "register_id",
                    "name",
                    "collection",
                    "compilation_number",
                    "compilation_date",
                    "version_is_current",
                    "current_version_start",
                    "retrieved",
                    "source_url",
                    "register_page",
                },
            )
            self.assertIs(baseline["titles"][0]["version_is_current"], True)
            self.assertIsNone(baseline["titles"][0]["current_version_start"])
            self.assertIs(baseline["titles"][1]["version_is_current"], False)
            self.assertEqual(
                baseline["titles"][1]["current_version_start"], "2025-10-05"
            )
            self.assertEqual(session.urls, [CURRENT_URL, INSTRUMENT_URL])

    def test_rejects_ambiguous_or_invalid_manifest_before_output_preflight(self) -> None:
        """Every invalid source snapshot must fail before a missing output parent appears."""
        valid = manifest_row()
        duplicate_id_rows = [valid, dict(valid)]
        duplicate_member = json_bytes([valid]).replace(
            b'"id": "C2004A00467",',
            b'"id": "C2004A00467",\n    "id": "C2004A00467",',
            1,
        )
        nested_duplicate = json_bytes(
            [manifest_row(unrelated={"one": 1})]
        ).replace(b'"one": 1', b'"one": 1, "one": 2', 1)
        cases: list[tuple[str, bytes, str]] = [
            ("empty array", b"[]\n", "non-empty array"),
            ("object top level", b"{}\n", "non-empty array"),
            ("oversized", b'["' + (b"x" * (8 * 1024 * 1024)) + b'"]', "8 MiB"),
            ("invalid utf8", b"[\xff]", "UTF-8"),
            ("duplicate title member", duplicate_member, "duplicate JSON member"),
            ("duplicate ignored nested member", nested_duplicate, "duplicate JSON member"),
            ("duplicate identifier", json_bytes(duplicate_id_rows), "duplicate Register"),
            (
                "invalid collection",
                json_bytes([manifest_row(collection="Regulation")]),
                "collection is unsupported",
            ),
            (
                "non-canonical date",
                json_bytes([manifest_row(retrieved="2026-8-4")]),
                "ISO calendar date",
            ),
            (
                "control character",
                json_bytes([manifest_row(name="Tax Act\u0001")]),
                "control characters",
            ),
            (
                "invalid Register identifier",
                json_bytes([manifest_row(id="../../escape")]),
                "invalid Federal Register identifier",
            ),
            (
                "false current flag without current start",
                json_bytes([manifest_row(version_is_current=False)]),
                "current_version_start is required",
            ),
            (
                "non-boolean current flag",
                json_bytes([manifest_row(version_is_current="false")]),
                "must be a boolean",
            ),
        ]

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, (label, content, expected) in enumerate(cases):
                with self.subTest(case=label):
                    manifest_path = root / f"manifest-{index}.json"
                    manifest_path.write_bytes(content)
                    missing_parent = root / f"missing-{index}"
                    destination = missing_parent / "capture"
                    with self.assertRaisesRegex(CaptureRegisterError, expected):
                        capture_register_run(
                            manifest_path,
                            destination,
                            session=MemorySession({}),
                        )
                    self.assertFalse(missing_parent.exists())
                    self.assertFalse(destination.exists())

    def test_accepts_only_the_exact_register_document_source_url(self) -> None:
        """Host, identity and version changes must not pass as equivalent evidence URLs."""
        expected_path = "/C2004A00467/2026-06-30/2026-06-30/text/original/epub"
        invalid_urls = {
            "plain http": f"http://www.legislation.gov.au{expected_path}",
            "lookalike host": f"https://www.legislation.gov.au.evil.example{expected_path}",
            "credentials": f"https://user@www.legislation.gov.au{expected_path}",
            "port": f"https://www.legislation.gov.au:443{expected_path}",
            "query": f"https://www.legislation.gov.au{expected_path}?download=1",
            "fragment": f"https://www.legislation.gov.au{expected_path}#text",
            "wrong id": (
                "https://www.legislation.gov.au/C2004A00468/"
                "2026-06-30/2026-06-30/text/original/epub"
            ),
            "wrong version": (
                "https://www.legislation.gov.au/C2004A00467/"
                "2026-06-29/2026-06-29/text/original/epub"
            ),
            "encoded separator": (
                "https://www.legislation.gov.au/C2004A00467%2F2026-06-30/"
                "2026-06-30/text/original/epub"
            ),
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, (label, source_url) in enumerate(invalid_urls.items()):
                with self.subTest(case=label):
                    manifest_path = root / f"source-{index}.json"
                    manifest_path.write_bytes(
                        json_bytes([manifest_row(sourceUrl=source_url)])
                    )
                    destination = root / f"capture-{index}"
                    with self.assertRaisesRegex(
                        CaptureRegisterError, "sourceUrl does not match"
                    ):
                        capture_register_run(
                            manifest_path,
                            destination,
                            session=MemorySession({}),
                        )
                    self.assertFalse(destination.exists())

    def test_manifest_must_be_an_ordinary_file(self) -> None:
        """A directory must never be opened as the authoritative manifest snapshot."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_directory = root / "manifest.json"
            manifest_directory.mkdir()
            with self.assertRaisesRegex(CaptureRegisterError, "ordinary file"):
                capture_register_run(
                    manifest_directory,
                    root / "capture",
                    session=MemorySession({}),
                )

    def test_manifest_symbolic_link_is_rejected_without_touching_target(self) -> None:
        """Following a manifest link would let the captured input change locations."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target.json"
            target_content = json_bytes([manifest_row()])
            target.write_bytes(target_content)
            link = root / "manifest.json"
            try:
                link.symlink_to(target)
            except OSError as exc:
                self.skipTest(f"symbolic links are unavailable: {exc}")
            with self.assertRaisesRegex(CaptureRegisterError, "ordinary file"):
                capture_register_run(
                    link,
                    root / "capture",
                    session=MemorySession({}),
                )
            self.assertEqual(target.read_bytes(), target_content)


if __name__ == "__main__":
    unittest.main()
