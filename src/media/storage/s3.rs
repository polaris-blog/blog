//! S3-compatible object storage provider — AWS S3, Cloudflare R2, MinIO,
//! Backblaze B2, … anything speaking the S3 API.
//!
//! SigV4 signing is implemented in-tree over the project's blocking HTTP
//! client (ureq + rustls): the static musl build stays dependency-free
//! (no aws-sdk / aws-lc-rs). Requests run under `block_in_place` so the
//! async runtime keeps scheduling while a transfer is in flight.
//!
//! Path-style addressing (`{endpoint}/{bucket}/{key}`) is used throughout:
//! it is what R2 and MinIO expect, and AWS still supports it.
//!
//! Object bodies are read fully into memory for small-object operations
//! (metadata, thumbnails, serving). Serving large files through the origin
//! is therefore not the intended hot path — put a CDN in front
//! (`public_base_url` or `media.cdn`).

use std::io::Read;
use std::time::Duration;

use quick_xml::Reader;
use quick_xml::events::Event;

use super::{StoredObject, validate_key};
use crate::config::StorageEntry;
use crate::error::{AppError, AppResult};
use crate::plugins::crypto::{hmac_sha256, sha256_bytes, sha256_hex};

/// Request timeout for every object-store call.
const TIMEOUT_SECS: u64 = 30;

#[derive(Debug)]
pub struct S3Storage {
    id: String,
    endpoint: String,
    region: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
    agent: ureq::Agent,
}

impl S3Storage {
    pub fn new(entry: &StorageEntry) -> AppResult<Self> {
        for (field, value) in [
            ("endpoint", entry.endpoint.as_str()),
            ("bucket", entry.bucket.as_str()),
            ("access_key_id", entry.access_key_id.as_str()),
            ("secret_access_key", entry.secret_access_key.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::BadRequest(format!(
                    "s3 storage '{}' requires `{}`",
                    entry.id, field
                )));
            }
        }
        let region = if entry.region.trim().is_empty() {
            "us-east-1".to_string()
        } else {
            entry.region.trim().to_string()
        };
        Ok(Self {
            id: entry.id.clone(),
            endpoint: entry.endpoint.trim().trim_end_matches('/').to_string(),
            region,
            bucket: entry.bucket.trim().to_string(),
            access_key_id: entry.access_key_id.trim().to_string(),
            secret_access_key: entry.secret_access_key.trim().to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(TIMEOUT_SECS))
                .build(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Path-style object URL: `{endpoint}/{bucket}/{key}` with every key
    /// segment percent-encoded.
    fn object_url(&self, key: &str) -> String {
        let encoded: Vec<String> = key.split('/').map(uri_encode_segment).collect();
        format!("{}/{}/{}", self.endpoint, self.bucket, encoded.join("/"))
    }

    fn key_url(&self, key: &str) -> String {
        self.object_url(key)
    }

    // -- request plumbing ----------------------------------------------------

    /// Execute one signed request. `extra_headers` are signed and sent
    /// verbatim (e.g. `x-amz-copy-source`, `range`).
    fn request(
        &self,
        method: &str,
        key: &str,
        query: &[(&str, &str)],
        extra_headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> AppResult<ureq::Response> {
        validate_key(key)?;
        let payload_hash = sha256_bytes(&body.map(|b| b.to_vec()).unwrap_or_default());
        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let host = self
            .endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_string();
        let encoded_key: Vec<String> = key.split('/').map(uri_encode_segment).collect();
        let canonical_uri = format!("/{}/{}", self.bucket, encoded_key.join("/"));
        let canonical_query = canonical_query_string(query);

        let mut signed: Vec<(String, String)> = vec![
            ("host".to_string(), host),
            ("x-amz-content-sha256".to_string(), payload_hash),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        for (k, v) in extra_headers {
            signed.push((k.to_lowercase(), v.to_string()));
        }
        signed.sort();

        let authorization = sign_v4(&SigV4Params {
            method,
            uri: &canonical_uri,
            query: &canonical_query,
            signed_headers: &signed,
            payload_hash: signed
                .iter()
                .find(|(k, _)| k == "x-amz-content-sha256")
                .map(|(_, v)| v.as_str())
                .unwrap_or_default(),
            access_key: &self.access_key_id,
            secret_key: &self.secret_access_key,
            region: &self.region,
            amz_date: &amz_date,
            date_stamp: &date_stamp,
        });

        let mut url = self.key_url(key);
        if !canonical_query.is_empty() {
            url = format!("{url}?{canonical_query}");
        }

        run_blocking(move || {
            let mut req = self
                .agent
                .request(method, &url)
                .set("Authorization", &authorization)
                .set("x-amz-date", &amz_date)
                .set("x-amz-content-sha256", &signed_hash(&signed));
            for (k, v) in extra_headers {
                req = req.set(k, v);
            }
            let outcome = match body {
                Some(b) if !b.is_empty() => req.send_bytes(b),
                _ => req.call(),
            };
            match outcome {
                Ok(resp) => Ok(resp),
                Err(ureq::Error::Status(_, resp)) => Ok(resp),
                Err(ureq::Error::Transport(t)) => Err(AppError::Internal(anyhow::anyhow!(
                    "s3 request failed: {t}"
                ))),
            }
        })
    }

    fn status_response(status: u16, body: &str) -> AppError {
        AppError::Internal(anyhow::anyhow!(
            "s3 responded {status}: {}",
            &body[..body.len().min(200)]
        ))
    }

    // -- Storage API ---------------------------------------------------------

    pub async fn put(&self, key: &str, data: &[u8]) -> AppResult<()> {
        let resp = self.request("PUT", key, &[], &[], Some(data))?;
        if resp.status() >= 300 {
            return Err(Self::status_response(resp.status(), &read_error_body(resp)));
        }
        Ok(())
    }

    pub async fn put_staged(&self, key: &str, staged: &std::path::Path) -> AppResult<()> {
        let data = std::fs::read(staged)?;
        self.put(key, &data).await
    }

    pub async fn read(&self, key: &str) -> AppResult<Option<Vec<u8>>> {
        let resp = self.request("GET", key, &[], &[], None)?;
        match resp.status() {
            200 | 206 => Ok(Some(read_bytes(resp))),
            404 => Ok(None),
            status => Err(Self::status_response(status, &read_error_body(resp))),
        }
    }

    pub async fn open_reader(
        &self,
        key: &str,
    ) -> AppResult<Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>>> {
        Ok(self.read(key).await?.map(|bytes| {
            Box::new(std::io::Cursor::new(bytes)) as Box<dyn tokio::io::AsyncRead + Send + Unpin>
        }))
    }

    pub async fn open(&self, key: &str) -> AppResult<Option<StoredObject>> {
        let Some(bytes) = self.read(key).await? else {
            return Ok(None);
        };
        Ok(Some(StoredObject {
            size: bytes.len() as u64,
            body: axum::body::Body::from(bytes),
        }))
    }

    pub async fn open_range(
        &self,
        key: &str,
        start: u64,
        end: u64,
    ) -> AppResult<Option<StoredObject>> {
        let header = format!("bytes={start}-{}", end.saturating_sub(1).max(start));
        let resp = self.request("GET", key, &[], &[("range", header.as_str())], None)?;
        match resp.status() {
            200 | 206 => {
                let bytes = read_bytes(resp);
                Ok(Some(StoredObject {
                    size: bytes.len() as u64,
                    body: axum::body::Body::from(bytes),
                }))
            }
            404 => Ok(None),
            status => Err(Self::status_response(status, &read_error_body(resp))),
        }
    }

    pub async fn hash_of(&self, key: &str) -> AppResult<Option<String>> {
        let Some(bytes) = self.read(key).await? else {
            return Ok(None);
        };
        Ok(Some(sha256_bytes(&bytes)))
    }

    pub async fn size_of(&self, key: &str) -> AppResult<Option<u64>> {
        let resp = self.request("HEAD", key, &[], &[], None)?;
        match resp.status() {
            200 => Ok(resp.header("content-length").and_then(|v| v.parse().ok())),
            404 => Ok(None),
            status => Err(Self::status_response(status, "")),
        }
    }

    pub async fn copy(&self, from: &str, to: &str) -> AppResult<()> {
        let source = format!("/{}/{}", self.bucket, from);
        let resp = self.request(
            "PUT",
            to,
            &[],
            &[("x-amz-copy-source", source.as_str())],
            None,
        )?;
        if resp.status() >= 300 {
            return Err(Self::status_response(resp.status(), &read_error_body(resp)));
        }
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> AppResult<()> {
        let resp = self.request("DELETE", key, &[], &[], None)?;
        match resp.status() {
            200 | 204 | 404 => Ok(()),
            status => Err(Self::status_response(status, &read_error_body(resp))),
        }
    }

    /// List object keys under a prefix (ListObjectsV2, paginated). Keys are
    /// decoded from XML; the `.tmp` staging convention never appears in an
    /// object store.
    pub async fn list(&self, prefix: &str) -> AppResult<Vec<String>> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query: Vec<(&str, String)> = vec![
                ("list-type", "2".to_string()),
                ("prefix", prefix.to_string()),
            ];
            if let Some(t) = &token {
                query.push(("continuation-token", t.clone()));
            }
            let query_refs: Vec<(&str, &str)> =
                query.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let resp = self.request("GET", "", &query_refs, &[], None)?;
            if resp.status() != 200 {
                return Err(Self::status_response(resp.status(), &read_error_body(resp)));
            }
            let body = read_bytes(resp);
            let body = String::from_utf8_lossy(&body).into_owned();
            let (page_keys, truncated, next) = parse_list_bucket(&body)?;
            keys.extend(page_keys);
            if truncated {
                token = next;
                if token.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        keys.sort();
        Ok(keys)
    }
}

/// Read an object body fully (raw bytes — never lossy-UTF-8).
fn read_bytes(resp: ureq::Response) -> Vec<u8> {
    let mut reader = resp.into_reader().take(1024 * 1024 * 1024);
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).ok();
    buf
}

/// Read an error response body for diagnostics (bounded, lossy).
fn read_error_body(resp: ureq::Response) -> String {
    let mut reader = resp.into_reader().take(1024 * 1024);
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).ok();
    String::from_utf8_lossy(&buf).into_owned()
}

/// Payload hash of the signed request (recomputed from the signed headers
/// so the request and the signature can never disagree).
fn signed_hash(signed: &[(String, String)]) -> String {
    signed
        .iter()
        .find(|(k, _)| k == "x-amz-content-sha256")
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

fn uri_encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Canonical query string: sorted, percent-encoded `k=v` pairs.
fn canonical_query_string(query: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode_segment(k), uri_encode_segment(v)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Inputs of one SigV4 signature (kept a plain struct so the signing logic
/// is unit-testable against the published AWS test vector).
struct SigV4Params<'a> {
    method: &'a str,
    uri: &'a str,
    query: &'a str,
    /// Sorted (lowercase name, value) pairs — always includes host,
    /// x-amz-content-sha256 and x-amz-date.
    signed_headers: &'a [(String, String)],
    payload_hash: &'a str,
    access_key: &'a str,
    secret_key: &'a str,
    region: &'a str,
    amz_date: &'a str,
    date_stamp: &'a str,
}

/// Full `Authorization` header value per AWS SigV4.
fn sign_v4(p: &SigV4Params) -> String {
    let signed_names: Vec<&str> = p.signed_headers.iter().map(|(k, _)| k.as_str()).collect();
    let signed_headers = signed_names.join(";");
    let canonical_headers: String = p
        .signed_headers
        .iter()
        .map(|(k, v)| format!("{k}:{v}\n"))
        .collect();
    let canonical_request = format!(
        "{method}\n{uri}\n{query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        method = p.method,
        uri = p.uri,
        query = p.query,
        canonical_headers = canonical_headers,
        signed_headers = signed_headers,
        payload_hash = p.payload_hash,
    );
    let scope = format!(
        "{date}/{region}/s3/aws4_request",
        date = p.date_stamp,
        region = p.region
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{request_hash}",
        amz_date = p.amz_date,
        scope = scope,
        request_hash = sha256_hex(&canonical_request),
    );
    let mut key = b"AWS4".to_vec();
    key.extend_from_slice(p.secret_key.as_bytes());
    let date_key = hmac_sha256(&key, p.date_stamp);
    let region_key = hmac_sha256(&date_key, p.region);
    let service_key = hmac_sha256(&region_key, "s3");
    let signing_key = hmac_sha256(&service_key, "aws4_request");
    let signature = hmac_sha256(&signing_key, &string_to_sign);
    let signature_hex: String = signature.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "AWS4-HMAC-SHA256 Credential={key}/{scope}, SignedHeaders={signed_headers}, Signature={signature_hex}",
        key = p.access_key,
        scope = scope,
        signed_headers = signed_headers,
        signature_hex = signature_hex,
    )
}

/// Parse a ListBucketResult page: (keys, truncated, next token).
fn parse_list_bucket(body: &str) -> AppResult<(Vec<String>, bool, Option<String>)> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut keys = Vec::new();
    let mut truncated = false;
    let mut next: Option<String> = None;
    let mut in_key = false;
    let mut in_truncated = false;
    let mut in_token = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"Key" => in_key = true,
                b"IsTruncated" => in_truncated = true,
                b"NextContinuationToken" => in_token = true,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                let text = t
                    .unescape()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("s3 list xml: {e}")))?;
                if in_key {
                    keys.push(text.to_string());
                } else if in_truncated {
                    truncated = text == "true";
                } else if in_token {
                    next = Some(text.to_string());
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"Key" => in_key = false,
                b"IsTruncated" => in_truncated = false,
                b"NextContinuationToken" => in_token = false,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(AppError::Internal(anyhow::anyhow!("s3 list xml: {e}"))),
            _ => {}
        }
    }
    Ok((keys, truncated, next))
}

/// Run a blocking object-store call without stalling the async runtime
/// (same pattern as the plugin HTTP API).
fn run_blocking<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> StorageEntry {
        StorageEntry {
            id: "r2".into(),
            r#type: "s3".into(),
            dir: String::new(),
            endpoint: "https://example.r2.cloudflarestorage.com".into(),
            region: "auto".into(),
            bucket: "media".into(),
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            public_base_url: String::new(),
        }
    }

    #[test]
    fn new_requires_credentials() {
        let mut e = entry();
        e.bucket = String::new();
        assert!(S3Storage::new(&e).is_err());
    }

    #[test]
    fn object_url_is_path_style_and_encoded() {
        let s = S3Storage::new(&entry()).unwrap();
        assert_eq!(
            s.object_url("2026/08/a b+c.webp"),
            "https://example.r2.cloudflarestorage.com/media/2026/08/a%20b%2Bc.webp"
        );
    }

    /// AWS documentation test vector for SigV4 ("Example GET Object" —
    /// Signature Calculations for the Authorization Header). If this
    /// signature matches, the canonical request, string-to-sign and
    /// signing-key chain are all correct.
    #[test]
    fn sigv4_matches_aws_test_vector() {
        let payload_hash = sha256_hex("");
        let signed: Vec<(String, String)> = vec![
            (
                "host".to_string(),
                "examplebucket.s3.amazonaws.com".to_string(),
            ),
            ("range".to_string(), "bytes=0-9".to_string()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
        ];
        let auth = sign_v4(&SigV4Params {
            method: "GET",
            uri: "/test.txt",
            query: "",
            signed_headers: &signed,
            payload_hash: &payload_hash,
            access_key: "AKIAIOSFODNN7EXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            amz_date: "20130524T000000Z",
            date_stamp: "20130524",
        });
        assert!(
            auth.ends_with(
                "Signature=67fe34c8530db585abddc51067328adfedb6e42487d2566dc7d927d6e2722900"
            ),
            "{auth}"
        );
        assert!(auth.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
    }

    #[test]
    fn canonical_query_sorts_and_encodes() {
        assert_eq!(
            canonical_query_string(&[("list-type", "2"), ("prefix", "2026/08/a b")]),
            "list-type=2&prefix=2026%2F08%2Fa%20b"
        );
    }

    #[test]
    fn list_bucket_parses_keys_and_pagination() {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>media</Name>
  <IsTruncated>true</IsTruncated>
  <NextContinuationToken>abc123</NextContinuationToken>
  <Contents><Key>2026/08/a.webp</Key><Size>10</Size></Contents>
  <Contents><Key>2026/08/b.jpg</Key><Size>20</Size></Contents>
</ListBucketResult>"#;
        let (keys, truncated, next) = parse_list_bucket(body).unwrap();
        assert_eq!(
            keys,
            vec!["2026/08/a.webp".to_string(), "2026/08/b.jpg".to_string()]
        );
        assert!(truncated);
        assert_eq!(next.as_deref(), Some("abc123"));

        let last = r#"<ListBucketResult><IsTruncated>false</IsTruncated>
  <Contents><Key>x</Key></Contents></ListBucketResult>"#;
        let (keys, truncated, next) = parse_list_bucket(last).unwrap();
        assert_eq!(keys, vec!["x".to_string()]);
        assert!(!truncated);
        assert!(next.is_none());
    }
}
