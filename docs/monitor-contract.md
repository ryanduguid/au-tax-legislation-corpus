## Exporting a change-review observation

`export_monitor_contract.py` projects a completed corpus `sources.json` and a
separately collected, structured observation-facts JSON file into the exact
v1 baseline and observation inputs for
[`tax-radar-au`](../RADAR.md):

```bash
python -m fadden export_monitor_contract -- corpus/sources.json observation-facts.json --out monitor-input
```

The facts file may use `au-tax-register-observation-facts.v1`, v2 or v3. It
contains the observation timestamp, whether coverage is complete, and one
stateful result for each observed Register id. V3 also binds every observation
to the SHA-256 of the exact source payload used, its content kind and media
type. The exporter validates the
scope, collection, UTC timestamps, HTTPS evidence links and state-specific
fields, rejects duplicate JSON members and control characters, and refuses an
input whose resolved path is either output filename. Existing output names must
be ordinary files, never directories, links, junctions or other special paths.
Exactly one writer may publish to an output directory at a time. An existing
publisher lock blocks for up to 30 seconds before reporting an active writer
or interrupted publication. The operator must stop writers and readers and
preserve the pair and recovery artefacts. Restore a known consistent pair or
retire the synthetic output and regenerate it in a new directory before removing
`.monitor-contract.publish.lock`. A dead owner, an old timestamp or missing
`.bak` files alone do not prove consistency. If rollback itself fails, the
exporter retains the lock and unrecovered backups. Lock cleanup failures are reported.

The exporter stages both files and restores the prior pair after an ordinary
write failure. `monitor-baseline.json` and `register-observation.json` are
replaced individually, not by a cross-file filesystem transaction. A process
or power loss between replacements can therefore leave an old/new pair and
lock or rollback files for recovery. A fully crash-atomic publication needs
versioned pair directories and an atomic generation pointer, which this v1
adapter does not create.

It does not call the Register, download a document, decide the legal effect of
a change, or update any workflow. `synthetic` remains this exporter's output
mode; live source capture is a separate, deliberately incompatible contract.
