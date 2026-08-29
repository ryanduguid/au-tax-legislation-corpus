"""Live Federal Register capture through its public, injected-session seam."""

from __future__ import annotations

import hashlib
import io
import json
import os
import tempfile
import unittest
import urllib.error
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path
from unittest import mock

import fadden.capture_register as capture_module
from fadden.capture_register import (
    CaptureRegisterError,
    RegisterExchange,
    capture_register_run,
    validate_capture_graph,
)
from fadden.export_publication_bundles import (
    PublicationBundleError,
    export_publication_bundles,
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
HISTORY_URL = (
    "https://api.prod.legislation.gov.au/v1/versions?%24top=1&"
    "%24filter=titleId%20eq%20%27C2004A00467%27&%24orderby=start%20desc&"
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
    compilation_number: str | None,
    document_id: str | None,
    *,
    is_current: bool = True,
    status: str = "InForce",
    registered_at: str | None = None,
) -> bytes:
    if registered_at is None and document_id is not None:
        registered_at = "2026-08-20T01:02:03.1234567"
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
                    "isCurrent": is_current,
                    "status": status,
                    "registerId": document_id,
                    "registeredAt": registered_at,
                    "compilationNumber": compilation_number,
                }
            ],
        },
        separators=(",", ":"),
    ).encode("utf-8")


def empty_versions_body() -> bytes:
    return json.dumps(
        {
            "@odata.context": (
                "https://api.prod.legislation.gov.au/v1/$metadata#Versions("
                "titleId,start,compilationNumber,registerId,isCurrent,status,registeredAt)"
            ),
            "value": [],
        },
        separators=(",", ":"),
    ).encode("utf-8")


def current_url_for(title_id: str) -> str:
    return CURRENT_URL.replace("C2004A00467", title_id)


def history_url_for(title_id: str) -> str:
    return HISTORY_URL.replace("C2004A00467", title_id)


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


class FakeHttpResponse:
    """Complete HTTP surface consumed by the production session."""

    def __init__(
        self,
        body: bytes,
        *,
        status: int = 200,
        headers: dict[str, str] | None = None,
    ) -> None:
        self.status = status
        self.headers = headers or {
            "Content-Type": CONTENT_TYPE,
            "OData-Version": "4.0",
        }
        self.body = body
        self.read_sizes: list[int] = []

    def __enter__(self) -> "FakeHttpResponse":
        return self

    def __exit__(self, *_args: object) -> None:
        return None

    def read(self, size: int) -> bytes:
        self.read_sizes.append(size)
        return self.body[:size]


class FakeOpener:
    def __init__(self, effects: list[FakeHttpResponse | BaseException]) -> None:
        self.effects = list(effects)
        self.calls: list[tuple[object, int]] = []

    def open(self, request: object, *, timeout: int) -> FakeHttpResponse:
        self.calls.append((request, timeout))
        if not self.effects:
            raise AssertionError("unexpected production HTTP attempt")
        effect = self.effects.pop(0)
        if isinstance(effect, BaseException):
            raise effect
        return effect


class FakeClock:
    def __init__(self) -> None:
        self.now = 0.0
        self.sleeps: list[float] = []

    def monotonic(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.sleeps.append(seconds)
        self.now += seconds


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

    def test_projects_a_legacy_unnumbered_compilation_without_inventing_a_number(
        self,
    ) -> None:
        """A Register null shared by the manifest and current row is a real baseline fact."""
        title_id = "C2004A00460"
        row = manifest_row(
            id=title_id,
            name="A New Tax System (Goods and Services Tax Imposition—Customs) Act 1999",
            versionStart="2005-07-01",
            compilationNumber=None,
            sourceUrl=(
                f"https://www.legislation.gov.au/{title_id}/"
                "2005-07-01/2005-07-01/text/original/epub"
            ),
        )
        response = version_body(
            title_id,
            "2005-07-01",
            None,
            "C2005C00389",
            registered_at="2005-07-01T10:15:01",
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes([row]))
            paths = capture_register_run(
                manifest_path,
                root / "capture",
                session=MemorySession(
                    {
                        current_url_for(title_id): [
                            successful_exchange(response, "2026-08-29T00:00:03Z")
                        ]
                    }
                ),
            )

            baseline = json.loads(paths["baseline"].read_bytes())
            observation = json.loads(paths["observation"].read_bytes())
            self.assertIsNone(baseline["titles"][0]["compilation_number"])
            self.assertEqual(observation["observations"][0]["state"], "UNCHANGED")
            self.assertEqual(observation["run_status"], "VERIFIED")

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
            (
                "missing compilation number member",
                json_bytes(
                    [
                        {
                            key: value
                            for key, value in valid.items()
                            if key != "compilationNumber"
                        }
                    ]
                ),
                "compilationNumber is required",
            ),
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

    def test_manifest_path_swap_cannot_substitute_a_different_regular_file(self) -> None:
        """The descriptor read must still identify the file that passed lstat preflight."""
        replacement = manifest_row(
            id="F2020L01498",
            name="A New Tax System (Australian Business Number) Regulations 2020",
            collection="LegislativeInstrument",
            versionStart="2025-10-04",
            compilationNumber="2",
            sourceUrl=(
                "https://www.legislation.gov.au/F2020L01498/"
                "2025-10-04/2025-10-04/text/original/epub"
            ),
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "manifest.json"
            manifest.write_bytes(json_bytes([manifest_row()]))
            alternate = root / "alternate.json"
            alternate.write_bytes(json_bytes([replacement]))
            original_open = open

            def swapped_open(path: object, mode: str = "r", *args: object, **kwargs: object):
                if Path(path) == manifest and mode == "rb":
                    return original_open(alternate, mode, *args, **kwargs)
                return original_open(path, mode, *args, **kwargs)

            with (
                mock.patch("builtins.open", side_effect=swapped_open),
                self.assertRaisesRegex(CaptureRegisterError, "changed during capture"),
            ):
                capture_register_run(
                    manifest,
                    root / "capture",
                    session=MemorySession({}),
                )
            self.assertFalse((root / "capture").exists())


class RegisterTimestampCompatibilityTests(unittest.TestCase):
    """Contract fraction widths must parse identically on every supported Python."""

    def test_contract_fraction_widths_parse_on_python_3_10(self) -> None:
        seven = "2026-08-27T17:31:41.1234567+10:00"
        self.assertEqual(capture_module._version_timestamp(seven, "field"), seven)
        self.assertEqual(
            capture_module._start_date("2026-08-18T00:00:00.1234567+10:00"),
            "2026-08-18",
        )
        for fraction in ("1", "12", "1234", "12345"):
            stamp = f"2026-08-27T07:31:41.{fraction}Z"
            self.assertEqual(capture_module._utc_timestamp(stamp, "field"), stamp)
        with self.assertRaises(CaptureRegisterError):
            capture_module._version_timestamp(
                "2026-08-27T17:31:41.12345678+10:00", "field"
            )


class LiveRegisterCaptureStateTests(unittest.TestCase):
    def _capture_documents(
        self,
        exchanges: dict[str, list[RegisterExchange]],
        *,
        row: dict[str, object] | None = None,
    ) -> tuple[dict[str, object], dict[str, object], dict[str, bytes]]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes([row or manifest_row()]))
            paths = capture_register_run(
                manifest_path,
                root / "capture",
                session=MemorySession(exchanges),
            )
            evidence = {
                path.name: path.read_bytes()
                for path in (root / "capture" / "evidence").iterdir()
            }
            return (
                json.loads(paths["capture"].read_bytes()),
                json.loads(paths["observation"].read_bytes()),
                evidence,
            )

    def test_derives_every_source_state_and_run_decision(self) -> None:
        """Removing or guessing any state branch must change an observable contract field."""
        cases = [
            {
                "label": "unchanged",
                "row": manifest_row(),
                "exchanges": {
                    CURRENT_URL: [
                        successful_exchange(CURRENT_BODY, "2026-08-29T01:00:01Z")
                    ]
                },
                "state": "UNCHANGED",
                "run_status": "VERIFIED",
                "conditional": {
                    "observed_compilation_number": None,
                    "observed_compilation_date": None,
                    "observed_register_document_id": None,
                    "current_version_start": None,
                    "error_category": None,
                },
            },
            {
                "label": "superseded",
                "row": manifest_row(
                    id="F2020L01498",
                    name="A New Tax System (Australian Business Number) Regulations 2020",
                    collection="LegislativeInstrument",
                    versionStart="2025-10-04",
                    compilationNumber="2",
                    sourceUrl=(
                        "https://www.legislation.gov.au/F2020L01498/"
                        "2025-10-04/2025-10-04/text/original/epub"
                    ),
                ),
                "exchanges": {
                    INSTRUMENT_URL: [
                        successful_exchange(
                            version_body(
                                "F2020L01498", "2026-08-01", "3", "F2026C00001"
                            ),
                            "2026-08-29T01:00:02Z",
                        )
                    ]
                },
                "state": "SUPERSEDED",
                "run_status": "VERIFIED",
                "conditional": {
                    "observed_compilation_number": "3",
                    "observed_compilation_date": "2026-08-01",
                    "observed_register_document_id": "F2026C00001",
                    "current_version_start": None,
                    "error_category": None,
                },
            },
            {
                "label": "current without published compilation",
                "row": manifest_row(
                    id="F2022L00764",
                    name="ASIC Corporations Superannuation Trustees Instrument 2022/497",
                    collection="LegislativeInstrument",
                    versionStart="2023-09-01",
                    compilationNumber="2",
                    sourceUrl=(
                        "https://www.legislation.gov.au/F2022L00764/"
                        "2023-09-01/2023-09-01/text/original/epub"
                    ),
                    version_is_current=False,
                    current_version_start="2026-07-07",
                ),
                "exchanges": {
                    current_url_for("F2022L00764"): [
                        successful_exchange(
                            version_body("F2022L00764", "2026-07-07", None, None),
                            "2026-08-29T01:00:03Z",
                        )
                    ]
                },
                "state": "CURRENT_NO_PUBLISHED_COMPILATION",
                "run_status": "VERIFIED",
                "conditional": {
                    "observed_compilation_number": None,
                    "observed_compilation_date": None,
                    "observed_register_document_id": None,
                    "current_version_start": "2026-07-07",
                    "error_category": None,
                },
            },
            {
                "label": "no longer in force",
                "row": manifest_row(
                    id="F2006B07691",
                    name="Commonwealth Places Mirror Taxes Queensland Notice 2002",
                    collection="LegislativeInstrument",
                    versionStart="1997-10-06",
                    compilationNumber="0",
                    retrieved="2026-08-03",
                    sourceUrl=(
                        "https://www.legislation.gov.au/F2006B07691/"
                        "1997-10-06/1997-10-06/text/original/epub"
                    ),
                ),
                "exchanges": {
                    current_url_for("F2006B07691"): [
                        successful_exchange(
                            empty_versions_body(), "2026-08-29T01:00:04Z"
                        )
                    ],
                    history_url_for("F2006B07691"): [
                        successful_exchange(
                            version_body(
                                "F2006B07691",
                                "1997-10-06",
                                "0",
                                "F2006B07691",
                                is_current=False,
                                status="Ceased",
                            ),
                            "2026-08-29T01:00:05Z",
                        )
                    ],
                },
                "state": "NO_LONGER_IN_FORCE",
                "run_status": "VERIFIED",
                "conditional": {
                    "observed_compilation_number": None,
                    "observed_compilation_date": None,
                    "observed_register_document_id": None,
                    "current_version_start": None,
                    "error_category": None,
                },
            },
            {
                "label": "repealed current row",
                "row": manifest_row(
                    id="F2016L01256",
                    name="Repealed Instrument 2016",
                    collection="LegislativeInstrument",
                    versionStart="2016-07-29",
                    compilationNumber=None,
                    sourceUrl=(
                        "https://www.legislation.gov.au/F2016L01256/"
                        "2016-07-29/2016-07-29/text/original/epub"
                    ),
                ),
                "exchanges": {
                    current_url_for("F2016L01256"): [
                        successful_exchange(
                            version_body(
                                "F2016L01256",
                                "2026-08-12",
                                None,
                                None,
                                status="Repealed",
                            ),
                            "2026-08-29T01:00:06Z",
                        )
                    ],
                },
                "state": "NO_LONGER_IN_FORCE",
                "run_status": "VERIFIED",
                "conditional": {
                    "observed_compilation_number": None,
                    "observed_compilation_date": None,
                    "observed_register_document_id": None,
                    "current_version_start": None,
                    "error_category": None,
                },
            },
            {
                "label": "repealed row carrying a superseding shape",
                "row": manifest_row(
                    id="F2016L01523",
                    name="Repealed Instrument With Compilation 2016",
                    collection="LegislativeInstrument",
                    versionStart="2016-07-29",
                    compilationNumber="1",
                    sourceUrl=(
                        "https://www.legislation.gov.au/F2016L01523/"
                        "2016-07-29/2016-07-29/text/original/epub"
                    ),
                ),
                "exchanges": {
                    current_url_for("F2016L01523"): [
                        successful_exchange(
                            version_body(
                                "F2016L01523",
                                "2026-08-15",
                                "2",
                                "F2026C00777",
                                status="Repealed",
                            ),
                            "2026-08-29T01:00:07Z",
                        )
                    ],
                },
                "state": "NO_LONGER_IN_FORCE",
                "run_status": "VERIFIED",
                "conditional": {
                    "observed_compilation_number": None,
                    "observed_compilation_date": None,
                    "observed_register_document_id": None,
                    "current_version_start": None,
                    "error_category": None,
                },
            },
            {
                "label": "lookup failed",
                "row": manifest_row(
                    id="F2025L00178",
                    name="Family Law Superannuation Regulations 2025",
                    collection="LegislativeInstrument",
                    versionStart="2025-04-01",
                    compilationNumber="1",
                    sourceUrl=(
                        "https://www.legislation.gov.au/F2025L00178/"
                        "2025-04-01/2025-04-01/text/original/epub"
                    ),
                ),
                "exchanges": {
                    current_url_for("F2025L00178"): [
                        RegisterExchange(
                            checked_at="2026-08-29T01:00:06Z",
                            status=None,
                            headers={},
                            body=None,
                            attempts=3,
                            error_category="TRANSPORT_ERROR",
                        )
                    ]
                },
                "state": "LOOKUP_FAILED",
                "run_status": "BLOCKED",
                "conditional": {
                    "observed_compilation_number": None,
                    "observed_compilation_date": None,
                    "observed_register_document_id": None,
                    "current_version_start": None,
                    "error_category": "TRANSPORT_ERROR",
                },
            },
        ]

        for case in cases:
            with self.subTest(case=case["label"]), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                manifest_path = root / "manifest.json"
                manifest_path.write_bytes(json_bytes([case["row"]]))
                paths = capture_register_run(
                    manifest_path,
                    root / "capture",
                    session=MemorySession(case["exchanges"]),
                )
                observation = json.loads(paths["observation"].read_bytes())
                self.assertTrue(observation["complete"])
                self.assertEqual(observation["run_status"], case["run_status"])
                item = observation["observations"][0]
                self.assertEqual(item["state"], case["state"])
                for field, expected in case["conditional"].items():
                    self.assertEqual(item[field], expected, field)

    def test_empty_current_result_uses_the_exact_ordered_history_query(self) -> None:
        """Dropping or changing the fallback query would confuse repeal with lookup failure."""
        current = successful_exchange(empty_versions_body(), "2026-08-29T02:00:01Z")
        history = successful_exchange(
            version_body(
                "C2004A00467",
                "2025-01-01",
                "31",
                "C2025C00001",
                is_current=False,
                status="Repealed",
            ),
            "2026-08-29T02:00:02Z",
        )
        session = MemorySession({CURRENT_URL: [current], HISTORY_URL: [history]})
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes([manifest_row()]))
            paths = capture_register_run(manifest_path, root / "capture", session=session)

            self.assertEqual(session.urls, [CURRENT_URL, HISTORY_URL])
            observation = json.loads(paths["observation"].read_bytes())
            self.assertEqual(
                observation["observations"][0]["state"], "NO_LONGER_IN_FORCE"
            )

    def test_lookup_failure_does_not_prevent_later_scope_attempts(self) -> None:
        """A failed first title must not silently shrink or abort the audit scope."""
        failed = manifest_row()
        later = manifest_row(
            id="F2020L01498",
            name="A New Tax System (Australian Business Number) Regulations 2020",
            collection="LegislativeInstrument",
            versionStart="2025-10-04",
            compilationNumber="2",
            sourceUrl=(
                "https://www.legislation.gov.au/F2020L01498/"
                "2025-10-04/2025-10-04/text/original/epub"
            ),
        )
        session = MemorySession(
            {
                CURRENT_URL: [
                    RegisterExchange(
                        checked_at="2026-08-29T03:00:01Z",
                        status=None,
                        headers={},
                        body=None,
                        attempts=3,
                        error_category="TRANSPORT_ERROR",
                    )
                ],
                INSTRUMENT_URL: [
                    successful_exchange(
                        version_body("F2020L01498", "2025-10-04", "2", "F2025C00987"),
                        "2026-08-29T03:00:02Z",
                    )
                ],
            }
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes([later, failed]))
            paths = capture_register_run(manifest_path, root / "capture", session=session)

            capture = json.loads(paths["capture"].read_bytes())
            observation = json.loads(paths["observation"].read_bytes())
            self.assertEqual(session.urls, [CURRENT_URL, INSTRUMENT_URL])
            self.assertTrue(capture["complete"])
            self.assertTrue(observation["complete"])
            self.assertEqual(observation["run_status"], "BLOCKED")
            self.assertEqual(
                [item["state"] for item in observation["observations"]],
                ["LOOKUP_FAILED", "UNCHANGED"],
            )

    def test_invalid_responses_use_fixed_fail_closed_categories(self) -> None:
        """Malformed source facts must never be coerced into a valid development state."""
        valid_document = json.loads(CURRENT_BODY)
        valid_row = valid_document["value"][0]

        def with_row(
            changes: dict[str, object] | None = None,
            *,
            remove: str | None = None,
            extra: bool = False,
        ) -> bytes:
            row = dict(valid_row)
            row.update(changes or {})
            if remove is not None:
                del row[remove]
            if extra:
                row["unexpected"] = "member"
            return json.dumps(
                {"@odata.context": valid_document["@odata.context"], "value": [row]},
                separators=(",", ":"),
            ).encode("utf-8")

        def exchange(
            body: bytes | None,
            *,
            status: int | None = 200,
            content_type: str = CONTENT_TYPE,
            checked_at: str = "2026-08-29T04:00:00Z",
            attempts: int = 1,
            error_category: str | None = None,
            headers: dict[str, str] | None = None,
        ) -> RegisterExchange:
            return RegisterExchange(
                checked_at=checked_at,
                status=status,
                headers=(
                    {"Content-Type": content_type, "OData-Version": "4.0"}
                    if headers is None
                    else headers
                ),
                body=body,
                attempts=attempts,
                error_category=error_category,
            )

        two_rows = json.dumps(
            {
                "@odata.context": valid_document["@odata.context"],
                "value": [valid_row, valid_row],
            },
            separators=(",", ":"),
        ).encode("utf-8")
        invalid_envelope = json.dumps(
            {
                "@odata.context": valid_document["@odata.context"],
                "value": [valid_row],
                "unexpected": True,
            },
            separators=(",", ":"),
        ).encode("utf-8")
        wrong_context = json.dumps(
            {"@odata.context": "https://example.invalid/$metadata", "value": [valid_row]},
            separators=(",", ":"),
        ).encode("utf-8")
        duplicate_json = CURRENT_BODY.replace(
            b'"titleId":"C2004A00467",',
            b'"titleId":"C2004A00467","titleId":"C2004A00467",',
            1,
        )
        cases = [
            (
                "transport",
                exchange(None, status=None, error_category="TRANSPORT_ERROR", headers={}),
                "TRANSPORT_ERROR",
            ),
            ("non-200", exchange(b"not retained", status=503), "HTTP_STATUS"),
            (
                "oversized",
                exchange(b"x" * ((256 * 1024) + 1)),
                "RESPONSE_TOO_LARGE",
            ),
            (
                "wrong media",
                exchange(CURRENT_BODY, content_type="text/html; charset=utf-8"),
                "UNSUPPORTED_MEDIA_TYPE",
            ),
            ("invalid utf8", exchange(b"\xff"), "INVALID_JSON"),
            ("malformed json", exchange(b'{"value":'), "INVALID_JSON"),
            ("duplicate json member", exchange(duplicate_json), "INVALID_JSON"),
            ("extra envelope member", exchange(invalid_envelope), "INVALID_ODATA_SHAPE"),
            ("wrong context", exchange(wrong_context), "INVALID_ODATA_SHAPE"),
            ("two rows", exchange(two_rows), "INVALID_ODATA_SHAPE"),
            ("missing row field", exchange(with_row(remove="status")), "INVALID_ODATA_SHAPE"),
            ("extra row field", exchange(with_row(extra=True)), "INVALID_ODATA_SHAPE"),
            (
                "wrong title",
                exchange(with_row({"titleId": "C2004A00468"})),
                "IDENTITY_MISMATCH",
            ),
            (
                "non-boolean current",
                exchange(with_row({"isCurrent": "true"})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "unknown status",
                exchange(with_row({"status": "Current"})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "non-scalar status",
                exchange(with_row({"status": []})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "malformed start",
                exchange(with_row({"start": "not-a-date"})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "date-only start",
                exchange(with_row({"start": "2026-06-30"})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "empty compilation",
                exchange(with_row({"compilationNumber": ""})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "non-string compilation",
                exchange(with_row({"compilationNumber": 32})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "malformed document id",
                exchange(with_row({"registerId": "../../document"})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "missing registration timestamp",
                exchange(with_row({"registeredAt": None})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "date-only registration timestamp",
                exchange(with_row({"registeredAt": "2026-07-14"})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "registration without document",
                exchange(with_row({"registerId": None})),
                "INVALID_ODATA_SHAPE",
            ),
            (
                "different compilation on equal date",
                exchange(with_row({"compilationNumber": "33"})),
                "INCONSISTENT_CHRONOLOGY",
            ),
            (
                "same compilation on later date",
                exchange(with_row({"start": "2026-07-01T00:00:00"})),
                "INCONSISTENT_CHRONOLOGY",
            ),
            (
                "backwards date",
                exchange(
                    with_row(
                        {
                            "start": "2026-06-29T00:00:00",
                            "compilationNumber": "33",
                        }
                    )
                ),
                "INCONSISTENT_CHRONOLOGY",
            ),
            (
                "malformed checked timestamp",
                exchange(CURRENT_BODY, checked_at="local-time"),
                "INVALID_EXCHANGE",
            ),
            ("zero attempts", exchange(CURRENT_BODY, attempts=0), "INVALID_EXCHANGE"),
            ("too many attempts", exchange(CURRENT_BODY, attempts=4), "INVALID_EXCHANGE"),
            (
                "unsafe selected header",
                exchange(
                    CURRENT_BODY,
                    headers={"Content-Type": "application/json\r\nInjected: yes"},
                ),
                "INVALID_EXCHANGE",
            ),
            (
                "ambiguous selected header",
                exchange(
                    CURRENT_BODY,
                    headers={
                        "Content-Type": "application/json",
                        "content-type": "text/html",
                    },
                ),
                "INVALID_EXCHANGE",
            ),
            (
                "status and transport contradiction",
                exchange(CURRENT_BODY, error_category="TRANSPORT_ERROR"),
                "INVALID_EXCHANGE",
            ),
            (
                "unbounded transport category",
                exchange(None, status=None, error_category="socket exploded", headers={}),
                "INVALID_EXCHANGE",
            ),
        ]

        for label, source_exchange, expected_category in cases:
            with self.subTest(case=label):
                capture, observation, _evidence = self._capture_documents(
                    {CURRENT_URL: [source_exchange]}
                )
                result = capture["results"][0]
                item = observation["observations"][0]
                self.assertEqual(result["state"], "LOOKUP_FAILED")
                self.assertEqual(result["error_category"], expected_category)
                self.assertEqual(item["state"], "LOOKUP_FAILED")
                self.assertEqual(item["error_category"], expected_category)
                self.assertEqual(observation["run_status"], "BLOCKED")

    def test_no_current_or_historical_version_is_insufficient_evidence(self) -> None:
        """Two successful empty responses must not be reported as a legal cessation fact."""
        current = successful_exchange(empty_versions_body(), "2026-08-29T05:00:01Z")
        history = successful_exchange(empty_versions_body(), "2026-08-29T05:00:02Z")
        capture, observation, evidence = self._capture_documents(
            {CURRENT_URL: [current], HISTORY_URL: [history]}
        )
        self.assertEqual(capture["results"][0]["error_category"], "NO_VERSION_EVIDENCE")
        self.assertEqual(observation["observations"][0]["state"], "LOOKUP_FAILED")
        self.assertEqual(len(evidence), 1)

    def test_bounded_malformed_200_body_is_retained_as_failure_evidence(self) -> None:
        """Removing malformed source bytes would make the closed decision unauditable."""
        malformed = b'{"value":'
        source_exchange = successful_exchange(malformed, "2026-08-29T06:00:01Z")
        capture, observation, evidence = self._capture_documents(
            {CURRENT_URL: [source_exchange]}
        )
        request = capture["results"][0]["requests"][0]
        digest = sha256_id(malformed)
        self.assertEqual(request["response_sha256"], digest)
        self.assertEqual(request["response_length"], len(malformed))
        self.assertEqual(
            request["evidence_path"],
            f"evidence/sha256-{digest.removeprefix('sha256:')}.json",
        )
        self.assertEqual(list(evidence.values()), [malformed])
        item = observation["observations"][0]
        self.assertEqual(item["primary_response_sha256"], digest)
        self.assertEqual(item["primary_response_media_type"], "application/json")

    def test_non_200_body_is_neither_retained_nor_disclosed(self) -> None:
        """A server error body must not cross the capture's bounded evidence contract."""
        secret_body = b"PRIVATE-SERVER-BODY-MUST-NOT-APPEAR"
        source_exchange = RegisterExchange(
            checked_at="2026-08-29T07:00:01Z",
            status=503,
            headers={"Content-Type": "text/plain", "Server": "ignored"},
            body=secret_body,
            attempts=3,
        )
        capture, observation, evidence = self._capture_documents(
            {CURRENT_URL: [source_exchange]}
        )
        request = capture["results"][0]["requests"][0]
        self.assertIsNone(request["response_length"])
        self.assertIsNone(request["response_sha256"])
        self.assertIsNone(request["evidence_path"])
        self.assertEqual(evidence, {})
        item = observation["observations"][0]
        self.assertIsNone(item["primary_response_sha256"])
        self.assertIsNone(item["primary_response_media_type"])
        serialised = json.dumps([capture, observation]).encode("utf-8")
        self.assertNotIn(secret_body, serialised)


class LiveRegisterProductionSessionTests(unittest.TestCase):
    def _run_with_production_session(
        self,
        opener: FakeOpener,
        clock: FakeClock,
        timestamps: list[str],
        *,
        rows: list[dict[str, object]] | None = None,
    ) -> tuple[dict[str, object], dict[str, object], Path, list[object]]:
        captured_handlers: list[object] = []

        def build_opener(*handlers: object) -> FakeOpener:
            captured_handlers.extend(handlers)
            return opener

        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        manifest_path = root / "manifest.json"
        manifest_path.write_bytes(json_bytes(rows or [manifest_row()]))
        with (
            mock.patch.object(capture_module.urllib.request, "build_opener", build_opener),
            mock.patch.object(capture_module.time, "monotonic", clock.monotonic),
            mock.patch.object(capture_module.time, "sleep", clock.sleep),
            mock.patch.object(capture_module, "_utc_now", side_effect=timestamps),
        ):
            paths = capture_register_run(manifest_path, root / "capture")
        return (
            json.loads(paths["capture"].read_bytes()),
            json.loads(paths["observation"].read_bytes()),
            root / "capture" / "evidence",
            captured_handlers,
        )

    def test_retries_transport_and_retryable_status_with_fixed_delays(self) -> None:
        """Removing an attempt, timeout, identity header or retry delay must fail this case."""
        retry_status = urllib.error.HTTPError(
            CURRENT_URL,
            503,
            "Service Unavailable",
            {"Content-Type": "text/plain", "Server": "ignored"},
            io.BytesIO(b"server body must not be read"),
        )
        response = FakeHttpResponse(
            CURRENT_BODY,
            headers={
                "Content-Type": CONTENT_TYPE,
                "OData-Version": "4.0",
                "X-Frl-Version": "2026.08.13-releaseyaml.1",
                "Server": "must-not-be-retained",
            },
        )
        opener = FakeOpener(
            [urllib.error.URLError("private socket detail"), retry_status, response]
        )
        clock = FakeClock()

        capture, observation, evidence, handlers = self._run_with_production_session(
            opener,
            clock,
            ["2026-08-29T08:00:00Z", "2026-08-29T08:00:13Z"],
        )

        self.assertEqual(clock.sleeps, [6.0, 6.0])
        self.assertEqual(len(opener.calls), 3)
        self.assertEqual([timeout for _request, timeout in opener.calls], [90, 90, 90])
        for request, _timeout in opener.calls:
            self.assertEqual(request.full_url, CURRENT_URL)
            self.assertEqual(
                request.get_header("User-agent"),
                "au-tax-legislation-corpus (+https://github.com/ryanduguid/au-tax-legislation-corpus)",
            )
        self.assertEqual(response.read_sizes, [(256 * 1024) + 1])
        self.assertEqual(len(handlers), 1)
        self.assertEqual(observation["run_status"], "VERIFIED")
        request_record = capture["results"][0]["requests"][0]
        self.assertEqual(request_record["attempt_count"], 3)
        self.assertEqual(
            request_record["response_headers"],
            {
                "content-type": CONTENT_TYPE,
                "odata-version": "4.0",
                "x-frl-version": "2026.08.13-releaseyaml.1",
            },
        )
        self.assertEqual(len(list(evidence.iterdir())), 1)
        self.assertNotIn("private socket detail", json.dumps([capture, observation]))

    def test_paces_separate_register_requests_without_wall_clock_sleep(self) -> None:
        """A second title must start no sooner than 1.5 seconds after the first request."""
        instrument = manifest_row(
            id="F2020L01498",
            name="A New Tax System (Australian Business Number) Regulations 2020",
            collection="LegislativeInstrument",
            versionStart="2025-10-04",
            compilationNumber="2",
            sourceUrl=(
                "https://www.legislation.gov.au/F2020L01498/"
                "2025-10-04/2025-10-04/text/original/epub"
            ),
        )
        second_body = version_body(
            "F2020L01498", "2025-10-04", "2", "F2025C00987"
        )
        opener = FakeOpener(
            [FakeHttpResponse(CURRENT_BODY), FakeHttpResponse(second_body)]
        )
        clock = FakeClock()

        _capture, observation, _evidence, _handlers = self._run_with_production_session(
            opener,
            clock,
            [
                "2026-08-29T09:00:00Z",
                "2026-08-29T09:00:01Z",
                "2026-08-29T09:00:03Z",
            ],
            rows=[instrument, manifest_row()],
        )

        self.assertEqual(clock.sleeps, [1.5])
        self.assertEqual([call[0].full_url for call in opener.calls], [CURRENT_URL, INSTRUMENT_URL])
        self.assertEqual(observation["run_status"], "VERIFIED")

    def test_redirect_is_not_followed_retried_or_retained(self) -> None:
        """A hostile Location response must remain one failed exchange on the exact host."""
        redirect = urllib.error.HTTPError(
            CURRENT_URL,
            302,
            "Found",
            {
                "Location": "https://evil.example/private",
                "Content-Type": "text/html",
            },
            io.BytesIO(b"redirect body must not be retained"),
        )
        opener = FakeOpener([redirect])
        clock = FakeClock()

        capture, observation, evidence, handlers = self._run_with_production_session(
            opener,
            clock,
            ["2026-08-29T10:00:00Z", "2026-08-29T10:00:01Z"],
        )

        self.assertEqual(len(opener.calls), 1)
        self.assertEqual(clock.sleeps, [])
        self.assertEqual(len(list(evidence.iterdir())), 0)
        self.assertEqual(observation["run_status"], "BLOCKED")
        self.assertEqual(capture["results"][0]["error_category"], "HTTP_STATUS")
        self.assertEqual(len(handlers), 1)
        redirect_request = handlers[0].redirect_request(
            None, None, 302, "Found", {}, CURRENT_URL
        )
        self.assertIsNone(redirect_request)
        serialised = json.dumps([capture, observation])
        self.assertNotIn("evil.example", serialised)
        self.assertNotIn("redirect body", serialised)

    def test_reads_only_one_byte_beyond_the_response_limit(self) -> None:
        """An oversized response must be detected without reading an unbounded body."""
        oversized = b"x" * ((256 * 1024) + 100)
        response = FakeHttpResponse(oversized)
        opener = FakeOpener([response])
        clock = FakeClock()

        capture, observation, evidence, _handlers = self._run_with_production_session(
            opener,
            clock,
            ["2026-08-29T11:00:00Z", "2026-08-29T11:00:01Z"],
        )

        self.assertEqual(response.read_sizes, [(256 * 1024) + 1])
        self.assertEqual(capture["results"][0]["error_category"], "RESPONSE_TOO_LARGE")
        self.assertEqual(observation["run_status"], "BLOCKED")
        self.assertEqual(len(list(evidence.iterdir())), 0)


class LiveRegisterEvidenceGraphTests(unittest.TestCase):
    def test_identical_failed_response_bytes_share_one_evidence_object(self) -> None:
        """Dropping content addressing would create duplicate mutable evidence identities."""
        malformed = b'{"same-malformed-response":'
        later = manifest_row(
            id="F2020L01498",
            name="A New Tax System (Australian Business Number) Regulations 2020",
            collection="LegislativeInstrument",
            versionStart="2025-10-04",
            compilationNumber="2",
            sourceUrl=(
                "https://www.legislation.gov.au/F2020L01498/"
                "2025-10-04/2025-10-04/text/original/epub"
            ),
        )
        session = MemorySession(
            {
                CURRENT_URL: [
                    successful_exchange(malformed, "2026-08-29T12:00:01Z")
                ],
                INSTRUMENT_URL: [
                    successful_exchange(malformed, "2026-08-29T12:00:02Z")
                ],
            }
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes([later, manifest_row()]))
            paths = capture_register_run(manifest_path, root / "capture", session=session)

            capture = json.loads(paths["capture"].read_bytes())
            evidence_files = list((root / "capture" / "evidence").iterdir())
            self.assertEqual(len(evidence_files), 1)
            self.assertEqual(evidence_files[0].read_bytes(), malformed)
            declared = [
                result["requests"][0]["response_sha256"]
                for result in capture["results"]
            ]
            self.assertEqual(declared, [sha256_id(malformed), sha256_id(malformed)])

    def test_repeated_inputs_exchanges_and_timestamps_are_byte_deterministic(self) -> None:
        """Random staging names must never leak into the immutable graph bytes."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes([manifest_row()]))
            outputs: list[dict[str, bytes]] = []
            for index in range(2):
                session = MemorySession(
                    {
                        CURRENT_URL: [
                            successful_exchange(CURRENT_BODY, "2026-08-29T13:00:01Z")
                        ]
                    }
                )
                destination = root / f"capture-{index}"
                capture_register_run(manifest_path, destination, session=session)
                outputs.append(
                    {
                        path.relative_to(destination).as_posix(): path.read_bytes()
                        for path in destination.rglob("*")
                        if path.is_file()
                    }
                )
            self.assertEqual(outputs[0], outputs[1])

    def test_final_revalidation_blocks_every_cross_file_tamper(self) -> None:
        """A staged byte or declaration change must prevent the official rename."""
        original_validator = capture_module._validate_staged_graph

        def mutate_baseline(staging: Path) -> None:
            path = staging / "monitor-baseline.json"
            path.write_bytes(path.read_bytes() + b" ")

        def mutate_capture_digest(staging: Path) -> None:
            path = staging / "register-capture.json"
            document = json.loads(path.read_bytes())
            document["baseline_sha256"] = f"sha256:{'0' * 64}"
            path.write_bytes(json_bytes(document))

        def mutate_observation_digest(staging: Path) -> None:
            path = staging / "register-observation.json"
            document = json.loads(path.read_bytes())
            document["capture_sha256"] = f"sha256:{'0' * 64}"
            path.write_bytes(json_bytes(document))

        def mutate_evidence(staging: Path) -> None:
            path = next((staging / "evidence").iterdir())
            path.write_bytes(path.read_bytes() + b"tamper")

        def add_undeclared_evidence(staging: Path) -> None:
            (staging / "evidence" / f"sha256-{'0' * 64}.json").write_bytes(b"extra")

        def escape_relative_evidence_path(staging: Path) -> None:
            capture_path = staging / "register-capture.json"
            capture_document = json.loads(capture_path.read_bytes())
            capture_document["results"][0]["requests"][0]["evidence_path"] = (
                "../outside.json"
            )
            capture_content = json_bytes(capture_document)
            capture_path.write_bytes(capture_content)
            observation_path = staging / "register-observation.json"
            observation_document = json.loads(observation_path.read_bytes())
            observation_document["capture_sha256"] = sha256_id(capture_content)
            observation_path.write_bytes(json_bytes(observation_document))

        mutators = {
            "baseline bytes": mutate_baseline,
            "capture declaration": mutate_capture_digest,
            "observation declaration": mutate_observation_digest,
            "evidence bytes": mutate_evidence,
            "undeclared evidence": add_undeclared_evidence,
            "escaping evidence path": escape_relative_evidence_path,
        }
        for label, mutate in mutators.items():
            with self.subTest(case=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                manifest_path = root / "manifest.json"
                manifest_path.write_bytes(json_bytes([manifest_row()]))
                destination = root / "capture"
                session = MemorySession(
                    {
                        CURRENT_URL: [
                            successful_exchange(CURRENT_BODY, "2026-08-29T14:00:01Z")
                        ]
                    }
                )

                def tampering_validator(staging: Path) -> None:
                    mutate(staging)
                    original_validator(staging)

                with (
                    mock.patch.object(
                        capture_module,
                        "_validate_staged_graph",
                        side_effect=tampering_validator,
                    ),
                    self.assertRaisesRegex(CaptureRegisterError, "staged capture"),
                ):
                    capture_register_run(
                        manifest_path,
                        destination,
                        session=session,
                    )
                self.assertFalse(destination.exists())
                self.assertEqual(
                    list(root.glob(".capture.register-capture-*.tmp")), []
                )

    def test_revalidation_checks_every_declaration_of_deduplicated_evidence(self) -> None:
        """A correct later declaration must not mask an earlier wrong length."""
        malformed = b'{"shared-invalid-response":'
        later = manifest_row(
            id="F2020L01498",
            name="A New Tax System (Australian Business Number) Regulations 2020",
            collection="LegislativeInstrument",
            versionStart="2025-10-04",
            compilationNumber="2",
            sourceUrl=(
                "https://www.legislation.gov.au/F2020L01498/"
                "2025-10-04/2025-10-04/text/original/epub"
            ),
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes([later, manifest_row()]))
            destination = root / "capture"
            original_validator = capture_module._validate_staged_graph

            def inconsistent_duplicate(staging: Path) -> None:
                capture_path = staging / "register-capture.json"
                capture_document = json.loads(capture_path.read_bytes())
                first_result = capture_document["results"][0]
                first_request = first_result["requests"][0]
                first_request["response_length"] += 1
                capture_content = json_bytes(capture_document)
                capture_path.write_bytes(capture_content)

                observation_path = staging / "register-observation.json"
                observation_document = json.loads(observation_path.read_bytes())
                result_digest = sha256_id(json_bytes(first_result))
                first_observation = observation_document["observations"][0]
                first_observation["capture_result_sha256"] = result_digest
                first_observation["evidence_id"] = (
                    f"frl:C2004A00467:{result_digest.removeprefix('sha256:')[:32]}"
                )
                observation_document["capture_sha256"] = sha256_id(capture_content)
                observation_path.write_bytes(json_bytes(observation_document))
                original_validator(staging)

            with (
                mock.patch.object(
                    capture_module,
                    "_validate_staged_graph",
                    side_effect=inconsistent_duplicate,
                ),
                self.assertRaisesRegex(CaptureRegisterError, "staged capture"),
            ):
                capture_register_run(
                    manifest_path,
                    destination,
                    session=MemorySession(
                        {
                            CURRENT_URL: [
                                successful_exchange(
                                    malformed, "2026-08-29T14:30:01Z"
                                )
                            ],
                            INSTRUMENT_URL: [
                                successful_exchange(
                                    malformed, "2026-08-29T14:30:02Z"
                                )
                            ],
                        }
                    ),
                )
            self.assertFalse(destination.exists())

    def test_revalidation_binds_each_read_to_the_staged_file_it_inspected(self) -> None:
        """A swapped descriptor must not conceal corrupt bytes that will be promoted."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes([manifest_row()]))
            destination = root / "capture"
            alternate = root / "alternate-capture.json"
            original_validator = capture_module._validate_staged_graph
            original_open = open

            def swapping_validator(staging: Path) -> None:
                capture_path = staging / "register-capture.json"
                alternate.write_bytes(capture_path.read_bytes())
                capture_path.write_bytes(capture_path.read_bytes() + b" ")

                def swapped_open(
                    path: object,
                    mode: str = "r",
                    *args: object,
                    **kwargs: object,
                ):
                    if Path(path) == capture_path and mode == "rb":
                        return original_open(alternate, mode, *args, **kwargs)
                    return original_open(path, mode, *args, **kwargs)

                with mock.patch("builtins.open", side_effect=swapped_open):
                    original_validator(staging)

            with (
                mock.patch.object(
                    capture_module,
                    "_validate_staged_graph",
                    side_effect=swapping_validator,
                ),
                self.assertRaisesRegex(CaptureRegisterError, "staged capture"),
            ):
                capture_register_run(
                    manifest_path,
                    destination,
                    session=MemorySession(
                        {
                            CURRENT_URL: [
                                successful_exchange(
                                    CURRENT_BODY, "2026-08-29T14:45:01Z"
                                )
                            ]
                        }
                    ),
                )
            self.assertFalse(destination.exists())


    def test_public_graph_validator_accepts_generated_graph_and_rejects_mutation(self) -> None:
        """The exporter needs the same final Stage 3A proof as capture promotion."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "manifest.json"
            manifest_path.write_bytes(json_bytes([manifest_row()]))
            destination = root / "capture"
            capture_register_run(
                manifest_path,
                destination,
                session=MemorySession(
                    {
                        CURRENT_URL: [
                            successful_exchange(CURRENT_BODY, "2026-08-29T14:45:01Z")
                        ]
                    }
                ),
            )

            validate_capture_graph(destination)
            baseline = destination / "monitor-baseline.json"
            baseline.write_bytes(baseline.read_bytes() + b" ")

            with self.assertRaisesRegex(CaptureRegisterError, "staged capture"):
                validate_capture_graph(destination)


class LiveRegisterCaptureFilesystemTests(unittest.TestCase):
    def _manifest(self, root: Path, *, content: bytes | None = None) -> Path:
        path = root / "manifest.json"
        path.write_bytes(content or json_bytes([manifest_row()]))
        return path

    def _session(self) -> MemorySession:
        return MemorySession(
            {
                CURRENT_URL: [
                    successful_exchange(CURRENT_BODY, "2026-08-29T15:00:01Z")
                ]
            }
        )

    def test_creates_exactly_one_missing_output_parent_after_validation(self) -> None:
        """One safe missing parent is supported; deeper missing ancestry must stay absent."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self._manifest(root)
            parent = root / "new-parent"
            destination = parent / "capture"

            paths = capture_register_run(manifest, destination, session=self._session())

            self.assertTrue(parent.is_dir())
            self.assertEqual(paths["capture"], destination / "register-capture.json")

            deep_parent = root / "missing-one" / "missing-two"
            with self.assertRaisesRegex(CaptureRegisterError, "one missing output parent"):
                capture_register_run(
                    manifest,
                    deep_parent / "capture",
                    session=self._session(),
                )
            self.assertFalse((root / "missing-one").exists())

    def test_invalid_manifest_never_creates_the_permitted_missing_parent(self) -> None:
        """Filesystem mutation must occur only after the source snapshot validates."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self._manifest(root, content=b"[]")
            parent = root / "new-parent"
            with self.assertRaisesRegex(CaptureRegisterError, "non-empty array"):
                capture_register_run(
                    manifest,
                    parent / "capture",
                    session=self._session(),
                )
            self.assertFalse(parent.exists())

    def test_rejects_unsafe_or_existing_destinations_without_mutation(self) -> None:
        """No overwrite or ambiguous output name may enter the capture transaction."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self._manifest(root)
            for index, name in enumerate((".hidden", "bad name", "-option")):
                with self.subTest(name=name):
                    destination = root / name
                    with self.assertRaisesRegex(CaptureRegisterError, "name is unsafe"):
                        capture_register_run(
                            manifest,
                            destination,
                            session=self._session(),
                        )
                    self.assertFalse(destination.exists(), index)

            existing_file = root / "existing-file"
            existing_file.write_bytes(b"prior bytes")
            existing_directory = root / "existing-directory"
            existing_directory.mkdir()
            sentinel = existing_directory / "sentinel.txt"
            sentinel.write_bytes(b"prior directory")
            for destination in (existing_file, existing_directory, manifest):
                with self.subTest(destination=destination.name):
                    with self.assertRaisesRegex(CaptureRegisterError, "must not exist"):
                        capture_register_run(
                            manifest,
                            destination,
                            session=self._session(),
                        )
            self.assertEqual(existing_file.read_bytes(), b"prior bytes")
            self.assertEqual(sentinel.read_bytes(), b"prior directory")
            self.assertEqual(manifest.read_bytes(), json_bytes([manifest_row()]))

    def test_rejects_linked_manifest_ancestor_and_output_parent(self) -> None:
        """A linked ancestor must not redirect either the source or the official output."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            real_source = root / "real-source"
            real_source.mkdir()
            manifest = self._manifest(real_source)
            source_link = root / "source-link"
            real_output = root / "real-output"
            real_output.mkdir()
            output_link = root / "output-link"
            try:
                source_link.symlink_to(real_source, target_is_directory=True)
                output_link.symlink_to(real_output, target_is_directory=True)
            except OSError as exc:
                self.skipTest(f"symbolic links are unavailable: {exc}")

            with self.assertRaisesRegex(CaptureRegisterError, "ordinary ancestors"):
                capture_register_run(
                    source_link / "manifest.json",
                    root / "capture-source",
                    session=self._session(),
                )
            with self.assertRaisesRegex(CaptureRegisterError, "ordinary directory"):
                capture_register_run(
                    manifest,
                    output_link / "capture",
                    session=self._session(),
                )
            self.assertEqual(list(real_output.iterdir()), [])

    def test_reparse_output_parent_is_rejected_before_network_or_write(self) -> None:
        """Windows reparse metadata must fail even when the object otherwise looks like a directory."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self._manifest(root)
            output_parent = root / "output"
            output_parent.mkdir()
            real_lstat = os.lstat

            def marked_lstat(path: object) -> object:
                details = real_lstat(path)
                if Path(path) == output_parent:
                    marked = mock.Mock(wraps=details)
                    marked.st_mode = details.st_mode
                    marked.st_file_attributes = 0x400
                    return marked
                return details

            with (
                mock.patch.object(capture_module.os, "lstat", side_effect=marked_lstat),
                self.assertRaisesRegex(CaptureRegisterError, "ordinary directory"),
            ):
                capture_register_run(
                    manifest,
                    output_parent / "capture",
                    session=self._session(),
                )
            self.assertEqual(list(output_parent.iterdir()), [])

    def test_raced_destination_is_preserved_and_staging_is_removed(self) -> None:
        """A destination appearing before promotion belongs to another writer."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self._manifest(root)
            destination = root / "capture"
            original_validator = capture_module._validate_staged_graph

            def race_after_validation(staging: Path) -> None:
                original_validator(staging)
                destination.mkdir()
                (destination / "other-writer.txt").write_bytes(b"preserve me")

            with (
                mock.patch.object(
                    capture_module,
                    "_validate_staged_graph",
                    side_effect=race_after_validation,
                ),
                self.assertRaisesRegex(CaptureRegisterError, "must not exist"),
            ):
                capture_register_run(manifest, destination, session=self._session())
            self.assertEqual(
                (destination / "other-writer.txt").read_bytes(), b"preserve me"
            )
            self.assertEqual(list(root.glob(".capture.register-capture-*.tmp")), [])

    def test_write_and_promotion_failures_remove_only_owned_staging(self) -> None:
        """Handled output failures must leave unrelated siblings and the destination untouched."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = self._manifest(root)
            unrelated = root / "unrelated.txt"
            unrelated.write_bytes(b"keep")

            destination = root / "write-failure"
            with (
                mock.patch.object(
                    capture_module,
                    "_write_new",
                    side_effect=OSError("private write detail"),
                ),
                self.assertRaisesRegex(CaptureRegisterError, "could not be written"),
            ):
                capture_register_run(manifest, destination, session=self._session())
            self.assertFalse(destination.exists())
            self.assertEqual(list(root.glob(".write-failure.register-capture-*.tmp")), [])

            destination = root / "promotion-failure"
            with (
                mock.patch.object(
                    capture_module.os,
                    "rename",
                    side_effect=OSError("private rename detail"),
                ),
                self.assertRaisesRegex(CaptureRegisterError, "could not be promoted"),
            ):
                capture_register_run(manifest, destination, session=self._session())
            self.assertFalse(destination.exists())
            self.assertEqual(
                list(root.glob(".promotion-failure.register-capture-*.tmp")), []
            )
            self.assertEqual(unrelated.read_bytes(), b"keep")


class LiveRegisterCaptureCliTests(unittest.TestCase):
    def test_cli_captures_and_reports_the_three_contract_paths(self) -> None:
        """Breaking argument delegation or path reporting must fail the public command."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "manifest.json"
            manifest.write_bytes(json_bytes([manifest_row()]))
            destination = root / "capture"
            session = MemorySession(
                {
                    CURRENT_URL: [
                        successful_exchange(CURRENT_BODY, "2026-08-29T16:00:01Z")
                    ]
                }
            )
            stdout = StringIO()
            with (
                mock.patch.object(
                    capture_module,
                    "_HttpsRegisterSession",
                    return_value=session,
                ),
                redirect_stdout(stdout),
            ):
                result = capture_module.main(
                    [str(manifest), "--out", str(destination)]
                )

            self.assertEqual(result, 0)
            self.assertEqual(
                stdout.getvalue().splitlines(),
                [
                    f"baseline: {destination / 'monitor-baseline.json'}",
                    f"capture: {destination / 'register-capture.json'}",
                    f"observation: {destination / 'register-observation.json'}",
                ],
            )
            self.assertTrue((destination / "register-capture.json").is_file())

    def test_cli_uses_exit_two_and_a_bounded_error_for_invalid_input(self) -> None:
        """Local paths and source bytes must not leak through an argparse failure."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "manifest.json"
            manifest.write_bytes(b"[]")
            stderr = StringIO()
            with (
                mock.patch.object(
                    capture_module,
                    "_HttpsRegisterSession",
                    return_value=MemorySession({}),
                ),
                redirect_stderr(stderr),
                self.assertRaises(SystemExit) as raised,
            ):
                capture_module.main(
                    [str(manifest), "--out", str(root / "capture")]
                )
            self.assertEqual(raised.exception.code, 2)
            self.assertIn("manifest must be a non-empty array", stderr.getvalue())
            self.assertNotIn(str(root), stderr.getvalue())

    def test_cli_requires_manifest_and_output_arguments(self) -> None:
        """An accidental implicit source or destination would make a live run ambiguous."""
        with redirect_stderr(StringIO()), self.assertRaises(SystemExit) as raised:
            capture_module.main([])
        self.assertEqual(raised.exception.code, 2)


class LiveRegisterPublicationBoundaryTests(unittest.TestCase):
    def test_existing_publisher_rejects_live_v4_observation(self) -> None:
        """Stage 3A evidence must not become publishable without the Stage 3B admission seam."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "manifest.json"
            manifest.write_bytes(json_bytes([manifest_row()]))
            capture_paths = capture_register_run(
                manifest,
                root / "capture",
                session=MemorySession(
                    {
                        CURRENT_URL: [
                            successful_exchange(
                                version_body(
                                    "C2004A00467",
                                    "2026-08-01",
                                    "33",
                                    "C2026C00333",
                                ),
                                "2026-08-29T17:00:01Z",
                            )
                        ]
                    }
                ),
            )
            publication_output = root / "publication"

            with self.assertRaisesRegex(
                PublicationBundleError,
                "schema_version is unsupported",
            ):
                export_publication_bundles(
                    capture_paths["baseline"],
                    capture_paths["observation"],
                    publication_output,
                )
            self.assertFalse(publication_output.exists())


if __name__ == "__main__":
    unittest.main()
