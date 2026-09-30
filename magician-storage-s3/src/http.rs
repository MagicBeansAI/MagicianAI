use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use zeroize::Zeroizing;

use magician_storage::object::{ByteRange, DeleteCondition, ObjectVersion, PutCondition};
use magician_storage::StorageError;

use crate::backend::{digest_of, BlobMeta, BlobStore};
use crate::sigv4::{hex_sha256, sign_s3_request, SignInput};

pub struct S3HttpBlobStore {
    client: reqwest::Client,
    endpoint: String,
    bucket: String,
    region: String,
    access_key: String,
    secret_key: Zeroizing<String>,
    path_style: bool,
    sse_required: bool,
    abandoned: Mutex<Vec<(String, std::time::Instant)>>,
}

impl std::fmt::Debug for S3HttpBlobStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3HttpBlobStore")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("access_key", &"redacted")
            .field("secret_key", &"redacted")
            .field("path_style", &self.path_style)
            .field("sse_required", &self.sse_required)
            .finish()
    }
}

pub struct S3HttpSettings {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub path_style: bool,
    pub sse_required: bool,
    pub allow_http: bool,
    pub timeout: Duration,
}

impl S3HttpBlobStore {
    pub fn new(settings: S3HttpSettings) -> Result<Self, StorageError> {
        let S3HttpSettings {
            endpoint,
            bucket,
            region,
            access_key,
            secret_key,
            path_style,
            sse_required,
            allow_http,
            timeout,
        } = settings;
        let parsed = reqwest::Url::parse(endpoint.trim())
            .map_err(|_| StorageError::invalid_key("s3 endpoint"))?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(StorageError::PermissionDenied);
        }
        if !allow_http && parsed.scheme() != "https" {
            return Err(StorageError::PermissionDenied);
        }
        if !sse_required {
            return Err(StorageError::UnsupportedCapability);
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .use_rustls_tls()
            .https_only(!allow_http)
            .build()
            .map_err(|err| StorageError::backend(err.to_string()))?;
        Ok(Self {
            client,
            endpoint: endpoint.trim().trim_end_matches('/').to_string(),
            bucket,
            region,
            access_key,
            secret_key: Zeroizing::new(secret_key),
            path_style,
            sse_required,
            abandoned: Mutex::new(Vec::new()),
        })
    }

    fn object_url(&self, key: &str) -> String {
        if self.path_style {
            format!("{}/{}/{}", self.endpoint, self.bucket, key)
        } else {
            let host = self
                .endpoint
                .replacen("https://", "", 1)
                .replacen("http://", "", 1);
            let scheme = if self.endpoint.starts_with("http://") {
                "http"
            } else {
                "https"
            };
            format!("{scheme}://{}.{host}/{key}", self.bucket)
        }
    }

    fn host(&self) -> String {
        let trimmed = self
            .endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        if self.path_style {
            trimmed.to_string()
        } else {
            format!("{}.{}", self.bucket, trimmed)
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        query: &str,
        body: Bytes,
        extra: Vec<(String, String)>,
    ) -> Result<reqwest::Response, StorageError> {
        let url = if query.is_empty() {
            self.object_url(key)
        } else {
            format!("{}?{query}", self.object_url(key))
        };
        let path = if self.path_style {
            format!("/{}/{}", self.bucket, key)
        } else {
            format!("/{key}")
        };
        let payload_hash = hex_sha256(&body);
        let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let mut signed_extra = extra.clone();
        if self.sse_required {
            signed_extra.push(("x-amz-server-side-encryption".into(), "AES256".into()));
        }
        let auth = sign_s3_request(SignInput {
            method: method.as_str(),
            path: &path,
            query,
            host: &self.host(),
            payload_hash: &payload_hash,
            access_key: &self.access_key,
            secret_key: &self.secret_key,
            region: &self.region,
            amz_date: &amz_date,
            extra_amz_headers: &signed_extra,
        });
        let mut req = self
            .client
            .request(method, url)
            .header("authorization", auth)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", payload_hash)
            .header("host", self.host());
        if self.sse_required {
            req = req.header("x-amz-server-side-encryption", "AES256");
        }
        for (name, value) in extra {
            req = req.header(name, value);
        }
        if !body.is_empty() {
            req = req.body(body);
        }
        let built = req
            .build()
            .map_err(|err| StorageError::backend(err.to_string()))?;
        let mut last_timeout = false;
        for attempt in 0..3 {
            match self
                .client
                .execute(
                    built
                        .try_clone()
                        .ok_or_else(|| StorageError::backend("request clone"))?,
                )
                .await
            {
                Ok(resp) => return Ok(resp),
                Err(err) if err.is_timeout() || err.is_connect() => {
                    last_timeout = true;
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                },
                Err(err) => return Err(StorageError::backend(err.to_string())),
            }
        }
        if last_timeout {
            Err(StorageError::Timeout)
        } else {
            Err(StorageError::Unavailable {
                retry_after: Some(Duration::from_secs(1)),
            })
        }
    }
}

#[async_trait]
impl BlobStore for S3HttpBlobStore {
    async fn put(
        &self,
        key: &str,
        bytes: Bytes,
        condition: PutCondition,
    ) -> Result<BlobMeta, StorageError> {
        let digest = digest_of(&bytes);
        let mut extra = match condition {
            PutCondition::Overwrite => Vec::new(),
            PutCondition::CreateOnly => vec![("if-none-match".into(), "*".into())],
            PutCondition::ExpectedVersion(version) => {
                vec![("if-match".into(), quoted_etag(version.as_str()))]
            },
        };
        extra.push(("x-amz-meta-blake3".into(), digest.hex.clone()));
        extra.push(("x-amz-meta-len".into(), bytes.len().to_string()));
        let resp = self
            .send(reqwest::Method::PUT, key, "", bytes.clone(), extra)
            .await?;
        map_status(resp.status())?;
        Ok(BlobMeta {
            key: key.to_string(),
            len: bytes.len() as u64,
            digest,
            version: version_from_headers(&resp),
        })
    }

    async fn get(
        &self,
        key: &str,
        range: Option<ByteRange>,
    ) -> Result<(BlobMeta, Bytes), StorageError> {
        let extra = match range {
            Some(range) => {
                let end = range
                    .end_exclusive
                    .map(|end| (end.saturating_sub(1)).to_string())
                    .unwrap_or_default();
                let value = if end.is_empty() {
                    format!("bytes={}-", range.start)
                } else {
                    format!("bytes={}-{}", range.start, end)
                };
                vec![("range".into(), value)]
            },
            None => Vec::new(),
        };
        let resp = self
            .send(reqwest::Method::GET, key, "", Bytes::new(), extra)
            .await?;
        map_status(resp.status())?;
        let stored_digest = meta_digest(&resp).ok_or(StorageError::Integrity {
            expected: "stored-digest".into(),
            actual: "missing".into(),
        })?;
        let stored_len = meta_len(&resp).ok_or(StorageError::Integrity {
            expected: "stored-len".into(),
            actual: "missing".into(),
        })?;
        let version = version_from_headers(&resp);
        let bytes = resp
            .bytes()
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        if range.is_none() {
            let actual = digest_of(&bytes);
            if actual != stored_digest || bytes.len() as u64 != stored_len {
                return Err(StorageError::Integrity {
                    expected: format!("{}:{stored_len}", stored_digest.hex),
                    actual: format!("{}:{}", actual.hex, bytes.len()),
                });
            }
        }
        Ok((
            BlobMeta {
                key: key.to_string(),
                len: stored_len,
                digest: stored_digest,
                version,
            },
            bytes,
        ))
    }

    async fn head(&self, key: &str) -> Result<Option<BlobMeta>, StorageError> {
        let resp = self
            .send(reqwest::Method::HEAD, key, "", Bytes::new(), Vec::new())
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        map_status(resp.status())?;
        let len = meta_len(&resp).ok_or(StorageError::Integrity {
            expected: "stored-len".into(),
            actual: "missing".into(),
        })?;
        if let Some(content_len) = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            if content_len != len {
                return Err(StorageError::Integrity {
                    expected: len.to_string(),
                    actual: content_len.to_string(),
                });
            }
        }
        let digest = meta_digest(&resp).ok_or(StorageError::Integrity {
            expected: "stored-digest".into(),
            actual: "missing".into(),
        })?;
        Ok(Some(BlobMeta {
            key: key.to_string(),
            len,
            digest,
            version: version_from_headers(&resp),
        }))
    }

    async fn delete(
        &self,
        key: &str,
        condition: DeleteCondition,
    ) -> Result<ObjectVersion, StorageError> {
        let extra = match condition {
            DeleteCondition::Existing => Vec::new(),
            DeleteCondition::ExpectedVersion(version) => {
                vec![("if-match".into(), quoted_etag(version.as_str()))]
            },
        };
        let resp = self
            .send(reqwest::Method::DELETE, key, "", Bytes::new(), extra)
            .await?;
        map_status(resp.status())?;
        Ok(version_from_headers(&resp))
    }

    async fn list(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Vec<BlobMeta>, StorageError> {
        let mut query = format!("list-type=2&prefix={prefix}&max-keys={limit}");
        if let Some(cursor) = cursor {
            query.push_str("&start-after=");
            query.push_str(cursor);
        }
        let resp = self
            .send(reqwest::Method::GET, "", &query, Bytes::new(), Vec::new())
            .await?;
        map_status(resp.status())?;
        let body = resp
            .text()
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        Ok(parse_list_keys(&body)
            .into_iter()
            .map(|key| BlobMeta {
                key,
                len: 0,
                digest: digest_of(&[]),
                version: ObjectVersion::unversioned(),
            })
            .collect())
    }

    async fn create_multipart(&self, key: &str) -> Result<String, StorageError> {
        let resp = self
            .send(
                reqwest::Method::POST,
                key,
                "uploads=",
                Bytes::new(),
                Vec::new(),
            )
            .await?;
        map_status(resp.status())?;
        let body = resp
            .text()
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        let id = xml_tag(&body, "UploadId").ok_or_else(|| StorageError::backend("upload id"))?;
        self.abandoned
            .lock()
            .expect("abandoned")
            .push((id.clone(), std::time::Instant::now()));
        Ok(id)
    }

    async fn upload_part(
        &self,
        upload_id: &str,
        part: i32,
        bytes: Bytes,
    ) -> Result<(), StorageError> {
        let query = format!("partNumber={part}&uploadId={upload_id}");
        let resp = self
            .send(reqwest::Method::PUT, "", &query, bytes, Vec::new())
            .await?;
        map_status(resp.status())?;
        Ok(())
    }

    async fn complete_multipart(
        &self,
        upload_id: &str,
        _condition: PutCondition,
    ) -> Result<BlobMeta, StorageError> {
        let query = format!("uploadId={upload_id}");
        let resp = self
            .send(
                reqwest::Method::POST,
                "",
                &query,
                Bytes::from_static(b"<CompleteMultipartUpload/>"),
                Vec::new(),
            )
            .await?;
        map_status(resp.status())?;
        self.abandoned
            .lock()
            .expect("abandoned")
            .retain(|(id, _)| id != upload_id);
        Ok(BlobMeta {
            key: String::new(),
            len: 0,
            digest: digest_of(&[]),
            version: ObjectVersion::unversioned(),
        })
    }

    async fn abort_multipart(&self, upload_id: &str) -> Result<(), StorageError> {
        let query = format!("uploadId={upload_id}");
        let resp = self
            .send(
                reqwest::Method::DELETE,
                "",
                &query,
                Bytes::new(),
                Vec::new(),
            )
            .await?;
        map_status(resp.status())?;
        self.abandoned
            .lock()
            .expect("abandoned")
            .retain(|(id, _)| id != upload_id);
        Ok(())
    }

    async fn cleanup_abandoned(&self, older_than: Duration) -> Result<u64, StorageError> {
        let cutoff = std::time::Instant::now() - older_than;
        let stale: Vec<String> = self
            .abandoned
            .lock()
            .expect("abandoned")
            .iter()
            .filter(|(_, at)| *at <= cutoff)
            .map(|(id, _)| id.clone())
            .collect();
        let count = stale.len() as u64;
        for id in &stale {
            let _ = self.abort_multipart(id).await;
        }
        Ok(count)
    }
}

fn map_status(status: reqwest::StatusCode) -> Result<(), StorageError> {
    match status.as_u16() {
        200 | 204 | 206 => Ok(()),
        404 => Err(StorageError::NotFound),
        412 | 409 => Err(StorageError::Conflict {
            expected: None,
            actual: None,
        }),
        403 => Err(StorageError::PermissionDenied),
        _ => Err(StorageError::backend(format!("s3 status {status}"))),
    }
}

fn quoted_etag(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"');
    format!("\"{trimmed}\"")
}

fn version_from_headers(resp: &reqwest::Response) -> ObjectVersion {
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty());
    match etag {
        Some(etag) => ObjectVersion::new(etag).unwrap_or_else(|_| ObjectVersion::unversioned()),
        None => ObjectVersion::unversioned(),
    }
}

fn meta_digest(resp: &reqwest::Response) -> Option<magician_storage::ContentDigest> {
    let hex = resp
        .headers()
        .get("x-amz-meta-blake3")
        .and_then(|v| v.to_str().ok())?;
    magician_storage::ContentDigest::parse(magician_storage::DigestAlgorithm::Blake3, hex).ok()
}

fn meta_len(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get("x-amz-meta-len")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

fn parse_list_keys(xml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Key>") {
        rest = &rest[start + 5..];
        if let Some(end) = rest.find("</Key>") {
            keys.push(rest[..end].to_string());
            rest = &rest[end + 6..];
        } else {
            break;
        }
    }
    keys
}

fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}
