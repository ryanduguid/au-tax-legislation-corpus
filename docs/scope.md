## Accuracy and limits

Not the authorised text. Authorised versions are PDF only, stamped under
sections 15ZA and 15ZB of the *Legislation Act 2003*. Everything here derives
from the EPUB reading view.

Check the `collection` field before relying on a provision. An Act and a
regulation made under it are not interchangeable, and an instrument can be
disallowed or sunset while its enabling Act stands.

Selection uses title keywords (Tax, Excise, Superannuation, Customs Tariff,
Medicare Levy) applied to the Act, LegislativeInstrument and
NotifiableInstrument collections. A tax-relevant title without one of those
words in its name is absent. The Register's `contains(name,...)` also matches
former titles and substrings, which is how the Passenger Movement Charge Act
1978 arrived under its old name, the *Departure Tax Act 1978*, and how 2 dozen
`AD/ROTAX/...` airworthiness directives arrived because 'Rotax' contains 'tax'.
`sources.json` records `keywords_in_name` rather than filtering them out, since
any rule strict enough to drop Rotax also drops the Departure Tax Act.

Some titles have no published compilation for the version now in force, so they
carry the last compilation the Register holds, marked `version_is_current:
false` in the front matter and on every row. There is no newer document to
re-download: the URL built from the in-force date answers 404.

## What this deliberately does not ship

The corpus itself. It is about a gigabyte and this code rebuilds it in an
afternoon, so shipping it would trade a lot of storage for very little. Two
parts of it should not be redistributed at all, and `dist.py` exists to separate
them out:

1. **The EPUBs embed the Commonwealth Coat of Arms**, which is excluded from the
   Register's CC BY 4.0 grant, along with any third-party material the Register
   has not cleared. Stripping the image would also destroy the only thing that
   makes shipping EPUBs worthwhile, which is that they are byte-identical to
   what the Register served. The markdown and JSONL carry no image data at all, because
   `extract.py` never emits any, so they need nothing removed.
2. **Twelve titles name private individuals.** The Tax Practitioners Board
   registers terminations and suspensions of tax and BAS agents as notifiable
   instruments: tables of named people with registration numbers and the
   provision breached, about 5,400 name mentions across 169 rows. Public on the
   Register as PDFs you read one at a time; shipping them as dataset rows makes
   them name-searchable at scale, which is a different act.

`pii_scan.py` finds the second category without being told where to look. It
tests every row in all 946 titles for personal names appearing alongside agent
registration numbers, because a bare name test flags the whole corpus:
legislation names Ministers, Commissioners and litigants constantly, and the
registration number is what separates a disciplinary register from a statute.
`pii_scan2.py` re-runs at a lower threshold and sweeps for emails, phone numbers
and 8- or nine-digit tax file numbers. On the source snapshot retrieved
2026-08-04, the JSONL scan finds 10 occurrences of 4 unique organisational
contacts across 3 titles: 2 government email addresses and 2 government
landlines. They are approved by a title-bound SHA-256 fingerprint in
`pii_contact_allowlist.json`; the policy does not store the identifiers
themselves. An allowlist entry is reserved for a manually reviewed
organisational contact published in that primary instrument; personal contacts
and TFNs are never allowlisted. Pattern false positives are fixed in
`pii_patterns.py` with a regression test rather than approved as data. Every
exception is bound to the identifier kind, digest and Register id and must carry
a review reason.

The scan, `dist.py` preflight and `dist_verify.py` all fail on a new or moved
contact identifier, and their diagnostics contain only Register ids and
truncated fingerprints. The publication gates require each title's exact
declared Markdown/JSONL file inventory and inspect every file in it, covering
the human-readable Markdown and endnotes as well as `sections.jsonl`. A private
name beside a registration number in any representation fails, as does an
unreadable file, binary control data, a nested path or an unexpected file.

For that validated source snapshot, `dist.py` writes `dist/`: 934 titles and
21,596 rows, the shipped manifest's 946 titles and 21,784 rows less the 12
titles `pii_flagged.json` names. Those 12 titles hold 188 rows between them, of
which `pii_flagged.json` records 169 as naming private individuals. `dist.py`
drops a flagged title whole rather than row by row, so all 188 of its rows leave
the distribution, not only the 169.
A `REMOVED.md` lists every exclusion and its Register link so the omission is
visible and reversible. `dist_verify.py` checks the result against its own
claims and exits non-zero if any check fails.
