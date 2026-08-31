# Corpus extraction performance baseline

This benchmark measures the production `to_markdown` path against two fabricated volumes containing 2,000 numbered sections, prose and periodic tables. It uses `pyperf` 2.10.0 from the locked development environment so warm-ups, worker processes and timing metadata follow a maintained benchmark tool rather than a repository-specific timer.

Run it from the repository root:

```bash
uv run --locked --extra dev --python 3.12 python benchmarks/benchmark_extract.py --rigorous
```

This is measurement-only. No duration or variance threshold is part of tests or CI. A threshold should be proposed only after repeated runs on a stable runner establish normal dispersion and the pull request records the raw results.

## Baseline recorded 2026-08-28

Three independent `--rigorous` runs on the same Windows workstation with
Python 3.12 measured the fabricated 2,000-section workload:

| Run | Mean | Standard deviation | Within-run CV |
| --- | ---: | ---: | ---: |
| 1 | 8.96 ms | 1.24 ms | 13.84% |
| 2 | 10.00 ms | 2.20 ms | 22.00% |
| 3 | 9.13 ms | 1.68 ms | 18.40% |

Across the three run means, the mean was 9.36 ms, the population standard
deviation was 0.46 ms and the coefficient of variation was 4.86%. The run-mean
range was 8.96-10.00 ms. `pyperf` warned that each individual run was unstable,
so these results establish workload shape and current dispersion only. They do
not justify a regression threshold.
