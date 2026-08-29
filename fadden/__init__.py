"""Commonwealth tax legislation corpus builder."""

from __future__ import annotations

__all__ = ["STAGES"]

STAGES = (
    "discover",
    "versions",
    "download",
    "probe13",
    "retry13",
    "extract",
    "pii_scan",
    "pii_scan2",
    "finalize",
    "rates",
    "check_current",
    "dist",
    "dist_verify",
    "export_monitor_contract",
    "export_publication_bundles",
    "capture_register",
    "export_live_evidence_bundles",
)
