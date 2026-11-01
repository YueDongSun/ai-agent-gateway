//! Bounded HTTP fetch for the fixed public catalog sources.
//!
//! No credentials, fixed `https` URLs, a hard size cap, a hard timeout, and
//! stable failure codes: raw bodies, URLs and transport errors are never
//! surfaced to the UI or the logs.

use std::time::Duration;

const FETCH_TIMEOUT_SECS: u64 = 20;
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// Stable fetch failure codes; doubles as the cache `last_error` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FetchFailure {
    Timeout,
    Transport,
    HttpStatus(u16),
    TooLarge,
    Decode,
}

impl FetchFailure {
    pub(crate) fn code(self) -> &'static str {
        match self {
            FetchFailure::Timeout => "fetch_timeout",
            FetchFailure::Transport => "fetch_transport",
            FetchFailure::HttpStatus(_) => "fetch_http_error",
            FetchFailure::TooLarge => "fetch_too_large",
            FetchFailure::Decode => "fetch_decode_failed",
        }
    }
}

/// Fetch one public document with a bounded timeout and body size. The body
/// is read in chunks so a lying `content-length` cannot blow past `max_bytes`.
pub(crate) async fn fetch_document(url: &str, max_bytes: usize) -> Result<String, FetchFailure> {
    fetch_document_with(url, max_bytes, FETCH_TIMEOUT_SECS).await
}

async fn fetch_document_with(
    url: &str,
    max_bytes: usize,
    timeout_secs: u64,
) -> Result<String, FetchFailure> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS.min(timeout_secs)))
        .build()
        .map_err(|_| FetchFailure::Transport)?;
    let mut response = client.get(url).send().await.map_err(|error| {
        if error.is_timeout() {
            FetchFailure::Timeout
        } else {
            FetchFailure::Transport
        }
    })?;
    if !response.status().is_success() {
        return Err(FetchFailure::HttpStatus(response.status().as_u16()));
    }
    if let Some(length) = response.content_length() {
        if length > max_bytes as u64 {
            return Err(FetchFailure::TooLarge);
        }
    }
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let chunk = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                FetchFailure::Timeout
            } else {
                FetchFailure::Transport
            }
        })?;
        let Some(chunk) = chunk else {
            break;
        };
        append_chunk(&mut buffer, &chunk, max_bytes)?;
    }
    String::from_utf8(buffer).map_err(|_| FetchFailure::Decode)
}

/// Accumulate one chunk under the hard cap; a lying `content-length` cannot
/// smuggle an oversized body past this check.
fn append_chunk(buffer: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> Result<(), FetchFailure> {
    if buffer.len().saturating_add(chunk.len()) > max_bytes {
        return Err(FetchFailure::TooLarge);
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "reads fixed public Arena row/statistics metadata without updating caches"]
    async fn live_reqwest_arena_rows_metadata() {
        assert_eq!(
            std::env::var("AAG_MIRROR_LOADING_ACCEPTANCE").as_deref(),
            Ok("ARENA_ROWS_PROBE")
        );
        for (endpoint,url) in [
            ("rows","https://datasets-server.huggingface.co/rows?dataset=lmarena-ai%2Fleaderboard-dataset&config=text_style_control&split=latest&offset=0&length=1"),
            ("statistics","https://datasets-server.huggingface.co/statistics?dataset=lmarena-ai%2Fleaderboard-dataset&config=text_style_control&split=latest"),
        ] {
            let started=std::time::Instant::now();
            let result=fetch_document_with(url,1024*1024,20).await;
            println!("{}",serde_json::json!({"transport":"reqwest","endpoint":endpoint,"elapsed_ms":started.elapsed().as_millis(),"result":result.as_ref().map(|body|body.len()).map_err(|failure|failure.code())}));
            assert!(result.is_ok(),"fixed public metadata transport unavailable");
        }
    }

    async fn serve<F, Fut>(handler: F) -> String
    where
        F: Fn(axum::extract::Request) -> Fut + Clone + Send + Sync + 'static,
        Fut: std::future::Future<Output = axum::response::Response> + Send + 'static,
    {
        let app = axum::Router::new().fallback(axum::routing::any(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        // The test caller owns shutdown through the returned address; keep
        // the server alive by leaking the shutdown sender into the address
        // string protocol: tests stop by dropping the listener task at the
        // end of the runtime.
        std::mem::forget(shutdown);
        let _ = server;
        format!("http://{address}")
    }

    #[tokio::test]
    async fn bounded_fetch_returns_bodies_under_the_cap() {
        let base = serve(|_request| async {
            axum::response::Response::new(axum::body::Body::from("{\"ok\": true}"))
        })
        .await;
        let body = fetch_document_with(&format!("{base}/doc.json"), 1024, 5)
            .await
            .expect("fetch succeeds");
        assert_eq!(body, "{\"ok\": true}");
    }

    #[tokio::test]
    async fn oversize_bodies_fail_closed() {
        let base = serve(|_request| async {
            axum::response::Response::new(axum::body::Body::from("x".repeat(4096)))
        })
        .await;
        assert_eq!(
            fetch_document_with(&format!("{base}/doc.json"), 128, 5)
                .await
                .unwrap_err(),
            FetchFailure::TooLarge
        );
    }

    #[test]
    fn chunk_accumulation_is_bounded_regardless_of_advertised_length() {
        let mut buffer = Vec::new();
        assert!(append_chunk(&mut buffer, &[0_u8; 100], 128).is_ok());
        // One byte over the cap is refused even after partial accumulation.
        assert_eq!(
            append_chunk(&mut buffer, &[0_u8; 29], 128).unwrap_err(),
            FetchFailure::TooLarge
        );
        assert_eq!(buffer.len(), 100);
    }

    #[tokio::test]
    async fn http_failures_and_invalid_utf8_map_to_stable_codes() {
        let base = serve(|request: axum::extract::Request| async move {
            if request.uri().path().ends_with("boom") {
                axum::response::Response::builder()
                    .status(503)
                    .body(axum::body::Body::empty())
                    .unwrap()
            } else {
                axum::response::Response::new(axum::body::Body::from(vec![0xff, 0xfe, 0xfd]))
            }
        })
        .await;
        assert_eq!(
            fetch_document_with(&format!("{base}/boom"), 1024, 5)
                .await
                .unwrap_err(),
            FetchFailure::HttpStatus(503)
        );
        assert_eq!(
            fetch_document_with(&format!("{base}/binary"), 1024, 5)
                .await
                .unwrap_err(),
            FetchFailure::Decode
        );
        assert_eq!(FetchFailure::HttpStatus(503).code(), "fetch_http_error");
        assert_eq!(FetchFailure::Timeout.code(), "fetch_timeout");
    }

    #[tokio::test]
    async fn timeouts_fail_closed_with_a_stable_code() {
        let base = serve(|_request| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            axum::response::Response::new(axum::body::Body::empty())
        })
        .await;
        let failure = fetch_document_with(&format!("{base}/slow"), 1024, 1)
            .await
            .unwrap_err();
        assert!(
            matches!(failure, FetchFailure::Timeout | FetchFailure::Transport),
            "unexpected failure: {failure:?}"
        );
    }
}
