use bytes::Bytes;
use futures_core::Stream;
use futures_util::stream;
use std::env;
use std::hint::black_box;
use std::io::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use std::time::Instant;

#[path = "../../../src-tauri/src/proxy/providers/sse.rs"]
mod current_sse;

#[cfg(feature = "baseline")]
#[path = "sse_baseline_temp.rs"]
mod baseline_sse;

const DEFAULT_SAMPLES: usize = 15;
const DEFAULT_TARGET_BYTES: usize = 8 * 1024 * 1024;
const MIN_SAMPLES: usize = 15;
const MAX_SAMPLES: usize = 1000;
const MIN_TARGET_BYTES: usize = 64 * 1024;
const MAX_TARGET_BYTES: usize = 64 * 1024 * 1024;
const WARMUP_RUNS: usize = 3;
const TOKEN_PAYLOAD_BYTES: usize = 160;
const TOOL_PAYLOAD_BYTES: usize = 64 * 1024;

struct Fixture {
    name: &'static str,
    wire: Vec<u8>,
    expected_events: Vec<String>,
    chunk_bytes: usize,
    target_bytes: usize,
}

fn parse_settings() -> Result<(usize, usize), String> {
    let mut samples = DEFAULT_SAMPLES;
    let mut target_bytes = DEFAULT_TARGET_BYTES;
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--samples" => samples = value.parse().map_err(|_| "invalid --samples".to_string())?,
            "--target-bytes" => {
                target_bytes = value
                    .parse()
                    .map_err(|_| "invalid --target-bytes".to_string())?
            }
            _ => return Err(format!("unknown argument: {flag}")),
        }
    }
    if !(MIN_SAMPLES..=MAX_SAMPLES).contains(&samples) {
        return Err(format!("--samples must be {MIN_SAMPLES}..={MAX_SAMPLES}"));
    }
    if !(MIN_TARGET_BYTES..=MAX_TARGET_BYTES).contains(&target_bytes) {
        return Err(format!(
            "--target-bytes must be {MIN_TARGET_BYTES}..={MAX_TARGET_BYTES}"
        ));
    }
    Ok((samples, target_bytes))
}

fn make_fixture(
    name: &'static str,
    frames: Vec<(Vec<u8>, String)>,
    chunk_bytes: usize,
    target_bytes: usize,
) -> Fixture {
    let mut wire = Vec::new();
    let mut expected_events = Vec::with_capacity(frames.len());
    for (frame, event) in frames {
        wire.extend_from_slice(&frame);
        expected_events.push(event);
    }
    let repetitions = target_bytes.div_ceil(wire.len()).max(1);
    let base = wire;
    wire = Vec::with_capacity(base.len() * repetitions);
    let base_expected = expected_events;
    expected_events = Vec::with_capacity(base_expected.len() * repetitions);
    for _ in 0..repetitions {
        wire.extend_from_slice(&base);
        expected_events.extend(base_expected.iter().cloned());
    }
    Fixture {
        name,
        wire,
        expected_events,
        chunk_bytes,
        target_bytes,
    }
}

fn typical_fixture(
    name: &'static str,
    crlf: bool,
    chunk_bytes: usize,
    target_bytes: usize,
) -> Fixture {
    let json_payload = format!(
        "{{\"id\":\"evt-000001\",\"delta\":\"{}\"}}",
        "x".repeat(TOKEN_PAYLOAD_BYTES)
    );
    let line_ending = if crlf { "\r\n" } else { "\n" };
    let frame = format!("data: {json_payload}{line_ending}{line_ending}");
    make_fixture(
        name,
        vec![(frame.into_bytes(), json_payload)],
        chunk_bytes,
        target_bytes,
    )
}

fn tool_fixture(
    name: &'static str,
    crlf: bool,
    chunk_bytes: usize,
    target_bytes: usize,
) -> Fixture {
    let tool_payload = format!(
        "{{\"tool\":\"search\",\"arguments\":{{\"query\":\"{}\"}}}}",
        "t".repeat(TOOL_PAYLOAD_BYTES)
    );
    let line_ending = if crlf { "\r\n" } else { "\n" };
    let frame = format!("data: {tool_payload}{line_ending}{line_ending}");
    make_fixture(
        name,
        vec![(frame.into_bytes(), tool_payload)],
        chunk_bytes,
        target_bytes,
    )
}

fn fixtures(target_bytes: usize) -> impl Iterator<Item = Fixture> {
    (0..11).map(move |index| match index {
        0 => typical_fixture("typical_lf_200b", false, 200, target_bytes),
        1 => typical_fixture("typical_crlf_200b", true, 200, target_bytes),
        2 => typical_fixture("coalesced_1k_lf", false, 1024, target_bytes),
        3 => typical_fixture("coalesced_1k_crlf", true, 1024, target_bytes),
        4 => typical_fixture("coalesced_16k_lf", false, 16 * 1024, target_bytes),
        5 => typical_fixture("coalesced_16k_crlf", true, 16 * 1024, target_bytes),
        6 => typical_fixture("coalesced_64k_lf", false, 64 * 1024, target_bytes),
        7 => typical_fixture("coalesced_64k_crlf", true, 64 * 1024, target_bytes),
        8 => typical_fixture("packed_1m_lf", false, 1024 * 1024, target_bytes),
        9 => tool_fixture("tool_event_64k_fragment_128b", false, 128, target_bytes),
        10 => tool_fixture("tool_event_64k_fragment_1k", true, 1024, target_bytes),
        _ => unreachable!("fixture index is bounded"),
    })
}
fn make_chunks(wire: &[u8], chunk_bytes: usize) -> Vec<Bytes> {
    wire.chunks(chunk_bytes)
        .map(Bytes::copy_from_slice)
        .collect()
}

fn drain<S>(mut decoder: S) -> Result<Vec<String>, Error>
where
    S: Stream<Item = Result<String, Error>> + Unpin,
{
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut events = Vec::new();
    loop {
        match Pin::new(&mut decoder).poll_next(&mut context) {
            Poll::Ready(Some(Ok(event))) => events.push(black_box(event)),
            Poll::Ready(Some(Err(error))) => return Err(error),
            Poll::Ready(None) => return Ok(events),
            Poll::Pending => {
                return Err(Error::new(
                    ErrorKind::Other,
                    "fixture stream unexpectedly pending",
                ))
            }
        }
    }
}

fn run_current(chunks: &[Bytes]) -> Result<Vec<String>, Error> {
    let input = chunks.iter().cloned().map(Ok).collect::<Vec<_>>();
    drain(current_sse::SseDataStream::new(stream::iter(input)))
}

#[cfg(feature = "baseline")]
fn run_baseline(chunks: &[Bytes]) -> Result<Vec<String>, Error> {
    let input = chunks.iter().cloned().map(Ok).collect::<Vec<_>>();
    drain(baseline_sse::SseDataStream::new(stream::iter(input)))
}

fn verify(events: &[String], expected: &[String]) -> Result<(), String> {
    if events != expected {
        return Err(format!(
            "decoder output mismatch: got {} events, expected {}",
            events.len(),
            expected.len()
        ));
    }
    Ok(())
}

fn percentile(values: &mut [u128], numerator: usize, denominator: usize) -> u128 {
    values.sort_unstable();
    values[(values.len() * numerator)
        .div_ceil(denominator)
        .saturating_sub(1)]
}

fn timed_sample<F>(fixture: &Fixture, chunks: &[Bytes], mut decode: F) -> Result<u128, String>
where
    F: FnMut(&[Bytes]) -> Result<Vec<String>, Error>,
{
    let started = Instant::now();
    let events = decode(chunks).map_err(|error| error.to_string())?;
    let elapsed = started.elapsed().as_nanos();
    verify(&events, &fixture.expected_events)?;
    black_box(events);
    Ok(elapsed)
}

fn warm_up<F>(fixture: &Fixture, chunks: &[Bytes], mut decode: F) -> Result<(), String>
where
    F: FnMut(&[Bytes]) -> Result<Vec<String>, Error>,
{
    for _ in 0..WARMUP_RUNS {
        let events = decode(chunks).map_err(|error| error.to_string())?;
        verify(&events, &fixture.expected_events)?;
        black_box(events);
    }
    Ok(())
}

fn report(
    fixture: &Fixture,
    chunks: &[Bytes],
    label: &'static str,
    durations: &mut [u128],
    samples: usize,
) {
    let p95_ns = percentile(&mut durations.to_vec(), 95, 100);
    let median_ns = percentile(durations, 50, 100);
    let seconds = median_ns as f64 / 1_000_000_000.0;
    println!("{{\"fixture\":\"{}\",\"decoder\":\"{}\",\"median_us\":{:.3},\"p95_us\":{:.3},\"events\":{},\"bytes\":{},\"chunks\":{},\"events_per_second\":{:.1},\"mib_per_second\":{:.3},\"samples\":{},\"warmups\":{},\"target_bytes\":{}}}", fixture.name, label, median_ns as f64 / 1000.0, p95_ns as f64 / 1000.0, fixture.expected_events.len(), fixture.wire.len(), chunks.len(), fixture.expected_events.len() as f64 / seconds, fixture.wire.len() as f64 / (1024.0 * 1024.0) / seconds, samples, WARMUP_RUNS, fixture.target_bytes);
}

#[cfg(not(feature = "baseline"))]
fn measure<F>(
    fixture: &Fixture,
    chunks: &[Bytes],
    label: &'static str,
    samples: usize,
    mut decode: F,
) -> Result<(), String>
where
    F: FnMut(&[Bytes]) -> Result<Vec<String>, Error>,
{
    warm_up(fixture, chunks, &mut decode)?;
    let mut durations = Vec::with_capacity(samples);
    for _ in 0..samples {
        durations.push(timed_sample(fixture, chunks, &mut decode)?);
    }
    report(fixture, chunks, label, &mut durations, samples);
    Ok(())
}

#[cfg(feature = "baseline")]
fn measure_pair(fixture: &Fixture, chunks: &[Bytes], samples: usize) -> Result<(), String> {
    warm_up(fixture, chunks, run_current)?;
    warm_up(fixture, chunks, run_baseline)?;
    let mut current_durations = Vec::with_capacity(samples);
    let mut baseline_durations = Vec::with_capacity(samples);
    for sample_index in 0..samples {
        if sample_index % 2 == 0 {
            current_durations.push(timed_sample(fixture, chunks, run_current)?);
            baseline_durations.push(timed_sample(fixture, chunks, run_baseline)?);
        } else {
            baseline_durations.push(timed_sample(fixture, chunks, run_baseline)?);
            current_durations.push(timed_sample(fixture, chunks, run_current)?);
        }
    }
    report(fixture, chunks, "current", &mut current_durations, samples);
    report(
        fixture,
        chunks,
        "baseline",
        &mut baseline_durations,
        samples,
    );
    Ok(())
}

fn run() -> Result<(), String> {
    let (samples, target_bytes) = parse_settings()?;
    for fixture in fixtures(target_bytes) {
        let chunks = make_chunks(&fixture.wire, fixture.chunk_bytes);
        #[cfg(feature = "baseline")]
        measure_pair(&fixture, &chunks, samples)?;
        #[cfg(not(feature = "baseline"))]
        measure(&fixture, &chunks, "current", samples, run_current)?;
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("router_performance: {error}");
        std::process::exit(2);
    }
}
