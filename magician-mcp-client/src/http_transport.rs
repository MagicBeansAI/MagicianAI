use std::{borrow::Cow, collections::HashMap, io::Write, sync::Arc};

use bytes::Bytes;
use futures_util::{future, stream::BoxStream, Stream, StreamExt};
use http::{HeaderName, HeaderValue};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use rmcp::{
    model::{ClientJsonRpcMessage, JsonRpcMessage, ServerJsonRpcMessage},
    transport::{
        common::http_header::{
            EVENT_STREAM_MIME_TYPE, HEADER_LAST_EVENT_ID, HEADER_MCP_PROTOCOL_VERSION,
            HEADER_SESSION_ID, JSON_MIME_TYPE,
        },
        streamable_http_client::{
            AuthRequiredError, InsufficientScopeError, StreamableHttpClient,
            StreamableHttpClientTransportConfig, StreamableHttpError, StreamableHttpPostResponse,
        },
        StreamableHttpClientTransport,
    },
};
use sse_stream::{Error as SseError, Sse, SseStream};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::BearerToken;

const MAX_AUTH_CHALLENGE_BYTES: usize = 8 * 1024;
const MAX_CUSTOM_HEADER_COUNT: usize = 64;
const MAX_HEADER_BYTES: usize = 8 * 1024;

#[derive(Debug, Error)]
pub(crate) enum BoundedHttpError {
    #[error("HTTP request failed")]
    Request,
    #[error("HTTP response body exceeded the configured {maximum}-byte limit")]
    BodyTooLarge { maximum: usize },
    #[error("HTTP authorization value was invalid")]
    InvalidAuthorization,
    #[error("outbound HTTP message was invalid or exceeded its configured limit")]
    InvalidMessage,
}

#[derive(Debug, Error)]
enum BoundedSseBodyError {
    #[error("HTTP SSE source failed")]
    Source,
    #[error("SSE event exceeded the configured {maximum}-byte limit")]
    EventTooLarge { maximum: usize },
}

#[derive(Debug)]
struct SseEventSizeLimiter {
    maximum: usize,
    retained_size: usize,
    line_size: usize,
    line_is_comment: bool,
    previous_was_cr: bool,
}

impl SseEventSizeLimiter {
    fn new(maximum: usize) -> Self {
        Self {
            maximum,
            retained_size: 0,
            line_size: 0,
            line_is_comment: false,
            previous_was_cr: false,
        }
    }

    fn observe(&mut self, chunk: &[u8]) -> Result<(), ()> {
        for &byte in chunk {
            if self.previous_was_cr {
                self.previous_was_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' => {
                    self.finish_line()?;
                    self.previous_was_cr = true;
                },
                b'\n' => self.finish_line()?,
                _ => {
                    if self.line_size == 0 {
                        self.line_is_comment = byte == b':';
                    }
                    self.line_size = self.line_size.saturating_add(1);
                    self.check_limit()?;
                },
            }
        }
        Ok(())
    }

    fn finish_line(&mut self) -> Result<(), ()> {
        if self.line_size == 0 {
            self.retained_size = 0;
        } else if !self.line_is_comment {
            self.retained_size = self
                .retained_size
                .saturating_add(self.line_size)
                .saturating_add(1);
        }
        self.line_size = 0;
        self.line_is_comment = false;
        self.check_limit()
    }

    fn check_limit(&self) -> Result<(), ()> {
        if self.retained_size.saturating_add(self.line_size) > self.maximum {
            Err(())
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub(crate) struct BoundedHttpClient {
    client: reqwest::Client,
    bearer_token: Option<Arc<BearerToken>>,
    max_body_bytes: usize,
    max_sse_event_bytes: usize,
}

impl BoundedHttpClient {
    pub(crate) fn new(
        bearer_token: Option<BearerToken>,
        max_body_bytes: usize,
        max_sse_event_bytes: usize,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            bearer_token: bearer_token.map(Arc::new),
            max_body_bytes,
            max_sse_event_bytes,
        })
    }

    fn authorize(
        &self,
        mut request: reqwest::RequestBuilder,
        sdk_token: Option<String>,
    ) -> Result<reqwest::RequestBuilder, BoundedHttpError> {
        let sdk_token = sdk_token.map(Zeroizing::new);
        let token = sdk_token
            .as_ref()
            .map(|token| token.as_str())
            .or_else(|| self.bearer_token.as_deref().map(BearerToken::expose));
        if let Some(token) = token {
            let rendered = Zeroizing::new(format!("Bearer {token}"));
            let mut header = HeaderValue::from_str(rendered.as_str())
                .map_err(|_| BoundedHttpError::InvalidAuthorization)?;
            header.set_sensitive(true);
            request = request.header(AUTHORIZATION, header);
        }
        Ok(request)
    }

    fn apply_custom_headers(
        mut request: reqwest::RequestBuilder,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<reqwest::RequestBuilder, StreamableHttpError<BoundedHttpError>> {
        if custom_headers.len() > MAX_CUSTOM_HEADER_COUNT {
            return Err(StreamableHttpError::UnexpectedServerResponse(
                Cow::Borrowed("too many custom request headers"),
            ));
        }
        for (name, value) in custom_headers {
            let reserved = name == ACCEPT
                || name.as_str().eq_ignore_ascii_case(HEADER_SESSION_ID)
                || name.as_str().eq_ignore_ascii_case(HEADER_LAST_EVENT_ID);
            let protocol_version = name
                .as_str()
                .eq_ignore_ascii_case(HEADER_MCP_PROTOCOL_VERSION);
            if reserved && !protocol_version {
                return Err(StreamableHttpError::ReservedHeaderConflict(
                    name.to_string(),
                ));
            }
            if name.as_str().len() > 256 || value.as_bytes().len() > MAX_HEADER_BYTES {
                return Err(StreamableHttpError::UnexpectedServerResponse(
                    Cow::Borrowed("custom request header exceeded the configured limit"),
                ));
            }
            request = request.header(name, value);
        }
        Ok(request)
    }

    async fn bounded_body(
        &self,
        response: reqwest::Response,
    ) -> Result<Bytes, StreamableHttpError<BoundedHttpError>> {
        let content_length = response.content_length();
        match collect_bounded_chunks(response.bytes_stream(), content_length, self.max_body_bytes)
            .await
        {
            Ok(bytes) => Ok(bytes),
            Err(CollectBoundedError::Source(_)) => {
                Err(StreamableHttpError::Client(BoundedHttpError::Request))
            },
            Err(CollectBoundedError::TooLarge) => Err(StreamableHttpError::Client(
                BoundedHttpError::BodyTooLarge {
                    maximum: self.max_body_bytes,
                },
            )),
        }
    }
}

pub(crate) fn build_bounded_http_transport(
    endpoint: String,
    bearer_token: Option<BearerToken>,
    max_body_bytes: usize,
    max_sse_event_bytes: usize,
) -> Result<StreamableHttpClientTransport<BoundedHttpClient>, reqwest::Error> {
    let client = BoundedHttpClient::new(bearer_token, max_body_bytes, max_sse_event_bytes)?;
    let config = StreamableHttpClientTransportConfig::with_uri(endpoint)
        .max_sse_event_size(max_sse_event_bytes);
    Ok(StreamableHttpClientTransport::with_client(client, config))
}

impl StreamableHttpClient for BoundedHttpClient {
    type Error = BoundedHttpError;

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        self.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_token,
            custom_headers,
            self.max_sse_event_bytes,
        )
        .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        let mut request = self
            .client
            .get(uri.as_ref())
            .header(ACCEPT, [EVENT_STREAM_MIME_TYPE, JSON_MIME_TYPE].join(", "));
        if let Some(session_id) = session_id {
            request = request.header(
                HEADER_SESSION_ID,
                bounded_request_header_value(session_id.as_ref())?,
            );
        }
        if let Some(last_event_id) = last_event_id {
            request = request.header(
                HEADER_LAST_EVENT_ID,
                bounded_request_header_value(&last_event_id)?,
            );
        }
        request = self
            .authorize(request, auth_token)
            .map_err(StreamableHttpError::Client)?;
        request = Self::apply_custom_headers(request, custom_headers)?;
        let response = request
            .send()
            .await
            .map_err(|_| StreamableHttpError::Client(BoundedHttpError::Request))?;
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        let response = response
            .error_for_status()
            .map_err(|_| StreamableHttpError::Client(BoundedHttpError::Request))?;
        validate_sse_content_type(response.headers().get(CONTENT_TYPE))?;
        Ok(bounded_sse_stream(
            response.bytes_stream(),
            max_sse_event_size,
        ))
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        let request = self.client.delete(uri.as_ref()).header(
            HEADER_SESSION_ID,
            bounded_request_header_value(session_id.as_ref())?,
        );
        let request = self
            .authorize(request, auth_token)
            .map_err(StreamableHttpError::Client)?;
        let request = Self::apply_custom_headers(request, custom_headers)?;
        let response = request
            .send()
            .await
            .map_err(|_| StreamableHttpError::Client(BoundedHttpError::Request))?;
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Ok(());
        }
        response
            .error_for_status()
            .map_err(|_| StreamableHttpError::Client(BoundedHttpError::Request))?;
        Ok(())
    }

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_token,
            custom_headers,
            self.max_sse_event_bytes,
        )
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let mut request = self
            .client
            .post(uri.as_ref())
            .header(ACCEPT, [EVENT_STREAM_MIME_TYPE, JSON_MIME_TYPE].join(", "))
            .header(CONTENT_TYPE, JSON_MIME_TYPE);
        request = self
            .authorize(request, auth_token)
            .map_err(StreamableHttpError::Client)?;
        request = Self::apply_custom_headers(request, custom_headers)?;
        let session_was_attached = session_id.is_some();
        if let Some(session_id) = session_id {
            request = request.header(
                HEADER_SESSION_ID,
                bounded_request_header_value(session_id.as_ref())?,
            );
        }
        let body = serialize_bounded_json(&message, self.max_body_bytes)
            .map_err(|_| StreamableHttpError::Client(BoundedHttpError::InvalidMessage))?;
        let response = request
            .body(body)
            .send()
            .await
            .map_err(|_| StreamableHttpError::Client(BoundedHttpError::Request))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            if let Some(header) = response.headers().get(WWW_AUTHENTICATE) {
                return Err(StreamableHttpError::AuthRequired(AuthRequiredError::new(
                    bounded_auth_challenge(header)?,
                )));
            }
        }
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            if let Some(header) = response.headers().get(WWW_AUTHENTICATE) {
                let challenge = bounded_auth_challenge(header)?;
                return Err(StreamableHttpError::InsufficientScope(
                    InsufficientScopeError::new(challenge.clone(), extract_scope(&challenge)),
                ));
            }
        }

        let status = response.status();
        if matches!(
            status,
            reqwest::StatusCode::ACCEPTED | reqwest::StatusCode::NO_CONTENT
        ) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status == reqwest::StatusCode::NOT_FOUND && session_was_attached {
            return Err(StreamableHttpError::SessionExpired);
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .map(|value| value.as_bytes().to_vec());
        let content_length = response.content_length();
        let response_session_id = response
            .headers()
            .get(HEADER_SESSION_ID)
            .map(bounded_response_session_id)
            .transpose()?;

        if status.is_success()
            && content_length == Some(0)
            && matches!(
                message,
                ClientJsonRpcMessage::Notification(_)
                    | ClientJsonRpcMessage::Response(_)
                    | ClientJsonRpcMessage::Error(_)
            )
        {
            return Ok(StreamableHttpPostResponse::Accepted);
        }

        if !status.is_success() {
            let body = self.bounded_body(response).await?;
            if is_content_type(&content_type, JSON_MIME_TYPE) {
                if let Ok(message @ JsonRpcMessage::Error(_)) =
                    serde_json::from_slice::<ServerJsonRpcMessage>(&body)
                {
                    return Ok(StreamableHttpPostResponse::Json(
                        message,
                        response_session_id,
                    ));
                }
            }
            return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
                format!("HTTP {status}"),
            )));
        }

        if is_content_type(&content_type, EVENT_STREAM_MIME_TYPE) {
            return Ok(StreamableHttpPostResponse::Sse(
                bounded_sse_stream(response.bytes_stream(), max_sse_event_size),
                response_session_id,
            ));
        }
        if is_content_type(&content_type, JSON_MIME_TYPE) {
            let body = self.bounded_body(response).await?;
            return match serde_json::from_slice::<ServerJsonRpcMessage>(&body) {
                Ok(message) => Ok(StreamableHttpPostResponse::Json(
                    message,
                    response_session_id,
                )),
                Err(_) => Err(StreamableHttpError::UnexpectedServerResponse(
                    Cow::Borrowed("server returned malformed JSON"),
                )),
            };
        }
        Err(StreamableHttpError::UnexpectedContentType(
            content_type.map(|_| "unsupported".to_owned()),
        ))
    }
}

fn validate_sse_content_type(
    content_type: Option<&HeaderValue>,
) -> Result<(), StreamableHttpError<BoundedHttpError>> {
    let Some(content_type) = content_type else {
        return Err(StreamableHttpError::UnexpectedContentType(None));
    };
    if content_type_matches(content_type.as_bytes(), EVENT_STREAM_MIME_TYPE)
        || content_type_matches(content_type.as_bytes(), JSON_MIME_TYPE)
    {
        Ok(())
    } else {
        Err(StreamableHttpError::UnexpectedContentType(Some(
            "unsupported".to_owned(),
        )))
    }
}

fn is_content_type(content_type: &Option<Vec<u8>>, expected: &str) -> bool {
    content_type
        .as_deref()
        .is_some_and(|value| content_type_matches(value, expected))
}

fn content_type_matches(value: &[u8], expected: &str) -> bool {
    let expected = expected.as_bytes();
    value == expected
        || value
            .strip_prefix(expected)
            .is_some_and(|suffix| suffix.first() == Some(&b';'))
}

fn serialize_bounded_json<T: serde::Serialize>(
    value: &T,
    maximum: usize,
) -> Result<Vec<u8>, serde_json::Error> {
    struct LimitedWriter {
        bytes: Vec<u8>,
        maximum: usize,
    }

    impl Write for LimitedWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len().saturating_add(buffer.len()) > self.maximum {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "serialized MCP HTTP message exceeded configured limit",
                ));
            }
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = LimitedWriter {
        bytes: Vec::with_capacity(maximum.min(4 * 1024)),
        maximum,
    };
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes)
}

fn bounded_header_value(
    value: &HeaderValue,
) -> Result<String, StreamableHttpError<BoundedHttpError>> {
    let bytes = value.as_bytes();
    if bytes.len() > MAX_AUTH_CHALLENGE_BYTES {
        return Err(StreamableHttpError::UnexpectedServerResponse(
            Cow::Borrowed("response header exceeded the configured limit"),
        ));
    }
    value.to_str().map(str::to_owned).map_err(|_| {
        StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed("invalid response header"))
    })
}

fn bounded_request_header_value(
    value: &str,
) -> Result<HeaderValue, StreamableHttpError<BoundedHttpError>> {
    if value.is_empty() || value.len() > MAX_HEADER_BYTES || value.chars().any(char::is_control) {
        return Err(StreamableHttpError::UnexpectedServerResponse(
            Cow::Borrowed("invalid bounded request header"),
        ));
    }
    HeaderValue::from_str(value).map_err(|_| {
        StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed(
            "invalid bounded request header",
        ))
    })
}

fn bounded_auth_challenge(
    value: &HeaderValue,
) -> Result<String, StreamableHttpError<BoundedHttpError>> {
    let challenge = bounded_header_value(value)?;
    if challenge.chars().any(char::is_control) {
        return Err(StreamableHttpError::UnexpectedServerResponse(
            Cow::Borrowed("invalid authorization challenge"),
        ));
    }
    Ok(challenge)
}

fn bounded_response_session_id(
    value: &HeaderValue,
) -> Result<String, StreamableHttpError<BoundedHttpError>> {
    let session_id = bounded_header_value(value)?;
    if session_id.is_empty() || session_id.chars().any(char::is_control) {
        return Err(StreamableHttpError::UnexpectedServerResponse(
            Cow::Borrowed("invalid response session identifier"),
        ));
    }
    Ok(session_id)
}

fn extract_scope(challenge: &str) -> Option<String> {
    let lower = challenge.to_ascii_lowercase();
    let start = lower.find("scope=")?.saturating_add("scope=".len());
    let rest = &challenge[start..];
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"')?;
        Some(quoted[..end].to_owned())
    } else {
        let end = rest
            .find(|character: char| {
                character == ',' || character == ';' || character.is_whitespace()
            })
            .unwrap_or(rest.len());
        (end > 0).then(|| rest[..end].to_owned())
    }
}

fn bounded_sse_stream<S>(
    stream: S,
    max_event_size: usize,
) -> BoxStream<'static, Result<Sse, SseError>>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let stream = stream.scan(
        (SseEventSizeLimiter::new(max_event_size), false),
        |(limiter, failed), item| {
            let output = if *failed {
                None
            } else {
                match item {
                    Ok(chunk) if limiter.observe(&chunk).is_ok() => Some(Ok(chunk)),
                    Ok(_) => {
                        *failed = true;
                        Some(Err(BoundedSseBodyError::EventTooLarge {
                            maximum: limiter.maximum,
                        }))
                    },
                    Err(_) => {
                        *failed = true;
                        Some(Err(BoundedSseBodyError::Source))
                    },
                }
            };
            future::ready(output)
        },
    );
    SseStream::from_bytes_stream(stream).boxed()
}

enum CollectBoundedError<E> {
    Source(E),
    TooLarge,
}

async fn collect_bounded_chunks<S, E>(
    stream: S,
    content_length: Option<u64>,
    maximum: usize,
) -> Result<Bytes, CollectBoundedError<E>>
where
    S: Stream<Item = Result<Bytes, E>>,
{
    if content_length.is_some_and(|length| length > maximum as u64) {
        return Err(CollectBoundedError::TooLarge);
    }
    let mut bytes =
        Vec::with_capacity(content_length.unwrap_or_default().min(maximum as u64) as usize);
    futures_util::pin_mut!(stream);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(CollectBoundedError::Source)?;
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(CollectBoundedError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(bytes))
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use futures_util::stream;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    #[tokio::test]
    async fn bounded_body_rejects_streamed_content_above_the_limit() {
        let source = stream::iter([
            Ok::<_, Infallible>(Bytes::from_static(b"1234")),
            Ok(Bytes::from_static(b"5678")),
        ]);
        assert!(matches!(
            collect_bounded_chunks(source, None, 7).await,
            Err(CollectBoundedError::TooLarge)
        ));
    }

    #[tokio::test]
    async fn bounded_body_rejects_oversized_content_length_before_streaming() {
        let source = stream::empty::<Result<Bytes, Infallible>>();
        assert!(matches!(
            collect_bounded_chunks(source, Some(8), 7).await,
            Err(CollectBoundedError::TooLarge)
        ));
    }

    #[test]
    fn outbound_http_json_is_bounded_during_serialization() {
        let message: ClientJsonRpcMessage = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "custom/oversized",
            "params": {"payload": "x".repeat(128)}
        }))
        .unwrap();
        assert!(serialize_bounded_json(&message, 64).is_err());
    }

    #[test]
    fn content_type_matching_rejects_prefix_confusion() {
        assert!(content_type_matches(
            b"application/json; charset=utf-8",
            JSON_MIME_TYPE
        ));
        assert!(!content_type_matches(b"application/jsonp", JSON_MIME_TYPE));
        assert!(!content_type_matches(
            b"text/event-streaming",
            EVENT_STREAM_MIME_TYPE
        ));
    }

    #[tokio::test]
    async fn streamable_http_rejects_oversized_json_before_decoding() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4_096];
            let _ = socket.read(&mut request).await.unwrap();
            let response = [
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n"
                    .as_slice(),
                &[b'x'; 100],
            ]
            .concat();
            socket.write_all(&response).await.unwrap();
        });
        let client = BoundedHttpClient::new(None, 64, 64).unwrap();
        let message: ClientJsonRpcMessage = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .unwrap();
        let error = client
            .post_message(
                Arc::<str>::from(format!("http://{address}/mcp")),
                message,
                None,
                None,
                HashMap::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StreamableHttpError::Client(BoundedHttpError::BodyTooLarge { maximum: 64 })
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_sse_event_fails_before_sse_payload_is_retained() {
        let source = stream::iter([Ok::<_, reqwest::Error>(Bytes::from_static(
            b"data: 123456789\n\n",
        ))]);
        let mut events = bounded_sse_stream(source, 8);
        assert!(events.next().await.unwrap().is_err());
        assert!(events.next().await.is_none());
    }

    #[test]
    fn scope_extraction_is_bounded_and_preserves_quoted_scopes() {
        assert_eq!(
            extract_scope(r#"Bearer error="insufficient_scope", scope="mail.read calendar.read""#),
            Some("mail.read calendar.read".to_owned())
        );
    }
}
