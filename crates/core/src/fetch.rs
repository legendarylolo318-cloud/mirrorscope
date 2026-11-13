use std::time::Duration;

use crate::{Error, Result, StatusDocument, STATUS_URL};

/// Upper bound for the status document (the real one is ~0.5 MB).
const MAX_STATUS_BYTES: usize = 16 * 1024 * 1024;

pub fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("mirrorscope/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(timeout)
        .build()?)
}

/// Turn a reqwest error into a message a user can act on.
fn describe(url: &str, e: reqwest::Error) -> Error {
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned());
    let msg = if e.is_timeout() {
        format!("timed out talking to {host} (slow or no network connection?)")
    } else if e.is_connect() {
        format!("cannot connect to {host} (are you offline?)")
    } else if let Some(s) = e.status() {
        format!("{host} returned HTTP {s} (the mirror status service may be down; try again later)")
    } else if e.is_decode() || e.is_body() {
        format!("connection to {host} broke while downloading the mirror status")
    } else {
        format!("request to {host} failed: {e}")
    };
    Error::Other(msg)
}

/// Download and parse the mirror status document.
pub async fn fetch_status(client: &reqwest::Client, url: Option<&str>) -> Result<StatusDocument> {
    let url = url.unwrap_or(STATUS_URL);
    let mut resp = client
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| describe(url, e))?;
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| describe(url, e))? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_STATUS_BYTES {
            return Err(Error::Other(format!(
                "mirror status from {url} is unexpectedly large"
            )));
        }
    }
    let text = String::from_utf8(body)
        .map_err(|_| Error::Other(format!("mirror status from {url} is not UTF-8")))?;
    StatusDocument::parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn serve_once(status: &'static str, body: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(body.as_bytes()).await;
            }
        });
        format!("http://{addr}/json/")
    }

    #[tokio::test]
    async fn parses_served_fixture() {
        let url = serve_once("200 OK", include_str!("../tests/fixtures/status.json")).await;
        let c = http_client(Duration::from_secs(2)).unwrap();
        let doc = fetch_status(&c, Some(&url)).await.unwrap();
        assert_eq!(doc.urls.len(), 9);
    }

    #[tokio::test]
    async fn readable_errors() {
        let c = http_client(Duration::from_secs(2)).unwrap();
        let down = serve_once("503 Service Unavailable", "").await;
        let e = fetch_status(&c, Some(&down)).await.unwrap_err().to_string();
        assert!(e.contains("HTTP 503"), "{e}");
        let e = fetch_status(&c, Some("http://127.0.0.1:1/"))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("offline"), "{e}");
        let garbage = serve_once("200 OK", "<html>maintenance</html>").await;
        let e = fetch_status(&c, Some(&garbage))
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("parse"), "{e}");
    }
}
