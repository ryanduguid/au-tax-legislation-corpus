"""Golden contract tests for live-only evidence-bundle.v2 exports."""

from __future__ import annotations

import base64
import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from typing import Callable

from fadden.export_live_evidence_bundles import (
    LiveEvidenceBundleError,
    export_live_evidence_bundles,
)


FIXTURES = Path(__file__).parent / "fixtures" / "live-evidence"
V2_FIXTURE = FIXTURES / "evidence-bundle.v2.json"
DEFAULT_RESPONSE = (
    b'{"@odata.context":"https://api.prod.legislation.gov.au/v1/'
    b'$metadata#Versions(titleId,start,compilationNumber,registerId,'
    b'isCurrent,status,registeredAt)","value":[{"titleId":"F2022L00347",'
    b'"start":"2026-08-18T00:00:00","compilationNumber":"20",'
    b'"registerId":"F2026C00838","isCurrent":true,"status":"InForce",'
    b'"registeredAt":"2026-08-27T17:31:41.1234567+10:00"}]}'
)


def _json_bytes(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def _sha256_id(content: bytes) -> str:
    return f"sha256:{hashlib.sha256(content).hexdigest()}"


class LiveEvidenceBundleContractTests(unittest.TestCase):
    """The v2 bundle must preserve the reviewed, live-only byte contract."""

    def _write_capture(
        self,
        root: Path,
        *,
        response: bytes | None = None,
        mutate: Callable[[dict, dict, dict], None] | None = None,
    ) -> tuple[Path, str]:
        capture = root / "capture"
        evidence = capture / "evidence"
        evidence.mkdir(parents=True)
        register_id = "F2022L00347"
        current_document_id = "F2026C00838"
        response = response or DEFAULT_RESPONSE
        response_sha256 = _sha256_id(response)
        evidence_path = f"evidence/sha256-{response_sha256.removeprefix('sha256:')}.json"
        (capture / evidence_path).write_bytes(response)

        baseline = {
            "corpus": "Commonwealth tax statutes and legislative instruments",
            "retrieved": "2026-08-05",
            "source": "Federal Register of Legislation",
            "source_api": "https://api.prod.legislation.gov.au/v1/",
            "titles": [
                {
                    "register_id": register_id,
                    "name": "Taxation Administration Regulations 2017",
                    "collection": "LegislativeInstrument",
                    "compilation_number": "19",
                    "compilation_date": "2026-08-05",
                    "version_is_current": True,
                    "current_version_start": None,
                    "retrieved": "2026-08-05",
                    "source_url": (
                        "https://www.legislation.gov.au/F2022L00347/2026-08-05/"
                        "2026-08-05/text/original/epub"
                    ),
                    "register_page": "https://www.legislation.gov.au/F2022L00347/latest/text",
                }
            ],
        }
        current_url = (
            "https://api.prod.legislation.gov.au/v1/versions?%24top=1&"
            "%24filter=titleId%20eq%20%27F2022L00347%27%20and%20isCurrent%20eq%20true&"
            "%24select=titleId%2Cstart%2CcompilationNumber%2CregisterId%2CisCurrent%2Cstatus%2CregisteredAt"
        )
        result = {
            "register_id": register_id,
            "collection": "LegislativeInstrument",
            "checked_at": "2026-08-28T00:00:00Z",
            "state": "SUPERSEDED",
            "error_category": None,
            "requests": [
                {
                    "role": "current",
                    "url": current_url,
                    "checked_at": "2026-08-28T00:00:00Z",
                    "http_status": 200,
                    "transport_error_category": None,
                    "attempt_count": 1,
                    "response_headers": {
                        "content-type": "application/json",
                        "odata-version": "4.0",
                    },
                    "response_length": len(response),
                    "response_sha256": response_sha256,
                    "evidence_path": evidence_path,
                }
            ],
        }
        result_sha256 = _sha256_id(_json_bytes(result))
        observation = {
            "schema_version": "au-tax-register-observation.v4",
            "mode": "live",
            "observed_at": "2026-08-28T00:00:00Z",
            "scope_id": "au-primary-tax-legislation.v4",
            "baseline_sha256": "",
            "capture_sha256": "",
            "expected_register_ids": [register_id],
            "complete": True,
            "run_status": "VERIFIED",
            "observations": [
                {
                    "register_id": register_id,
                    "collection": "LegislativeInstrument",
                    "state": "SUPERSEDED",
                    "evidence_id": f"frl:{register_id}:{result_sha256.removeprefix('sha256:')[:32]}",
                    "observed_compilation_number": "20",
                    "observed_compilation_date": "2026-08-18",
                    "observed_register_document_id": current_document_id,
                    "current_version_start": None,
                    "evidence_url": "https://www.legislation.gov.au/F2022L00347/latest/text",
                    "checked_at": "2026-08-28T00:00:00Z",
                    "error_category": None,
                    "capture_result_sha256": result_sha256,
                    "primary_response_sha256": response_sha256,
                    "primary_response_media_type": "application/json",
                }
            ],
        }
        if mutate is not None:
            mutate(baseline, result, observation)
        baseline_bytes = _json_bytes(baseline)
        (capture / "monitor-baseline.json").write_bytes(baseline_bytes)
        observation["baseline_sha256"] = _sha256_id(baseline_bytes)
        capture_document = {
            "schema_version": "au-tax-register-capture.v1",
            "mode": "live",
            "observed_at": "2026-08-28T00:00:00Z",
            "source_api": "https://api.prod.legislation.gov.au/v1/",
            "baseline_sha256": _sha256_id(baseline_bytes),
            "expected_register_ids": [register_id],
            "complete": True,
            "results": [result],
        }
        capture_bytes = _json_bytes(capture_document)
        (capture / "register-capture.json").write_bytes(capture_bytes)
        observation["capture_sha256"] = _sha256_id(capture_bytes)
        observation_bytes = _json_bytes(observation)
        (capture / "register-observation.json").write_bytes(observation_bytes)
        return capture, _sha256_id(observation_bytes)

    def test_exports_the_live_only_contract_and_reviewed_golden_bytes(self) -> None:
        """A wrong identity, rights object or serialisation must fail this contract."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, observation_digest = self._write_capture(root)
            output = root / "output"

            result = export_live_evidence_bundles(capture, output)

            self.assertEqual(
                result.release_tag,
                f"live-evidence-v2-{observation_digest.removeprefix('sha256:')}",
            )
            self.assertEqual(
                tuple(path.name for path in result.candidates),
                ("bundle-frl-f2022l00347-f2026c00838-r1.json",),
            )
            actual = result.candidates[0].read_bytes()
            bundle = json.loads(actual)
            self.assertEqual(
                bundle["rights"],
                {
                    "mode": "metadata-only",
                    "attribution": (
                        "Based on content from the Federal Register of Legislation at 2026-08-28. "
                        "For the latest information on Australian Government legislation please go "
                        "to https://www.legislation.gov.au. Changes: selected and reformatted Federal "
                        "Register metadata into a bounded evidence bundle and factual source update; "
                        "no legislation text is reproduced."
                    ),
                    "licence_url": "https://creativecommons.org/licenses/by/4.0/",
                },
            )
            self.assertEqual(
                set(bundle),
                {
                    "schema_version",
                    "producer",
                    "run",
                    "baseline_title",
                    "capture_result",
                    "observation",
                    "rights",
                    "primary_response_base64",
                },
            )
            self.assertEqual(set(bundle["producer"]), {"name", "version"})
            self.assertEqual(
                set(bundle["run"]),
                {
                    "observed_at",
                    "scope_id",
                    "complete",
                    "run_status",
                    "baseline_sha256",
                    "observation_sha256",
                },
            )
            self.assertEqual(
                set(bundle["baseline_title"]),
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
            self.assertEqual(
                set(bundle["capture_result"]),
                {"register_id", "collection", "checked_at", "state", "error_category", "requests"},
            )
            self.assertEqual(
                set(bundle["capture_result"]["requests"][0]),
                {
                    "role",
                    "url",
                    "checked_at",
                    "http_status",
                    "transport_error_category",
                    "attempt_count",
                    "response_headers",
                    "response_length",
                    "response_sha256",
                    "evidence_path",
                },
            )
            self.assertEqual(
                set(bundle["observation"]),
                {
                    "register_id",
                    "collection",
                    "state",
                    "evidence_id",
                    "observed_compilation_number",
                    "observed_compilation_date",
                    "observed_register_document_id",
                    "current_version_start",
                    "evidence_url",
                    "checked_at",
                    "error_category",
                    "capture_result_sha256",
                    "primary_response_sha256",
                    "primary_response_media_type",
                },
            )
            self.assertEqual(set(bundle["rights"]), {"mode", "attribution", "licence_url"})
            self.assertEqual(actual, V2_FIXTURE.read_bytes())
            self.assertIn(b"\n  \"schema_version\"", actual)
            self.assertNotIn(b"\r", actual)
            self.assertTrue(actual.endswith(b"\n"))
            self.assertFalse(actual.endswith(b"\n\n"))
            encoded = bundle["primary_response_base64"]
            decoded = base64.b64decode(encoded, validate=True)
            self.assertEqual(base64.b64encode(decoded).decode("ascii"), encoded)

    def test_accepts_a_raw_response_at_the_256_kib_limit(self) -> None:
        """The permitted raw-response boundary must remain usable."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(
                root,
                response=DEFAULT_RESPONSE + b" " * (256 * 1024 - len(DEFAULT_RESPONSE)),
            )

            result = export_live_evidence_bundles(capture, root / "output")

        self.assertEqual(len(result.candidates), 1)

    def test_rejects_a_raw_response_over_the_256_kib_limit(self) -> None:
        """An oversized source response must fail before encoding it into the bundle."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(
                root,
                response=DEFAULT_RESPONSE + b" " * (256 * 1024 + 1 - len(DEFAULT_RESPONSE)),
            )

            with self.assertRaisesRegex(
                LiveEvidenceBundleError, "capture graph is not a verified Stage 3A graph"
            ):
                export_live_evidence_bundles(capture, root / "output")

    def test_rejects_a_serialised_bundle_over_the_1_mib_limit(self) -> None:
        """A source value that expands the envelope past 1 MiB must not be written."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(
                root,
                mutate=lambda baseline, _result, _observation: baseline["titles"][0].__setitem__(
                    "name", "x" * (1024 * 1024)
                ),
            )

            with self.assertRaisesRegex(
                LiveEvidenceBundleError, "serialised bundle exceeds the 1 MiB limit"
            ):
                export_live_evidence_bundles(capture, root / "output")

    def test_projects_exact_member_sets_when_stage_3a_objects_include_extras(self) -> None:
        """A complete graph refuses extensions rather than projecting them away."""
        def inject_extras(baseline: dict, result: dict, observation: dict) -> None:
            baseline["titles"][0]["unexpected_title"] = "must not be emitted"
            result["unexpected_result"] = "must not be emitted"
            result["requests"][0]["unexpected_request"] = "must not be emitted"
            observation["observations"][0]["unexpected_observation"] = "must not be emitted"

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(root, mutate=inject_extras)

            with self.assertRaises(LiveEvidenceBundleError):
                export_live_evidence_bundles(capture, root / "output")


class LiveEvidenceGraphTests(unittest.TestCase):
    """The exporter admits only a complete, independently provable Stage 3A graph."""

    def _write_capture(self, root: Path) -> tuple[Path, str]:
        return LiveEvidenceBundleContractTests()._write_capture(root)

    @staticmethod
    def _documents(capture: Path) -> tuple[dict, dict, dict]:
        return tuple(
            json.loads((capture / name).read_bytes())
            for name in (
                "monitor-baseline.json",
                "register-capture.json",
                "register-observation.json",
            )
        )  # type: ignore[return-value]

    @staticmethod
    def _write_documents(capture: Path, baseline: dict, register: dict, observation: dict) -> None:
        baseline_bytes = _json_bytes(baseline)
        register["baseline_sha256"] = _sha256_id(baseline_bytes)
        register_bytes = _json_bytes(register)
        observation["baseline_sha256"] = _sha256_id(baseline_bytes)
        observation["capture_sha256"] = _sha256_id(register_bytes)
        for result, item in zip(register["results"], observation["observations"], strict=True):
            result_sha256 = _sha256_id(_json_bytes(result))
            item["capture_result_sha256"] = result_sha256
            item["evidence_id"] = (
                f"frl:{item['register_id']}:{result_sha256.removeprefix('sha256:')[:32]}"
            )
            item["primary_response_sha256"] = result["requests"][0]["response_sha256"]
            content_type = result["requests"][0]["response_headers"].get("content-type")
            item["primary_response_media_type"] = (
                content_type.split(";", 1)[0].strip().lower()
                if content_type is not None
                else None
            )
        (capture / "monitor-baseline.json").write_bytes(baseline_bytes)
        (capture / "register-capture.json").write_bytes(register_bytes)
        (capture / "register-observation.json").write_bytes(_json_bytes(observation))

    def _set_non_candidate_state(self, capture: Path, state: str) -> None:
        baseline, register, observation = self._documents(capture)
        result = register["results"][0]
        item = observation["observations"][0]
        result["state"] = state
        result["error_category"] = None
        item.update(
            {
                "state": state,
                "error_category": None,
                "observed_compilation_number": None,
                "observed_compilation_date": None,
                "observed_register_document_id": None,
                "current_version_start": (
                    "2026-08-18"
                    if state == "CURRENT_NO_PUBLISHED_COMPILATION"
                    else None
                ),
            }
        )
        self._write_documents(capture, baseline, register, observation)

    def test_complete_graph_failures_are_table_driven(self) -> None:
        """No malformed graph may be partially exported as a plausible candidate."""
        def incomplete(capture: Path) -> None:
            _baseline, register, observation = self._documents(capture)
            register["complete"] = False
            observation["complete"] = False
            (capture / "register-capture.json").write_bytes(_json_bytes(register))
            (capture / "register-observation.json").write_bytes(_json_bytes(observation))

        def blocked(capture: Path) -> None:
            _baseline, _register, observation = self._documents(capture)
            observation["run_status"] = "BLOCKED"
            (capture / "register-observation.json").write_bytes(_json_bytes(observation))

        def lookup_failed(capture: Path) -> None:
            baseline, register, observation = self._documents(capture)
            result = register["results"][0]
            item = observation["observations"][0]
            result["state"] = "LOOKUP_FAILED"
            result["error_category"] = "INVALID_JSON"
            item.update(
                {
                    "state": "LOOKUP_FAILED",
                    "error_category": "INVALID_JSON",
                    "observed_compilation_number": None,
                    "observed_compilation_date": None,
                    "observed_register_document_id": None,
                    "current_version_start": None,
                }
            )
            observation["run_status"] = "BLOCKED"
            self._write_documents(capture, baseline, register, observation)

        def duplicate_title(capture: Path) -> None:
            baseline, _register, _observation = self._documents(capture)
            baseline["titles"].append(dict(baseline["titles"][0]))
            (capture / "monitor-baseline.json").write_bytes(_json_bytes(baseline))

        def digest_mismatch(capture: Path) -> None:
            _baseline, register, _observation = self._documents(capture)
            register["baseline_sha256"] = f"sha256:{'0' * 64}"
            (capture / "register-capture.json").write_bytes(_json_bytes(register))

        def response_length_mismatch(capture: Path) -> None:
            _baseline, register, _observation = self._documents(capture)
            register["results"][0]["requests"][0]["response_length"] += 1
            (capture / "register-capture.json").write_bytes(_json_bytes(register))

        def capture_result_digest_mismatch(capture: Path) -> None:
            _baseline, _register, observation = self._documents(capture)
            observation["observations"][0]["capture_result_sha256"] = f"sha256:{'0' * 64}"
            (capture / "register-observation.json").write_bytes(_json_bytes(observation))

        def unsafe_evidence_path(capture: Path) -> None:
            _baseline, register, _observation = self._documents(capture)
            register["results"][0]["requests"][0]["evidence_path"] = "../outside.json"
            (capture / "register-capture.json").write_bytes(_json_bytes(register))

        def undeclared_file(capture: Path) -> None:
            (capture / "evidence" / f"sha256-{'0' * 64}.json").write_bytes(b"extra")

        cases: tuple[tuple[str, Callable[[Path], None]], ...] = (
            ("incomplete", incomplete),
            ("non-VERIFIED", blocked),
            ("LOOKUP_FAILED", lookup_failed),
            ("duplicate derived identity", duplicate_title),
            ("digest mismatch", digest_mismatch),
            ("response length mismatch", response_length_mismatch),
            ("capture result digest mismatch", capture_result_digest_mismatch),
            ("unsafe evidence path", unsafe_evidence_path),
            ("undeclared evidence", undeclared_file),
        )
        for label, mutate in cases:
            with self.subTest(case=label), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                capture, _ = self._write_capture(root)
                mutate(capture)

                with self.assertRaises(LiveEvidenceBundleError):
                    export_live_evidence_bundles(capture, root / "output")

    def test_non_candidate_states_create_no_asset(self) -> None:
        """Only a verified SUPERSEDED observation is publishable evidence."""
        for state in (
            "UNCHANGED",
            "CURRENT_NO_PUBLISHED_COMPILATION",
            "NO_LONGER_IN_FORCE",
        ):
            with self.subTest(state=state), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                capture, _ = self._write_capture(root)
                self._set_non_candidate_state(capture, state)

                export = export_live_evidence_bundles(capture, root / "output")

                self.assertEqual(export.candidates, ())
                self.assertEqual(list((root / "output").iterdir()), [])

    def test_registered_at_is_independent_from_compilation_start(self) -> None:
        """Registration remains a separate source fact, not a substituted compilation date."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(root)

            bundle_path = export_live_evidence_bundles(capture, root / "output").candidates[0]
            bundle = json.loads(bundle_path.read_bytes())
            raw = base64.b64decode(bundle["primary_response_base64"], validate=True)

        self.assertEqual(bundle["observation"]["observed_compilation_date"], "2026-08-18")
        self.assertIn(b'"registeredAt":"2026-08-27T17:31:41.1234567+10:00"', raw)

    def test_null_previous_compilation_number_and_non_empty_current_number_are_required(self) -> None:
        """An unnumbered historical baseline is valid but the new compilation is not optional."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(root)
            baseline, register, observation = self._documents(capture)
            baseline["titles"][0]["compilation_number"] = None
            self._write_documents(capture, baseline, register, observation)

            self.assertEqual(len(export_live_evidence_bundles(capture, root / "output").candidates), 1)

    def test_malformed_registered_at_is_rejected_without_a_new_request(self) -> None:
        """Retained bytes must stand alone; export never refetches malformed source data."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(root)
            baseline, register, observation = self._documents(capture)
            request = register["results"][0]["requests"][0]
            evidence = capture / request["evidence_path"]
            raw = evidence.read_bytes().replace(
                b'"registeredAt":"2026-08-27T17:31:41.1234567+10:00"',
                b'"registeredAt":"2026-08-27"',
            )
            evidence.unlink()
            digest = _sha256_id(raw)
            replacement = capture / "evidence" / f"sha256-{digest.removeprefix('sha256:')}.json"
            replacement.write_bytes(raw)
            request.update(
                {
                    "response_sha256": digest,
                    "response_length": len(raw),
                    "evidence_path": f"evidence/{replacement.name}",
                }
            )
            self._write_documents(capture, baseline, register, observation)

            with self.assertRaises(LiveEvidenceBundleError):
                export_live_evidence_bundles(capture, root / "output")

    def test_raw_row_relationships_are_table_driven(self) -> None:
        """Each observed compilation fact must be recovered from the retained OData row."""
        def replace_raw(capture: Path, replace: tuple[bytes, bytes]) -> None:
            baseline, register, observation = self._documents(capture)
            request = register["results"][0]["requests"][0]
            evidence = capture / request["evidence_path"]
            raw = evidence.read_bytes().replace(*replace)
            evidence.unlink()
            digest = _sha256_id(raw)
            replacement = capture / "evidence" / f"sha256-{digest.removeprefix('sha256:')}.json"
            replacement.write_bytes(raw)
            request.update(
                {
                    "response_sha256": digest,
                    "response_length": len(raw),
                    "evidence_path": f"evidence/{replacement.name}",
                }
            )
            self._write_documents(capture, baseline, register, observation)

        cases: tuple[tuple[str, Callable[[Path], None]], ...] = (
            (
                "raw title identifier",
                lambda capture: replace_raw(capture, (b'"titleId":"F2022L00347"', b'"titleId":"F2022L00348"')),
            ),
            (
                "raw compilation start",
                lambda capture: replace_raw(capture, (b'"start":"2026-08-18', b'"start":"2026-08-19')),
            ),
            (
                "raw compilation number",
                lambda capture: replace_raw(capture, (b'"compilationNumber":"20"', b'"compilationNumber":""')),
            ),
            (
                "raw document identifier",
                lambda capture: replace_raw(capture, (b'"registerId":"F2026C00838"', b'"registerId":"F2026C00839"')),
            ),
            (
                "duplicate raw member",
                lambda capture: replace_raw(capture, (b'"start":"2026-08-18T00:00:00",', b'"start":"2026-08-18T00:00:00","start":"2026-08-18T00:00:00",')),
            ),
        )
        for label, mutate in cases:
            with self.subTest(case=label), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                capture, _ = self._write_capture(root)
                mutate(capture)

                with self.assertRaises(LiveEvidenceBundleError):
                    export_live_evidence_bundles(capture, root / "output")
