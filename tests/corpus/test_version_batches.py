"""Paced version batches preserve attribution and complete-only publication."""

import contextlib
import io
import json
import re
import tempfile
import unittest
import urllib.parse
from pathlib import Path
from unittest import mock

from fadden import versions


def titles(count):
    collections = (("C2099A", "Act"), ("F2099L", "LegislativeInstrument"),
                   ("F2099N", "NotifiableInstrument"))
    return [{"id": "%s%05d" % (collections[i % 3][0], i + 1),
             "name": "Synthetic Tax Title %04d" % (count - i),
             "collection": collections[i % 3][1], "isPrincipal": True}
            for i in range(count)]


def requested_ids(url):
    return re.findall(r"titleId eq '([^']+)'", urllib.parse.unquote(url))


def metadata(rid):
    legacy = int(rid[-5:]) % 2 == 0
    return {"titleId": rid, "start": "2099-03-04T23:45:06.1234567+10:00",
            "registerId": None if legacy else "C2099C00001",
            "compilationNumber": None if legacy else "7"}


def response(url):
    return {"value": [metadata(rid) for rid in reversed(requested_ids(url))]}


def is_probe(url):
    return "$select=titleId,start,compilationNumber,isCurrent" in url


class VersionBatchTests(unittest.TestCase):
    def run_stage(self, rows, fetch=response, failure=None):
        events = []

        def request(url):
            events.append(("fetch", url))
            return fetch(url)

        def pause(delay):
            events.append(("sleep", delay))

        with tempfile.TemporaryDirectory() as temporary:
            scratch = Path(temporary)
            (scratch / "titles_all.json").write_text(json.dumps(rows), encoding="utf-8")
            output = scratch / "acts_resolved.json"
            old = b"previous complete manifest\n"
            output.write_bytes(old)
            stdout = io.StringIO()
            with mock.patch.object(versions, "SCRATCH", str(scratch)), \
                    mock.patch.object(versions, "fetch_json", side_effect=request), \
                    mock.patch.object(versions.time, "sleep", side_effect=pause), \
                    contextlib.redirect_stdout(stdout):
                if failure:
                    with self.assertRaises(failure):
                        versions.main()
                    self.assertEqual(output.read_bytes(), old)
                    result = None
                else:
                    versions.main()
                    result = json.loads(output.read_text(encoding="utf-8"))
            return result, events, stdout.getvalue()

    def test_946_title_workload_uses_120_calls_and_119_pauses(self):
        rows = titles(946)
        result, events, stdout = self.run_stage(rows)
        fetches = [value for kind, value in events if kind == "fetch"]
        pauses = [value for kind, value in events if kind == "sleep"]
        self.assertEqual(len(fetches), 120)
        self.assertEqual(pauses, [1.5] * 119)
        self.assertEqual(sum(pauses), 178.5)
        self.assertEqual([r["id"] for r in result],
                         [r["id"] for r in sorted(rows, key=lambda r: r["name"])])
        for title, record in zip(sorted(rows, key=lambda r: r["name"]), result):
            row = metadata(title["id"])
            self.assertEqual(record, dict(title, versionStart="2099-03-04",
                                          compilationNumber=row["compilationNumber"],
                                          compilationRegisterId=row["registerId"]))
        self.assertIn("resolved 925/946 (failed 0)", stdout)

    def test_boundaries_pacing_and_exact_singleton_url(self):
        for count, calls in ((0, 1), (1, 2), (8, 2), (9, 3), (17, 4)):
            with self.subTest(count=count):
                rows = titles(count)
                result, events, _ = self.run_stage(rows)
                self.assertEqual(len(result), count)
                self.assertEqual(len(events), 1 + 2 * (calls - 1))
                self.assertTrue(is_probe(events[0][1]))
                for fetch, pause in zip(events[1::2], events[2::2]):
                    self.assertEqual(fetch[0], "fetch")
                    self.assertEqual(pause, ("sleep", 1.5))
                    query = urllib.parse.parse_qs(urllib.parse.urlsplit(fetch[1]).query)
                    ids = requested_ids(fetch[1])
                    self.assertLessEqual(len(ids), 8)
                    self.assertEqual(query["$top"], [str(len(ids) + 1 if len(ids) > 1 else 1)])
                    self.assertEqual(query["$select"],
                                     ["titleId,start,compilationNumber,registerId"])
                    if len(ids) == 1:
                        self.assertEqual(fetch[1],
                                         "%s/versions?$top=1&$filter=%s&$select=titleId,start,compilationNumber,registerId"
                                         % (versions.API, urllib.parse.quote(
                                             "titleId eq '%s' and isCurrent eq true" % ids[0])))
                    else:
                        expected = "(%s) and isCurrent eq true" % " or ".join(
                            "titleId eq '%s'" % rid for rid in ids)
                        self.assertEqual(query["$filter"], [expected])

    def test_unavailable_batch_latches_to_paced_serial_and_preserves_output(self):
        rows = titles(17)
        fast, _, _ = self.run_stage(rows)

        def unavailable(url):
            return None if not is_probe(url) and len(requested_ids(url)) > 1 else response(url)

        serial, events, stdout = self.run_stage(rows, unavailable)
        self.assertEqual(serial, fast)
        self.assertEqual(len(events), 1 + 2 * 18)
        self.assertEqual(len(requested_ids(events[1][1])), 8)
        self.assertEqual(events[2], ("sleep", 1.5))
        for fetch, pause in zip(events[3::2], events[4::2]):
            self.assertEqual(len(requested_ids(fetch[1])), 1)
            self.assertEqual(pause, ("sleep", 1.5))
        self.assertEqual(stdout.count("switching to single-title lookups"), 1)

    def test_later_unavailable_batch_retries_only_remaining_titles(self):
        rows = titles(17)
        fast, _, _ = self.run_stage(rows)
        ordered_ids = [row["id"] for row in sorted(rows, key=lambda row: row["name"])]

        def unavailable(url):
            ids = requested_ids(url)
            if not is_probe(url) and len(ids) > 1 and ids != ordered_ids[:8]:
                return None
            return response(url)

        serial, events, stdout = self.run_stage(rows, unavailable)
        self.assertEqual(serial, fast)
        requests = [requested_ids(url) for kind, url in events
                    if kind == "fetch" and not is_probe(url)]
        self.assertEqual(requests[:2], [ordered_ids[:8], ordered_ids[8:16]])
        self.assertEqual(requests[2:], [[rid] for rid in ordered_ids[8:]])
        self.assertEqual(len(events), 23)
        for fetch, pause in zip(events[1::2], events[2::2]):
            self.assertEqual(fetch[0], "fetch")
            self.assertEqual(pause, ("sleep", 1.5))
        self.assertEqual(stdout.count("switching to single-title lookups"), 1)

    def test_valid_ambiguous_groups_discard_all_rows_and_resume_batching(self):
        rows = titles(17)
        ordered_ids = [row["id"] for row in sorted(rows, key=lambda row: row["name"])]
        duplicate_id = ordered_ids[0]

        def stable(url):
            payload = response(url)
            for row in payload["value"]:
                if row["titleId"] == duplicate_id:
                    row.update(start="2026-09-19T00:00:00", registerId=None,
                               compilationNumber=None)
            return payload

        def serial(url):
            return None if not is_probe(url) and len(requested_ids(url)) > 1 else stable(url)

        reference, _, _ = self.run_stage(rows, serial)
        good = [stable(versions._versions_url([rid]))["value"][0] for rid in ordered_ids[:8]]
        for row in good[1:]:
            row["start"] = "2088-01-01"
        extra = dict(good[0], start="2026-10-01T00:00:00")
        for batch in ([], good[:-1], good[:-1] + [extra], good + [extra]):
            with self.subTest(rows=len(batch)):
                def fetch(url):
                    return {"value": batch} if requested_ids(url) == ordered_ids[:8] else stable(url)

                result, events, stdout = self.run_stage(rows, fetch)
                self.assertEqual(result, reference)
                self.assertEqual(result[0]["versionStart"], "2026-09-19")
                self.assertIsNone(result[0]["compilationRegisterId"])
                requests = [requested_ids(url) for kind, url in events
                            if kind == "fetch" and not is_probe(url)]
                self.assertEqual(requests, [ordered_ids[:8]] + [[rid] for rid in ordered_ids[:8]]
                                 + [ordered_ids[8:16], ordered_ids[16:]])
                self.assertEqual(len(events), 23)
                self.assertEqual(events[2::2], [("sleep", 1.5)] * 11)
                self.assertEqual(stdout.count("ambiguous version batch"), 1)
                self.assertNotIn("switching to single-title lookups", stdout)

    def test_final_partial_group_detects_a_hidden_duplicate(self):
        rows = titles(10)
        reference, _, _ = self.run_stage(rows)

        def fetch(url):
            payload = response(url)
            if len(requested_ids(url)) == 2 and not is_probe(url):
                payload["value"].append(dict(payload["value"][-1]))
            return payload

        result, events, stdout = self.run_stage(rows, fetch)
        self.assertEqual(result, reference)
        self.assertEqual(len(events), 9)
        self.assertIn("retrying 2 title(s) individually", stdout)

    def test_946_titles_with_seven_ambiguous_groups_use_170_calls(self):
        rows = titles(946)
        reference, _, _ = self.run_stage(rows)
        ordered_ids = [row["id"] for row in sorted(rows, key=lambda row: row["name"])]
        first_ids = {ordered_ids[index] for index in (16, 200, 216, 304, 312, 320, 944)}

        def fetch(url):
            payload = response(url)
            ids = requested_ids(url)
            if len(ids) > 1 and ids[0] in first_ids:
                payload["value"].append(dict(payload["value"][-1]))
            return payload

        result, events, stdout = self.run_stage(rows, fetch)
        self.assertEqual(result, reference)
        self.assertEqual(sum(kind == "fetch" for kind, _ in events), 170)
        pauses = [delay for kind, delay in events if kind == "sleep"]
        self.assertEqual(pauses, [1.5] * 169)
        self.assertEqual(sum(pauses), 253.5)
        self.assertEqual(stdout.count("ambiguous version batch"), 7)

    def test_decoded_corrupt_batches_preserve_old_manifest_without_fallback(self):
        rows = titles(8)
        good = [metadata(row["id"]) for row in rows]
        invalid = [False, [], {}, {"value": 1}, {"value": good + good[:2]},
                   {"value": good[:-1] + [metadata("C2099A99999")]},
                   {"value": good[:-1] + ["wrong shape"]},
                   {"value": good[:-1] + [{}]},
                   {"value": good[:-1] + [{"titleId": []}]}]
        invalid += [dict(value=good, **{marker: value})
                    for marker in ("@odata.nextLink", "@nextLink")
                    for value in (None, "", "https://example.test/next")]
        for payload in invalid:
            with self.subTest(payload=payload):
                def fetch(url):
                    return response(url) if is_probe(url) else payload

                _, events, _ = self.run_stage(rows, fetch, RuntimeError)
                self.assertEqual(len(events), 3)
                self.assertEqual(events[-1], ("sleep", 1.5))

    def test_corrupt_extra_row_cannot_be_hidden_by_group_recovery(self):
        rows = titles(8)
        for field, value in (("start", "2099-02-30"), ("registerId", "missing"),
                             ("registerId", "C2099C00001/../../"),
                             ("titleId", "C2099A99999")):
            with self.subTest(field=field, value=value):
                def fetch(url):
                    payload = response(url)
                    if not is_probe(url) and len(requested_ids(url)) > 1:
                        extra = dict(payload["value"][0])
                        if value == "missing":
                            del extra[field]
                        else:
                            extra[field] = value
                        payload["value"].append(extra)
                    return payload

                _, events, _ = self.run_stage(rows, fetch, RuntimeError)
                self.assertEqual(len(events), 3)

    def test_bad_admitted_fields_fail_without_refetching(self):
        rows = titles(8)
        for field, value in (("start", "2099-02-30"), ("start", "2099-01-01T25:00:00"),
                             ("registerId", "C2099C00001/../../"), ("registerId", "missing")):
            with self.subTest(field=field, value=value):
                def fetch(url):
                    payload = response(url)
                    if not is_probe(url):
                        if value == "missing":
                            del payload["value"][0][field]
                        else:
                            payload["value"][0][field] = value
                    return payload

                _, events, _ = self.run_stage(rows, fetch, RuntimeError)
                self.assertEqual(len(events), 3)

    def test_singleton_failure_after_fallback_preserves_old_manifest(self):
        rows = titles(9)
        failed_id = rows[3]["id"]

        def fetch(url):
            ids = requested_ids(url)
            if not is_probe(url) and (len(ids) > 1 or ids == [failed_id]):
                return None
            return response(url)

        _, events, stdout = self.run_stage(rows, fetch, RuntimeError)
        self.assertEqual(len(events), 21)
        self.assertIn("resolved: 8   failed: 1", stdout)

    def test_singleton_failure_during_group_recovery_preserves_old_manifest(self):
        rows = titles(17)
        ordered_ids = [row["id"] for row in sorted(rows, key=lambda row: row["name"])]

        def fetch(url):
            ids = requested_ids(url)
            if ids == [ordered_ids[3]]:
                return None
            payload = response(url)
            if ids == ordered_ids[:8]:
                payload["value"].append(dict(payload["value"][0]))
            return payload

        _, events, stdout = self.run_stage(rows, fetch, RuntimeError)
        self.assertEqual(len(events), 23)
        self.assertIn("resolved: 16   failed: 1", stdout)
        self.assertNotIn("switching to single-title lookups", stdout)

    def test_empty_probe_does_not_control_batch_resolution(self):
        def fetch(url):
            return None if is_probe(url) else response(url)

        result, events, stdout = self.run_stage(titles(8), fetch)
        self.assertEqual(len(result), 8)
        self.assertEqual(len(events), 3)
        self.assertIn("probe returned no rows", stdout)

    def test_all_input_ids_are_validated_before_any_fetch(self):
        rows = titles(9)
        rows.append(dict(rows[0], id="C2099A00001' or isCurrent eq true"))
        _, events, _ = self.run_stage(rows, failure=ValueError)
        self.assertEqual(events, [])

    def test_last_duplicate_and_principal_selection_precede_batching(self):
        rows = titles(9)
        latest = dict(rows[2], name="First replacement title", collection="Replacement")
        rows.extend([latest, dict(rows[4], isPrincipal=False)])
        result, events, stdout = self.run_stage(rows)
        expected = sorted([r for r in rows[:9] if r["id"] != rows[4]["id"]],
                          key=lambda r: latest["name"] if r["id"] == latest["id"] else r["name"])
        self.assertEqual([r["id"] for r in result], [r["id"] for r in expected])
        self.assertEqual(result[0]["name"], latest["name"])
        self.assertEqual(result[0]["collection"], "Replacement")
        self.assertEqual(len(events), 3)
        self.assertIn("distinct principal titles:   8", stdout)
