use std::time::Duration;

use anyhow::Result;
use reqwest::{header::{HeaderValue, RETRY_AFTER}, Client, StatusCode, Url};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("LRCLIB HTTP {status_code} ({name}): {message}")]
pub struct ApiError {
    pub status_code: u16,
    name: String,
    message: String,
}

pub fn api_error(status: StatusCode, body: &Value) -> anyhow::Error {
    let name = body
        .get("error")
        .or_else(|| body.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("HTTP error");
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| status.canonical_reason())
        .unwrap_or("Unknown error");
    ApiError {
        status_code: status.as_u16(),
        name: name.to_owned(),
        message: message.to_owned(),
    }
    .into()
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn retry_delay(attempt: usize, retry_after: Option<&HeaderValue>) -> Duration {
    let suggested_seconds = retry_after
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Duration::from_secs((2_u64 << attempt).max(suggested_seconds).min(30))
}

/// Retry only read-only requests and transient transport/server failures.
pub async fn get_json(client: &Client, url: Url) -> Result<(StatusCode, Value)> {
    for attempt in 0..3 {
        let response = client.get(url.clone()).send().await;
        match response {
            Ok(response) => {
                let status = response.status();
                if attempt < 2 && retryable_status(status) {
                    tokio::time::sleep(retry_delay(attempt, response.headers().get(RETRY_AFTER))).await;
                    continue;
                }
                if status == StatusCode::NOT_FOUND {
                    return Ok((status, Value::Null));
                }
                match response.json::<Value>().await {
                    Ok(body) => return Ok((status, body)),
                    Err(error) if attempt < 2 && (error.is_body() || error.is_decode() || error.is_timeout()) => {
                        tokio::time::sleep(retry_delay(attempt, None)).await;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) if attempt < 2 && (error.is_connect() || error.is_timeout() || error.is_body() || error.is_request()) => {
                tokio::time::sleep(retry_delay(attempt, None)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::{api_error, retry_delay, retryable_status};
    use reqwest::{header::HeaderValue, StatusCode};
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn accepts_lrclib_validation_error_shape() {
        let error = api_error(
            StatusCode::BAD_REQUEST,
            &json!({"name": "ValidationError", "statusCode": 400, "message": "invalid track_name"}),
        );
        assert_eq!(error.to_string(), "LRCLIB HTTP 400 (ValidationError): invalid track_name");
    }

    #[test]
    fn server_busy_retries_after_at_least_two_seconds() {
        assert!(retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(!retryable_status(StatusCode::BAD_REQUEST));
        let retry_after = HeaderValue::from_static("1");
        assert_eq!(retry_delay(0, Some(&retry_after)), Duration::from_secs(2));
        assert_eq!(retry_delay(1, None), Duration::from_secs(4));
    }
}
