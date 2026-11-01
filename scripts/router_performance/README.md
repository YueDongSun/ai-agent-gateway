# Router SSE performance runner

This standalone benchmark measures the repository's current incremental SSE decoder with deterministic local fixtures. It does not start a gateway or contact an upstream. The `baseline` Cargo feature is an optional comparison hook; the parent task may temporarily provide `src/sse_baseline_temp.rs` containing the base revision decoder and remove it after the comparison.

The release profile matches the desktop application's size optimization, LTO, and codegen settings. This is a decoder CPU benchmark, not an end-to-end HTTP or model latency measurement. It collects output strings for exact payload validation; that collection is part of the harness, not production streaming behavior.

From this directory, run `cargo run --locked --offline --release -- --samples 15 --target-bytes 8388608`. Run the included decoder regression tests with `cargo test --locked --offline`. Fixtures are generated lazily and each large corpus is dropped after its measurements, so memory use tracks one fixture at a time. `--samples` accepts 15 through 1000; `--target-bytes` accepts 65536 through 67108864. Without an option, those shown values apply. Build `--features baseline` only while the temporary baseline source exists.

Each JSON line reports a fixture and decoder label, median and nearest-rank p95 time, events/s, MiB/s, input bytes, chunk count, and sample settings. Fixture construction is outside the timer. Each timed sample includes input chunk handle cloning, decoder construction, polling, and collecting decoded strings. Output payload equality is checked after timing.


