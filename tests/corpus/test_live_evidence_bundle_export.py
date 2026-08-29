"""Golden contract tests for live-only evidence-bundle.v2 exports."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import tempfile
import unittest
from pathlib import Path
from typing import Callable
from unittest import mock

import fadden.export_live_evidence_bundles as export_module
from fadden.capture_register import CaptureRegisterError, validate_capture_graph
from fadden.export_live_evidence_bundles import (
    LiveEvidenceBundleError,
    export_live_evidence_bundles,
)


WINDOWS_ONLY_EXPORT = unittest.skipUnless(
    os.name == "nt",
    "identity-bound live evidence publication is supported only on Windows",
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

    @WINDOWS_ONLY_EXPORT
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

    @WINDOWS_ONLY_EXPORT
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

    def _append_valid_superseded_candidate(self, capture: Path) -> None:
        baseline, register, observation = self._documents(capture)
        title = json.loads(json.dumps(baseline["titles"][0]))
        result = json.loads(json.dumps(register["results"][0]))
        item = json.loads(json.dumps(observation["observations"][0]))
        register_id = "F2022L00348"
        document_id = "F2026C00839"
        raw = (
            DEFAULT_RESPONSE.replace(b"F2022L00347", register_id.encode("ascii"))
            .replace(b"F2026C00838", document_id.encode("ascii"))
        )
        response_sha256 = _sha256_id(raw)
        evidence_name = f"sha256-{response_sha256.removeprefix('sha256:')}.json"
        (capture / "evidence" / evidence_name).write_bytes(raw)
        title.update(
            {
                "register_id": register_id,
                "source_url": title["source_url"].replace("F2022L00347", register_id),
                "register_page": title["register_page"].replace("F2022L00347", register_id),
            }
        )
        result["register_id"] = register_id
        request = result["requests"][0]
        request.update(
            {
                "url": request["url"].replace("F2022L00347", register_id),
                "response_length": len(raw),
                "response_sha256": response_sha256,
                "evidence_path": f"evidence/{evidence_name}",
            }
        )
        item.update(
            {
                "register_id": register_id,
                "observed_register_document_id": document_id,
                "evidence_url": item["evidence_url"].replace("F2022L00347", register_id),
            }
        )
        baseline["titles"].append(title)
        register["expected_register_ids"].append(register_id)
        register["results"].append(result)
        observation["expected_register_ids"].append(register_id)
        observation["observations"].append(item)
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

        def missing_title(capture: Path) -> None:
            baseline, _register, _observation = self._documents(capture)
            baseline["titles"] = []
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
            ("missing title", missing_title),
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

    def test_capture_directory_reparse_race_never_exports_external_graph(self) -> None:
        """A capture-directory swap cannot redirect the owned snapshot to external bytes."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(root)
            external, _ = LiveEvidenceBundleContractTests()._write_capture(
                root / "external",
                mutate=lambda baseline, _result, _observation: baseline["titles"][0].__setitem__(
                    "name", "External capture bytes"
                ),
            )
            retained = root / "retained-capture"
            original_copy = export_module._copy_regular_file
            swapped = False

            def replace_capture(source: Path, target: Path, **kwargs: object) -> None:
                nonlocal swapped
                if not swapped:
                    swapped = True
                    try:
                        os.rename(capture, retained)
                        os.symlink(external, capture, target_is_directory=True)
                    except OSError as exc:
                        raise LiveEvidenceBundleError("directory replacement was blocked") from exc
                original_copy(source, target, **kwargs)

            with mock.patch.object(
                export_module, "_copy_regular_file", side_effect=replace_capture
            ):
                try:
                    export = export_live_evidence_bundles(capture, root / "output")
                except LiveEvidenceBundleError:
                    export = None

            self.assertTrue(swapped)
            if export is None:
                self.assertFalse((root / "output").exists())
            else:
                bundle = json.loads(export.candidates[0].read_bytes())
                self.assertNotEqual(bundle["baseline_title"]["name"], "External capture bytes")

    def test_evidence_directory_reparse_race_never_copies_external_bytes(self) -> None:
        """Replacing evidence after inspection cannot make the snapshot copy external content."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(root)
            outside = root / "outside-evidence"
            outside.mkdir()
            (outside / next((capture / "evidence").iterdir()).name).write_bytes(b"external")
            retained = root / "retained-evidence"
            original_copy = export_module._copy_regular_file
            copied: list[bytes] = []
            swapped = False

            def replace_evidence(source: Path, target: Path, **kwargs: object) -> None:
                nonlocal swapped
                if source.parent == capture / "evidence" and not swapped:
                    swapped = True
                    try:
                        os.rename(capture / "evidence", retained)
                        os.symlink(outside, capture / "evidence", target_is_directory=True)
                    except OSError as exc:
                        raise LiveEvidenceBundleError("directory replacement was blocked") from exc
                original_copy(source, target, **kwargs)
                if source.parent.name == "evidence":
                    copied.append(target.read_bytes())

            with mock.patch.object(
                export_module, "_copy_regular_file", side_effect=replace_evidence
            ):
                try:
                    export_live_evidence_bundles(capture, root / "output")
                    export_succeeded = True
                except LiveEvidenceBundleError:
                    export_succeeded = False

            self.assertTrue(swapped)
            self.assertNotIn(b"external", copied)
            if export_succeeded:
                self.assertTrue((root / "output").exists())
            else:
                self.assertFalse((root / "output").exists())

    def test_posix_without_descriptor_primitives_fails_closed(self) -> None:
        """A non-Windows platform may not fall back to pathname capture reads."""
        current_directory = Path.cwd()
        with mock.patch.object(export_module.os, "name", "posix"), mock.patch.object(
            export_module, "_posix_descriptor_pinning_available", return_value=False
        ):
            with self.assertRaisesRegex(LiveEvidenceBundleError, "secure directory pinning"):
                with export_module._PinnedDirectoryChain() as pinned:
                    pinned.pin(current_directory, "capture input")

    @unittest.skipUnless(
        os.name == "posix"
        and hasattr(export_module, "_posix_descriptor_pinning_available")
        and export_module._posix_descriptor_pinning_available(),
        "requires POSIX descriptor-relative no-follow primitives",
    )
    def test_posix_evidence_swap_before_identity_capture_never_copies_external_bytes(self) -> None:
        """A pre-identity evidence swap stays bound to the pinned original directory."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(root)
            outside = root / "outside-evidence"
            outside.mkdir()
            evidence_name = next((capture / "evidence").iterdir()).name
            (outside / evidence_name).write_bytes(b"external")
            retained = root / "retained-evidence"
            original_details = export_module._ordinary_file_details
            original_copy = export_module._copy_regular_file
            copied: list[bytes] = []
            swapped = False

            def swap_before_identity(source: Path, **kwargs: object) -> os.stat_result:
                nonlocal swapped
                if source.parent == capture / "evidence" and not swapped:
                    swapped = True
                    os.rename(capture / "evidence", retained)
                    os.symlink(outside, capture / "evidence", target_is_directory=True)
                return original_details(source, **kwargs)

            def record_copy(source: Path, target: Path, **kwargs: object) -> os.stat_result:
                result = original_copy(source, target, **kwargs)
                if source.parent.name == "evidence":
                    copied.append(target.read_bytes())
                return result

            with mock.patch.object(
                export_module, "_ordinary_file_details", side_effect=swap_before_identity
            ), mock.patch.object(export_module, "_copy_regular_file", side_effect=record_copy):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "inventory changed"):
                    export_live_evidence_bundles(capture, root / "output")

            self.assertTrue(swapped)
            self.assertNotIn(b"external", copied)
            self.assertFalse((root / "output").exists())

    def test_candidate_identity_round_trips_the_validated_identifier_pair(self) -> None:
        """The fixed pair encoding proves lower-case output identity injectivity."""
        make_identity = getattr(export_module, "_candidate_identity", None)
        parse_identity = getattr(export_module, "_candidate_identity_pair", None)
        if not callable(make_identity) or not callable(parse_identity):
            self.fail("candidate identity must have a checked pair round trip")
        pairs = (
            ("A0000A00000", "A0000A00001"),
            ("Z9999Z99998", "Z9999Z99999"),
        )
        identities = [make_identity(*pair) for pair in pairs]

        self.assertEqual(
            identities,
            [
                "bundle-frl-a0000a00000-a0000a00001-r1",
                "bundle-frl-z9999z99998-z9999z99999-r1",
            ],
        )
        self.assertEqual([parse_identity(identity) for identity in identities], list(pairs))
        self.assertEqual(len(set(identities)), len(pairs))
        with self.assertRaises(LiveEvidenceBundleError):
            make_identity("a0000a00000", "A0000A00001")
        with self.assertRaises(LiveEvidenceBundleError):
            parse_identity("bundle-frl-a0000a00000-a0000a00001-extra-r1")

    def test_stage_3a_rejects_duplicate_title_before_candidate_derivation(self) -> None:
        """Duplicate valid identifiers cannot reach candidate identity derivation."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(root)
            baseline, register, observation = self._documents(capture)
            baseline["titles"].append(dict(baseline["titles"][0]))
            self._write_documents(capture, baseline, register, observation)

            with self.assertRaisesRegex(CaptureRegisterError, "identity is duplicated"):
                validate_capture_graph(capture)

    def test_one_inconsistent_superseded_candidate_blocks_all_candidates(self) -> None:
        """A later bad candidate cannot permit an earlier candidate to be exported alone."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture, _ = self._write_capture(root)
            self._append_valid_superseded_candidate(capture)
            baseline, register, observation = self._documents(capture)
            request = register["results"][1]["requests"][0]
            evidence = capture / request["evidence_path"]
            raw = evidence.read_bytes().replace(
                b'"registeredAt":"2026-08-27T17:31:41.1234567+10:00"',
                b'"registeredAt":"2026-08-27"',
            )
            evidence.unlink()
            digest = _sha256_id(raw)
            evidence_name = f"sha256-{digest.removeprefix('sha256:')}.json"
            (capture / "evidence" / evidence_name).write_bytes(raw)
            request.update(
                {
                    "response_length": len(raw),
                    "response_sha256": digest,
                    "evidence_path": f"evidence/{evidence_name}",
                }
            )
            self._write_documents(capture, baseline, register, observation)

            with self.assertRaises(LiveEvidenceBundleError):
                export_live_evidence_bundles(capture, root / "output")

            self.assertFalse((root / "output").exists())

    @WINDOWS_ONLY_EXPORT
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

    @WINDOWS_ONLY_EXPORT
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

    @WINDOWS_ONLY_EXPORT
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


class LiveEvidenceFilesystemTests(unittest.TestCase):
    """Publication is one no-overwrite directory transaction."""

    @staticmethod
    def _capture(root: Path) -> Path:
        return LiveEvidenceBundleContractTests()._write_capture(root)[0]

    @staticmethod
    def _owned_staging(parent: Path, output: Path) -> list[Path]:
        return list(parent.glob(f".{output.name}.live-evidence-*"))

    @WINDOWS_ONLY_EXPORT
    def test_absent_output_promotes_private_sibling_with_private_modes(self) -> None:
        """A successful export must publish one complete, non-world-readable directory."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "new-parent" / "output"

            export = export_live_evidence_bundles(self._capture(root), output)

            self.assertEqual(export.candidates, (output / "bundle-frl-f2022l00347-f2026c00838-r1.json",))
            self.assertEqual(len(list(output.iterdir())), 1)
            self.assertEqual(self._owned_staging(output.parent, output), [])
            if os.name == "posix":
                self.assertEqual((output.stat().st_mode & 0o777), 0o700)
                self.assertEqual((export.candidates[0].stat().st_mode & 0o777), 0o600)

    def test_only_one_safe_missing_output_parent_is_created(self) -> None:
        """Creating arbitrary ancestor chains would expand the exporter write boundary."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "first" / "second" / "output"

            with self.assertRaisesRegex(LiveEvidenceBundleError, "one missing output parent"):
                export_live_evidence_bundles(self._capture(root), output)

            self.assertFalse((root / "first").exists())

    def test_output_may_not_collide_with_capture_input(self) -> None:
        """A destination alias must not be able to replace the admitted capture graph."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture = self._capture(root)
            before = (capture / "monitor-baseline.json").read_bytes()

            with self.assertRaises(LiveEvidenceBundleError):
                export_live_evidence_bundles(capture, capture)

            self.assertEqual((capture / "monitor-baseline.json").read_bytes(), before)

    def test_existing_output_and_linked_ancestor_fail_without_mutation(self) -> None:
        """No existing target or redirected ancestor is an admissible publication location."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture = self._capture(root)
            existing = root / "existing"
            existing.mkdir()
            (existing / "keep").write_text("preserve", encoding="utf-8")

            with self.assertRaises(LiveEvidenceBundleError):
                export_live_evidence_bundles(capture, existing)
            self.assertEqual((existing / "keep").read_text(encoding="utf-8"), "preserve")

            linked = root / "linked"
            try:
                os.symlink(root, linked, target_is_directory=True)
            except (NotImplementedError, OSError):
                self.skipTest("directory symbolic links are unavailable on this host")
            with self.assertRaises(LiveEvidenceBundleError):
                export_live_evidence_bundles(capture, linked / "output")
            self.assertFalse((root / "output").exists())

    @WINDOWS_ONLY_EXPORT
    def test_write_failure_removes_only_owned_staging_and_preserves_destination(self) -> None:
        """A failed write may not leave an official partial directory or remove neighbours."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"
            neighbour = root / ".output.not-ours"
            neighbour.mkdir()

            with mock.patch.object(export_module, "_write_new", side_effect=OSError("disk full")):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "could not be written"):
                    export_live_evidence_bundles(self._capture(root), output)

            self.assertFalse(output.exists())
            self.assertTrue(neighbour.is_dir())
            self.assertEqual(self._owned_staging(root, output), [])

    @WINDOWS_ONLY_EXPORT
    def test_reread_or_revalidation_failure_never_promotes_output(self) -> None:
        """Staged bytes are untrusted until their exact reread validation succeeds."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"

            with mock.patch.object(
                export_module,
                "_validate_staged_export",
                side_effect=LiveEvidenceBundleError("staged output changed"),
            ):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "staged output changed"):
                    export_live_evidence_bundles(self._capture(root), output)

            self.assertFalse(output.exists())
            self.assertEqual(self._owned_staging(root, output), [])

    @WINDOWS_ONLY_EXPORT
    def test_promotion_race_cannot_overwrite_new_destination(self) -> None:
        """A competitor claiming the final name before promotion must win without replacement."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"

            def race(staging: Path, destination: Path, *_args: object) -> None:
                destination.mkdir()
                (destination / "competitor").write_text("keep", encoding="utf-8")
                raise FileExistsError("destination claimed")

            with mock.patch.object(export_module, "_promote_no_replace", side_effect=race):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "could not be promoted"):
                    export_live_evidence_bundles(self._capture(root), output)

            self.assertEqual((output / "competitor").read_text(encoding="utf-8"), "keep")
            self.assertEqual(self._owned_staging(root, output), [])

    def test_replacement_at_the_promotion_boundary_cannot_become_official(self) -> None:
        """The operation itself, not a preceding stat, must bind the promoted directory."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture = self._capture(root)
            output = root / "output"
            retained = root / "retained-staging"
            original_promote = export_module._promote_no_replace

            def replace_then_promote(staging: Path, destination: Path, *args: object) -> None:
                os.rename(staging, retained)
                staging.mkdir()
                (staging / "attacker.json").write_text("attacker", encoding="utf-8")
                original_promote(staging, destination, *args)

            with mock.patch.object(
                export_module, "_promote_no_replace", side_effect=replace_then_promote
            ):
                with self.assertRaises(LiveEvidenceBundleError):
                    export_live_evidence_bundles(capture, output)

            self.assertFalse(output.exists())
            self.assertFalse((output / "attacker.json").exists())

    @WINDOWS_ONLY_EXPORT
    def test_replacement_attempt_at_cleanup_delete_boundary_cannot_remove_another_directory(self) -> None:
        """The held staging object must prevent a path replacement before recursive cleanup."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture = self._capture(root)
            output = root / "output"
            retained = root / "retained-staging"
            original_cleanup = export_module._cleanup_owned_staging
            replacement_blocked = False

            def replace_then_cleanup(staging: object) -> None:
                nonlocal replacement_blocked
                try:
                    os.rename(staging.path, retained)
                except OSError:
                    replacement_blocked = True
                original_cleanup(staging)

            with mock.patch.object(
                export_module,
                "_validate_staged_export",
                side_effect=LiveEvidenceBundleError("staged output changed"),
            ), mock.patch.object(
                export_module,
                "_cleanup_owned_staging",
                side_effect=replace_then_cleanup,
            ):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "staged output changed"):
                    export_live_evidence_bundles(capture, output)

            self.assertTrue(replacement_blocked)
            self.assertFalse(output.exists())
            self.assertFalse(retained.exists())

    @WINDOWS_ONLY_EXPORT
    def test_cleanup_child_replacement_after_handle_validation_is_blocked(self) -> None:
        """A child replacement at the former lstat-to-unlink boundary must not be deleted."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture = self._capture(root)
            output = root / "output"
            retained = root / "retained-child.json"
            original_cleanup = export_module._cleanup_owned_staging
            original_dispose = export_module._mark_exact_windows_handle_for_deletion
            replacement_blocked = False

            def cleanup_with_replacement(staging: object) -> None:
                child = staging.path / next(iter(staging.files))

                def replace_then_dispose(handle: int, label: str) -> None:
                    nonlocal replacement_blocked
                    if label == "live evidence staging child":
                        try:
                            os.rename(child, retained)
                            child.write_bytes(b"attacker")
                        except OSError:
                            replacement_blocked = True
                    original_dispose(handle, label)

                with mock.patch.object(
                    export_module,
                    "_mark_exact_windows_handle_for_deletion",
                    side_effect=replace_then_dispose,
                ):
                    original_cleanup(staging)

            with mock.patch.object(
                export_module,
                "_validate_staged_export",
                side_effect=LiveEvidenceBundleError("staged output changed"),
            ), mock.patch.object(
                export_module,
                "_cleanup_owned_staging",
                side_effect=cleanup_with_replacement,
            ):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "staged output changed"):
                    export_live_evidence_bundles(capture, output)

            self.assertTrue(replacement_blocked)
            self.assertFalse(retained.exists())
            self.assertFalse(output.exists())

    @WINDOWS_ONLY_EXPORT
    def test_unexpected_staging_child_stops_cleanup_without_deleting_any_child(self) -> None:
        """An inventory mismatch must preserve every staged child for safe manual recovery."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"
            original_write = export_module._write_new

            def write_unexpected(path: Path, content: bytes) -> None:
                original_write(path, content)
                (path.parent / "unexpected.json").write_bytes(b"preserve")

            with mock.patch.object(export_module, "_write_new", side_effect=write_unexpected):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "changed before cleanup"):
                    export_live_evidence_bundles(self._capture(root), output)

            staging = self._owned_staging(root, output)
            self.assertEqual(len(staging), 1)
            self.assertEqual((staging[0] / "unexpected.json").read_bytes(), b"preserve")
            self.assertEqual(len(list(staging[0].glob("*.json"))), 2)
            self.assertFalse(output.exists())

    @WINDOWS_ONLY_EXPORT
    def test_cleanup_failure_is_reported_without_official_output(self) -> None:
        """A failed bounded cleanup is not hidden behind the original write failure."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"
            with mock.patch.object(export_module, "_write_new", side_effect=OSError("disk full")), mock.patch.object(
                export_module,
                "_cleanup_owned_staging",
                side_effect=LiveEvidenceBundleError("staging could not be removed"),
            ):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "staging could not be removed"):
                    export_live_evidence_bundles(self._capture(root), output)

            self.assertFalse(output.exists())

    @WINDOWS_ONLY_EXPORT
    def test_cleanup_never_removes_a_replaced_same_prefix_staging_directory(self) -> None:
        """The prefix proves scope, while the recorded directory identity proves ownership."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"
            original_write = export_module._write_new
            replacement_blocked = False

            def replace_staging(path: Path, content: bytes) -> None:
                nonlocal replacement_blocked
                original_write(path, content)
                retained = root / "retained-staging"
                try:
                    os.rename(path.parent, retained)
                except OSError:
                    replacement_blocked = True

            with mock.patch.object(export_module, "_write_new", side_effect=replace_staging):
                export_live_evidence_bundles(self._capture(root), output)

            self.assertTrue(replacement_blocked)
            self.assertTrue(output.is_dir())

    @WINDOWS_ONLY_EXPORT
    def test_zero_candidates_still_promotes_one_empty_directory(self) -> None:
        """A verified abstention is successful evidence of no publishable candidate."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture = self._capture(root)
            LiveEvidenceGraphTests()._set_non_candidate_state(capture, "UNCHANGED")
            output = root / "output"

            export = export_live_evidence_bundles(capture, output)

            self.assertEqual(export.candidates, ())
            self.assertTrue(output.is_dir())
            self.assertEqual(list(output.iterdir()), [])

    def test_non_windows_rejects_before_creating_output_parent_or_staging(self) -> None:
        """POSIX must fail closed until it has an identity-bound directory primitive."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "new-parent" / "output"

            with mock.patch.object(export_module.os, "name", "posix"):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "only on Windows"):
                    export_module._publish_candidates(output, [])

            self.assertFalse(output.parent.exists())
            self.assertEqual(list(root.iterdir()), [])

    def test_producer_version_requires_strict_semver_and_package_equality(self) -> None:
        """A malformed or divergent package version must not enter an attested bundle."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            version_path = root / "VERSION"
            project_path = root / "pyproject.toml"
            version_path.write_text("01.2.3\n", encoding="utf-8")
            project_path.write_text('[project]\nversion = "01.2.3"\n', encoding="utf-8")
            with mock.patch.object(export_module, "_VERSION_PATH", version_path), mock.patch.object(
                export_module, "_PYPROJECT_PATH", project_path
            ):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "strict Semantic Version"):
                    export_module._producer_version()

            version_path.write_text("1.2.3 \n", encoding="utf-8")
            project_path.write_text('[project]\nversion = "1.2.3"\n', encoding="utf-8")
            with mock.patch.object(export_module, "_VERSION_PATH", version_path), mock.patch.object(
                export_module, "_PYPROJECT_PATH", project_path
            ):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "strict Semantic Version"):
                    export_module._producer_version()

            version_path.write_text("1.2.3\n", encoding="utf-8")
            project_path.write_text('[project]\nversion = "1.2.4"\n', encoding="utf-8")
            with mock.patch.object(export_module, "_VERSION_PATH", version_path), mock.patch.object(
                export_module, "_PYPROJECT_PATH", project_path
            ):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "strict Semantic Version"):
                    export_module._producer_version()

    def test_snapshot_handle_close_error_is_bounded_before_publication(self) -> None:
        """Raw close failures may not escape after an exporter transaction begins."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture = self._capture(root)
            output = root / "output"
            original_exit = export_module._PinnedDirectoryChain.__exit__

            def close_then_fail(
                instance: object,
                exception_type: object,
                exception_value: object,
                traceback: object,
            ) -> None:
                original_exit(instance, exception_type, exception_value, traceback)
                raise OSError("raw pinned handle close")

            with mock.patch.object(
                export_module._PinnedDirectoryChain,
                "__exit__",
                autospec=True,
                side_effect=close_then_fail,
            ):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "snapshot cleanup"):
                    export_live_evidence_bundles(capture, output)

            self.assertFalse(output.exists())

    def test_temporary_snapshot_cleanup_error_is_bounded_before_publication(self) -> None:
        """A temporary snapshot cleanup error must not strand an official output directory."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            capture = self._capture(root)
            output = root / "output"

            class FailingTemporaryDirectory:
                def __enter__(self) -> str:
                    directory = root / "private-snapshot"
                    directory.mkdir()
                    return str(directory)

                def __exit__(self, *args: object) -> None:
                    raise OSError("raw temporary cleanup")

            with mock.patch.object(
                export_module.tempfile,
                "TemporaryDirectory",
                return_value=FailingTemporaryDirectory(),
            ):
                with self.assertRaisesRegex(LiveEvidenceBundleError, "snapshot cleanup"):
                    export_live_evidence_bundles(capture, output)

            self.assertFalse(output.exists())
