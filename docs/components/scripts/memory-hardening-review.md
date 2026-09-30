# Memory hardening review bundle

`python3 scripts/prepare-memory-hardening-review.py` writes the proposed
600-case JSON and six 100-case review packets linked from the
master review bundle.
It makes no model calls. Run it only before operator adjudication: regenerating
after review would replace the reviewed proposal and change its digest.

The proposal has 200 cases for each of applicability, utility and episode
quality. Each case records a pending review, a proposed label, a scenario group
and a development or held-out partition. Entire scenario families 3 and 7 are
held out. The exact inputs and SHA-256 appear in the master packet. The six
tables summarize them for one operator verdict with corrections by stable ID.
All cases are synthetic; the suite cannot establish the owner's natural case
distribution on its own.

After adjudication, freeze a corrected, approved dataset before any model
replay. Exclude unresolved cases and review replacements if an operation falls
below 200 scored items. `scripts/eval-memory-human-review.py` requires
`--partition development` or `--partition held_out` for a partitioned dataset;
older unpartitioned diagnostic datasets run without that flag. Tune only with
development results, then replay held-out cases once after settings are fixed.
Use `make test-decision-memory-human-review` for the replay runner's provider-free
regressions.

The paired evaluation
records the frozen dataset hash and gate results.
