use std::time::Duration;

use crate::{Result, StatusDocument, STATUS_URL};

pub fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("mirrorscope/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(timeout)
        .build()?)
}

/// Download and parse the mirror status document.
pub async fn fetch_status(client: &reqwest::Client, url: Option<&str>) -> Result<StatusDocument> {
    let resp = client
        .get(url.unwrap_or(STATUS_URL))
        .timeout(Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?;
    let body = resp.text().await?;
    StatusDocument::parse(&body)
}
