use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::SPEEDTEST_PATH;

#[derive(Debug, Clone, Copy)]
pub struct SpeedOptions {
    pub concurrency: usize,
    pub timeout: Duration,
}

impl Default for SpeedOptions {
    fn default() -> Self {
        SpeedOptions {
            concurrency: 8,
            timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpeedOutcome {
    /// Bytes per second.
    Rate(f64),
    Timeout,
    Error(String),
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum SpeedEvent {
    Started(String),
    Finished(String, SpeedOutcome),
}

/// URL of the test file for a mirror base URL.
pub fn test_url(base: &str) -> String {
    let mut b = base.trim().to_string();
    if !b.ends_with('/') {
        b.push('/');
    }
    b.push_str(SPEEDTEST_PATH);
    b
}

/// Download the test file once and compute throughput.
pub async fn measure_one(
    client: &reqwest::Client,
    base: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> SpeedOutcome {
    if base.starts_with("rsync://") {
        return SpeedOutcome::Error("rsync not supported".into());
    }
    let url = test_url(base);
    let work = async {
        let start = Instant::now();
        let mut resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => return SpeedOutcome::Error(short_err(&e)),
        };
        if !resp.status().is_success() {
            return SpeedOutcome::Error(format!("HTTP {}", resp.status().as_u16()));
        }
        let mut total: u64 = 0;
        loop {
            match resp.chunk().await {
                Ok(Some(c)) => total += c.len() as u64,
                Ok(None) => break,
                Err(e) => return SpeedOutcome::Error(short_err(&e)),
            }
        }
        let secs = start.elapsed().as_secs_f64().max(1e-6);
        if total == 0 {
            return SpeedOutcome::Error("empty response".into());
        }
        SpeedOutcome::Rate(total as f64 / secs)
    };
    tokio::select! {
        _ = cancel.cancelled() => SpeedOutcome::Cancelled,
        r = tokio::time::timeout(timeout, work) => r.unwrap_or(SpeedOutcome::Timeout),
    }
}

fn short_err(e: &reqwest::Error) -> String {
    if e.is_connect() {
        "connect failed".into()
    } else if e.is_timeout() {
        "timeout".into()
    } else {
        "request failed".into()
    }
}

/// Test many mirrors concurrently, reporting each start/finish through `on_event`.
/// Returns when all are done or cancelled.
pub async fn run_speedtest<F>(
    client: reqwest::Client,
    bases: Vec<String>,
    opts: SpeedOptions,
    cancel: CancellationToken,
    on_event: F,
) where
    F: Fn(SpeedEvent) + Send + Sync + 'static,
{
    let sem = Arc::new(Semaphore::new(opts.concurrency.max(1)));
    let on_event = Arc::new(on_event);
    let mut set = JoinSet::new();
    for base in bases {
        let sem = sem.clone();
        let client = client.clone();
        let cancel = cancel.clone();
        let cb = on_event.clone();
        set.spawn(async move {
            let permit = tokio::select! {
                _ = cancel.cancelled() => None,
                p = sem.acquire_owned() => p.ok(),
            };
            let Some(_permit) = permit else {
                cb(SpeedEvent::Finished(base, SpeedOutcome::Cancelled));
                return;
            };
            cb(SpeedEvent::Started(base.clone()));
            let out = measure_one(&client, &base, opts.timeout, &cancel).await;
            cb(SpeedEvent::Finished(base, out));
        });
    }
    while set.join_next().await.is_some() {}
}

/// Format bytes/second for display.
pub fn format_rate(bps: f64) -> String {
    format!("{:.2} MB/s", bps / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn serve(body: Vec<u8>, status: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    break;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = s.read(&mut buf).await;
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(&body).await;
                });
            }
        });
        format!("http://{addr}/")
    }

    #[test]
    fn builds_test_url() {
        assert_eq!(
            test_url("https://m.example/arch"),
            "https://m.example/arch/core/os/x86_64/core.db"
        );
    }

    #[tokio::test]
    async fn measures_rate_and_handles_errors() {
        let ok = serve(vec![1u8; 50_000], "200 OK").await;
        let bad = serve(vec![], "404 Not Found").await;
        let dead = "http://127.0.0.1:1/".to_string();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        run_speedtest(
            reqwest::Client::new(),
            vec![ok.clone(), bad.clone(), dead.clone()],
            SpeedOptions {
                concurrency: 2,
                timeout: Duration::from_secs(3),
            },
            CancellationToken::new(),
            move |e| ev.lock().unwrap().push(e),
        )
        .await;
        let ev = events.lock().unwrap();
        let find = |u: &str| {
            ev.iter().find_map(|e| match e {
                SpeedEvent::Finished(b, o) if b == u => Some(o.clone()),
                _ => None,
            })
        };
        assert!(matches!(find(&ok), Some(SpeedOutcome::Rate(r)) if r > 0.0));
        assert_eq!(find(&bad), Some(SpeedOutcome::Error("HTTP 404".into())));
        assert!(matches!(find(&dead), Some(SpeedOutcome::Error(_))));
    }

    #[tokio::test]
    async fn cancellation_stops_work() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let out = measure_one(
            &reqwest::Client::new(),
            "http://127.0.0.1:1/",
            Duration::from_secs(1),
            &cancel,
        )
        .await;
        assert_eq!(out, SpeedOutcome::Cancelled);
    }

    #[test]
    fn formats_rate() {
        assert_eq!(format_rate(2_500_000.0), "2.50 MB/s");
    }
}
