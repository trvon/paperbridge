use std::time::Duration;

use reqwest::multipart::{Form, Part};
use tokio::time::sleep;
use tracing::debug;

use crate::error::{Result, ZoteroMcpError};
use crate::security::ensure_secure_transport;

#[derive(Clone)]
pub struct GrobidClient {
    base_url: String,
    http: reqwest::Client,
}

impl GrobidClient {
    pub fn new(base_url: impl Into<String>, timeout_secs: u64) -> Result<Self> {
        let base = base_url.into().trim_end_matches('/').to_string();
        ensure_secure_transport(&base)?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(timeout_secs.max(10)))
            .build()
            .map_err(|e| ZoteroMcpError::Http(format!("failed to build GROBID HTTP client: {e}")))?;
        Ok(Self { base_url: base, http })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn is_alive(&self) -> bool {
        let url = format!("{}/api/isalive", self.base_url);
        match self.http.get(&url).timeout(Duration::from_secs(3)).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(err) => {
                debug!(%url, %err, "grobid isalive probe failed");
                false
            }
        }
    }

    pub async fn wait_until_alive(&self, max_wait: Duration) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < max_wait {
            if self.is_alive().await {
                return true;
            }
            sleep(Duration::from_millis(1500)).await;
        }
        false
    }

    pub async fn process_fulltext(&self, pdf_bytes: Vec<u8>) -> Result<String> {
        let url = format!("{}/api/processFulltextDocument", self.base_url);
        let part = Part::bytes(pdf_bytes)
            .file_name("paper.pdf")
            .mime_str("application/pdf")
            .map_err(|e| ZoteroMcpError::Http(format!("grobid multipart part error: {e}")))?;
        let form = Form::new()
            .part("input", part)
            .text("consolidateHeader", "1")
            .text("consolidateCitations", "0")
            .text("includeRawAffiliations", "0")
            .text("segmentSentences", "0");

        debug!(%url, "grobid processFulltextDocument start");
        let response = self
            .http
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ZoteroMcpError::Http(format!("grobid request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read error body>".to_string());
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("grobid {status}: {body}"),
            });
        }

        response
            .text()
            .await
            .map_err(|e| ZoteroMcpError::Http(format!("grobid response read failed: {e}")))
    }
}
