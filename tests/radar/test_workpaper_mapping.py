"""A source-change question names work without asserting the legal effect."""
from pathlib import Path

from tax_radar_au.monitor import compare
from tax_radar_au.util import sample_path


def test_workpaper_map_preserves_both_review_tasks_and_no_legal_conclusion():
    queue = compare(
        baseline_path=sample_path("baseline", "sample-sources.json"),
        observation_path=sample_path("observations", "sample-register-observation.json"),
        mapping_path=sample_path("mappings", "sample-workpaper-map.json"),
    )
    changed = [item for item in queue["items"] if item["state"] == "OPEN"]
    assert len(changed) == 1
    candidates = changed[0]["impact_candidates"]
    assert {row["skill_ref"] for row in candidates} == {"bas-preparation", "cashflow-forecast"}
    forecast = next(row for row in candidates if row["skill_ref"] == "cashflow-forecast")
    assert Path(forecast["skill_path"]).exists()
    questions = " ".join(row["review_question"] for row in candidates)
    assert "6150.00 purchase tie-out" in questions
    assert "preserve the original snapshot" in questions
    assert "September 2099" in questions
    assert changed[0]["limitations"]
