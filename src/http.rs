use std::{
    sync::OnceLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use reqwest::{StatusCode, header::HeaderMap};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::{debug, info, warn};

#[derive(Debug)]
struct ApiRequestThrottle {
    min_interval: Duration,
    next_allowed: tokio::sync::Mutex<Option<Instant>>,
}

impl ApiRequestThrottle {
    fn from_env() -> Self {
        let configured_ms = crate::first_non_empty_env(&["ARC_API_MIN_INTERVAL_MS"])
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(crate::DEFAULT_API_MIN_INTERVAL_MS);
        let min_interval = Duration::from_millis(configured_ms);
        info!(
            min_interval_ms = configured_ms,
            "api_throttle: configured global API minimum interval"
        );
        Self {
            min_interval,
            next_allowed: tokio::sync::Mutex::new(None),
        }
    }

    async fn wait_turn(&self, endpoint_hint: Option<&str>) {
        if self.min_interval.is_zero() {
            return;
        }

        let now = Instant::now();
        let delay = {
            let mut next_allowed = self.next_allowed.lock().await;
            let scheduled = next_allowed.filter(|next| *next > now).unwrap_or(now);
            *next_allowed = Some(scheduled + self.min_interval);
            scheduled.saturating_duration_since(now)
        };

        if !delay.is_zero() {
            debug!(
                wait_ms = delay.as_millis() as u64,
                endpoint = endpoint_hint.unwrap_or("unknown"),
                "api_throttle: delaying request"
            );
            tokio::time::sleep(delay).await;
        }
    }
}

fn api_request_throttle() -> &'static ApiRequestThrottle {
    static API_REQUEST_THROTTLE: OnceLock<ApiRequestThrottle> = OnceLock::new();
    API_REQUEST_THROTTLE.get_or_init(ApiRequestThrottle::from_env)
}

#[derive(Debug)]
struct ApiRetryConfig {
    max_retries: usize,
    base_delay: Duration,
    max_delay: Duration,
}

impl ApiRetryConfig {
    fn from_env() -> Self {
        let max_retries = crate::first_non_empty_env(&["ARC_API_MAX_RETRIES"])
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(crate::DEFAULT_API_MAX_RETRIES);
        let base_ms = crate::first_non_empty_env(&["ARC_API_RETRY_BASE_MS"])
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(crate::DEFAULT_API_RETRY_BASE_MS);
        let max_ms = crate::first_non_empty_env(&["ARC_API_RETRY_MAX_MS"])
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(crate::DEFAULT_API_RETRY_MAX_MS)
            .max(base_ms);

        info!(
            max_retries,
            retry_base_ms = base_ms,
            retry_max_ms = max_ms,
            "api_retry: configured retry policy"
        );

        Self {
            max_retries,
            base_delay: Duration::from_millis(base_ms),
            max_delay: Duration::from_millis(max_ms),
        }
    }

    fn delay_for_attempt(&self, attempt: usize) -> Duration {
        if self.base_delay.is_zero() {
            return Duration::from_millis(0);
        }
        let exp = attempt.min(10) as u32;
        let multiplier = 1u64 << exp;
        let base_ms = self.base_delay.as_millis() as u64;
        let max_ms = self.max_delay.as_millis() as u64;
        Duration::from_millis(base_ms.saturating_mul(multiplier).min(max_ms))
    }
}

fn api_retry_config() -> &'static ApiRetryConfig {
    static API_RETRY_CONFIG: OnceLock<ApiRetryConfig> = OnceLock::new();
    API_RETRY_CONFIG.get_or_init(ApiRetryConfig::from_env)
}

pub async fn get_json<T>(request: reqwest::RequestBuilder) -> Result<T>
where
    T: DeserializeOwned,
{
    let request_template = request
        .try_clone()
        .ok_or_else(|| anyhow!("HTTP request could not be cloned for retries"))?;

    let request_meta = request_template
        .try_clone()
        .and_then(|builder| builder.build().ok())
        .map(|req| (req.method().to_string(), req.url().to_string()));

    if let Some((method, url)) = request_meta.as_ref() {
        debug!(%method, %url, "get_json: sending request");
    } else {
        debug!("get_json: sending request");
    }

    let retry = api_retry_config();
    for attempt in 0..=retry.max_retries {
        api_request_throttle()
            .wait_turn(request_meta.as_ref().map(|(_, url)| url.as_str()))
            .await;

        let request_for_attempt = request_template
            .try_clone()
            .ok_or_else(|| anyhow!("HTTP request could not be cloned for retries"))?;

        let response = match request_for_attempt.send().await {
            Ok(response) => response,
            Err(err) => {
                if should_retry_transport(&err) && attempt < retry.max_retries {
                    let delay = retry.delay_for_attempt(attempt);
                    warn!(
                        attempt = attempt + 1,
                        max_retries = retry.max_retries,
                        retry_in_ms = delay.as_millis() as u64,
                        error = %err,
                        endpoint = request_meta
                            .as_ref()
                            .map(|(_, url)| url.as_str())
                            .unwrap_or("unknown"),
                        "get_json: transport error, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(err).context("HTTP request failed");
            }
        };

        let status = response.status();
        let headers = response.headers().clone();
        if let Some(rate) = extract_rate_limit_info(&headers) {
            debug!(
                limit = rate.limit,
                remaining = rate.remaining,
                reset_unix = rate.reset_unix,
                endpoint = request_meta
                    .as_ref()
                    .map(|(_, url)| url.as_str())
                    .unwrap_or("unknown"),
                "get_json: rate limit headers"
            );
        }
        let body = response
            .text()
            .await
            .context("failed reading HTTP response")?;

        if status.is_success() {
            if let Some((method, url)) = request_meta.as_ref() {
                debug!(%method, %url, status = %status, "get_json: request succeeded");
            }

            return serde_json::from_str(&body).context("failed to parse JSON response");
        }

        let snippet: String = body.chars().take(500).collect();
        let request_id = extract_request_id(&body);

        if let Some((method, url)) = request_meta.as_ref() {
            if let Some(request_id) = request_id.as_ref() {
                warn!(
                    %method,
                    %url,
                    status = %status,
                    request_id,
                    body = %snippet,
                    "get_json: HTTP error"
                );
            } else {
                warn!(
                    %method,
                    %url,
                    status = %status,
                    body = %snippet,
                    "get_json: HTTP error"
                );
            }
        } else if let Some(request_id) = request_id.as_ref() {
            warn!(
                status = %status,
                request_id,
                body = %snippet,
                "get_json: HTTP error"
            );
        } else {
            warn!(status = %status, body = %snippet, "get_json: HTTP error");
        }

        if should_retry_status(status) && attempt < retry.max_retries {
            let delay = retry_delay_for_status(status, &headers, retry, attempt);
            warn!(
                attempt = attempt + 1,
                max_retries = retry.max_retries,
                retry_in_ms = delay.as_millis() as u64,
                status = %status,
                endpoint = request_meta
                    .as_ref()
                    .map(|(_, url)| url.as_str())
                    .unwrap_or("unknown"),
                "get_json: retrying after HTTP error"
            );
            tokio::time::sleep(delay).await;
            continue;
        }

        if let Some(request_id) = request_id {
            return Err(anyhow!(
                "HTTP {} (requestId={}): {}",
                status,
                request_id,
                snippet
            ));
        }
        return Err(anyhow!("HTTP {}: {}", status, snippet));
    }

    Err(anyhow!("HTTP request retries exhausted"))
}

fn should_retry_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn should_retry_transport(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

#[derive(Debug, Clone, Copy)]
struct RateLimitInfo {
    limit: u64,
    remaining: u64,
    reset_unix: u64,
}

fn extract_rate_limit_info(headers: &HeaderMap) -> Option<RateLimitInfo> {
    let limit = headers
        .get("X-RateLimit-Limit")
        .or_else(|| headers.get("x-ratelimit-limit"))
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.parse::<u64>().ok())?;

    let remaining = headers
        .get("X-RateLimit-Remaining")
        .or_else(|| headers.get("x-ratelimit-remaining"))
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.parse::<u64>().ok())?;

    let reset_unix = headers
        .get("X-RateLimit-Reset")
        .or_else(|| headers.get("x-ratelimit-reset"))
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.parse::<u64>().ok())?;

    Some(RateLimitInfo {
        limit,
        remaining,
        reset_unix,
    })
}

fn retry_delay_for_status(
    status: StatusCode,
    headers: &HeaderMap,
    retry: &ApiRetryConfig,
    attempt: usize,
) -> Duration {
    if status == StatusCode::TOO_MANY_REQUESTS
        && let Some(rate) = extract_rate_limit_info(headers)
    {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        if rate.reset_unix > now_unix {
            let until_reset_ms = (rate.reset_unix - now_unix).saturating_mul(1000);
            if until_reset_ms > 0 {
                return Duration::from_millis(
                    until_reset_ms.min(retry.max_delay.as_millis() as u64),
                );
            }
        }
    }
    retry.delay_for_attempt(attempt)
}

pub fn extract_http_status_code_from_error(err: &anyhow::Error) -> Option<u16> {
    let text = err.to_string();
    let start = text.find("HTTP ")? + 5;
    let digits: String = text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<u16>().ok()
}

pub fn extract_request_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("meta")
        .and_then(|meta| {
            meta.get("requestId")
                .or_else(|| meta.get("request_id"))
                .or_else(|| meta.get("requestID"))
        })
        .and_then(value_as_string)
}

pub fn extract_request_id_from_error(err: &anyhow::Error) -> Option<String> {
    let text = err.to_string();
    if let Some(pos) = text.find("requestId=") {
        let rest = &text[pos + "requestId=".len()..];
        let id: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        if !id.is_empty() {
            return Some(id);
        }
    }

    if let Some(pos) = text.find("\"requestId\":\"") {
        let rest = &text[pos + "\"requestId\":\"".len()..];
        let id: String = rest.chars().take_while(|ch| *ch != '"').collect();
        if !id.is_empty() {
            return Some(id);
        }
    }

    None
}

pub fn truncate_for_report(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

fn extract_request_id(body: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    parsed
        .get("meta")
        .and_then(|meta| {
            meta.get("requestId")
                .or_else(|| meta.get("request_id"))
                .or_else(|| meta.get("requestID"))
        })
        .and_then(value_as_string)
}

fn value_as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        _ => None,
    }
}
