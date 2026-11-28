//! Injected I/O seams for the publish pipeline, so the whole Direct Upload sequence is
//! host-testable without a network: [`HttpClient`] (the wasm implementation uses
//! `worker::Fetch`), [`Runtime`] (clock, backoff sleep, log sink) and [`Secrets`]
//! (the Cloudflare API token lookup).
//!
//! The traits use `async fn` without `Send` bounds on purpose: a Worker isolate is
//! single-threaded, and callers are generic (never `dyn`).

use super::secret::ApiToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    /// Header pairs. `Authorization` values are secrets: never log a request.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Upper bound for this request; the wasm client aborts it after this many ms.
    pub timeout_ms: u64,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// `Retry-After` in whole seconds, when the server sent one.
    pub retry_after_secs: Option<u64>,
    pub body: Vec<u8>,
}

/// A request that produced no HTTP response. Whether the server acted on it is
/// unknown, so a non-idempotent step (activation) must reconcile before retrying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    Timeout,
    Network(String),
}

#[allow(async_fn_in_trait)]
pub trait HttpClient {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError>;
}

#[allow(async_fn_in_trait)]
pub trait Runtime {
    /// Unix seconds.
    fn now_secs(&self) -> u64;
    async fn sleep_ms(&self, ms: u64);
    /// Operational log line. Callers pass codes and stages only, never bodies or
    /// headers; the pipeline also redacts every known secret before calling this.
    fn log(&self, line: &str);
}

/// Resolves the Cloudflare API token by Worker-secret NAME (the value is never
/// stored in D1). `None` when the secret is not configured.
pub trait Secrets {
    fn api_token(&self, secret_name: &str) -> Option<ApiToken>;
}
