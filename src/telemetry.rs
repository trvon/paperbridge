//! Tracing subscriber setup.
//!
//! `PAPERBRIDGE_TRACE_SPANS` controls span timing output on stderr:
//! - `off` (default): events only
//! - `close`: one line per closed span with `time.busy` / `time.idle`
//! - `json`: same as `close`, as newline-delimited JSON
//!
//! `RUST_LOG` overrides the configured log level. Span timing is emitted at
//! the span's level, so `close` with the default `info` level shows only
//! info spans; use `RUST_LOG=paperbridge=debug` to see every stage.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;

pub const TRACE_SPANS_ENV: &str = "PAPERBRIDGE_TRACE_SPANS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanMode {
    Off,
    Close,
    Json,
}

impl SpanMode {
    /// Unknown values fall back to `Off` so a typo never breaks startup.
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("close" | "1" | "true" | "on") => Self::Close,
            Some("json") => Self::Json,
            _ => Self::Off,
        }
    }

    pub fn from_env() -> Self {
        Self::parse(std::env::var(TRACE_SPANS_ENV).ok().as_deref())
    }
}

/// Install the global subscriber. Safe to call once per process.
pub fn init(log_level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    match SpanMode::from_env() {
        SpanMode::Off => builder.init(),
        SpanMode::Close => builder.with_span_events(FmtSpan::CLOSE).init(),
        SpanMode::Json => builder
            .json()
            .with_current_span(false)
            .with_span_events(FmtSpan::CLOSE)
            .init(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_mode_parses_known_values_and_defaults_off() {
        assert_eq!(SpanMode::parse(None), SpanMode::Off);
        assert_eq!(SpanMode::parse(Some("off")), SpanMode::Off);
        assert_eq!(SpanMode::parse(Some(" Close ")), SpanMode::Close);
        assert_eq!(SpanMode::parse(Some("1")), SpanMode::Close);
        assert_eq!(SpanMode::parse(Some("JSON")), SpanMode::Json);
        assert_eq!(SpanMode::parse(Some("bogus")), SpanMode::Off);
    }
}
