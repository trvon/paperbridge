use std::time::Duration;

use tokio::process::Command;
use tracing::debug;

use crate::error::{Result, ZoteroMcpError};
use crate::paper::grobid::GrobidClient;

pub const DEFAULT_PORT: u16 = 8070;

pub async fn ensure_grobid_ready(image: &str, port: u16) -> Result<String> {
    let base_url = format!("http://localhost:{port}");
    let probe = GrobidClient::new(&base_url, 30)?;
    if probe.is_alive().await {
        debug!(%base_url, "reusing already-running GROBID instance");
        return Ok(base_url);
    }

    debug!(image, port, "spawning GROBID container");
    let output = Command::new("docker")
        .args(["run", "-d", "--rm", "-p", &format!("{port}:8070"), image])
        .output()
        .await
        .map_err(|e| {
            ZoteroMcpError::Http(format!(
                "failed to exec `docker` (is Docker installed and on PATH?): {e}"
            ))
        })?;

    if !output.status.success() {
        return Err(ZoteroMcpError::Http(format!(
            "`docker run` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    if !probe.wait_until_alive(Duration::from_secs(180)).await {
        return Err(ZoteroMcpError::Http(format!(
            "GROBID container started but did not become ready on {base_url} within 180s"
        )));
    }

    Ok(base_url)
}
