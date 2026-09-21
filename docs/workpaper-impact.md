# Route a source change to affected work

The optional `sample-workpaper-map.json` uses the existing exact register ID and
collection mapping. It gives the same synthetic change two concrete review
questions: a BAS purchase classification and a forecast payment assumption.
Each question names the workpaper, affected period, evidence to inspect and
work that can be preserved. Metadata alone cannot supply replacement treatment.

```powershell
uv run --locked tax-radar-au compare --baseline tax_radar_au/samples/baseline/sample-sources.json --observation tax_radar_au/samples/observations/sample-register-observation.json --map tax_radar_au/samples/mappings/sample-workpaper-map.json --out build/workpaper-impact
```

All identities and periods in this example are fabricated. Skill paths are
review destinations, not proof that a source governs a skill. A reviewer must
confirm scope, operative period and practical effect. Changing the mapping
changes the queue digest, so decisions made against the original sample queue
do not approve this one. No worksheet, rate, date or skill is automatically edited.
