# PES reconstruction regression checks

The default test suite uses synthetic designs and the checked-in `examples/alower.pes` fixture. Downloaded PES collections are external test data and are not redistributed in this repository.

To run the ignored corpus tests, point at a collection containing the `DailyFilled`, `Handstitch`, and `Floss` directories:

```sh
ZIGZAG_PES_FIXTURES=/path/to/pes cargo test --lib reconstruct -- --ignored
```

For full-corpus sizing checks, run from the repository root:

```sh
cargo run --example pes_regression -- --root /path/to/pes --help
cargo run --example pes_regression -- --root /path/to/pes --bless
cargo run --example pes_regression -- --root /path/to/pes
```

`--bless` writes a baseline after a successful full run. Reports default to `target/pes-sizing.jsonl`; the baseline defaults to `tests/pes-sizing-baseline.jsonl`. Keep corpus-specific baselines local unless intentionally sharing them. `--jobs`, `--filter`, and `--resume` support incremental investigation; resume validates the engine fingerprint.

`cargo run --example pes_diagnose -- design.pes` prints recovered model counts, uncertain spans, and resized stitch points as JSON for inspection.
