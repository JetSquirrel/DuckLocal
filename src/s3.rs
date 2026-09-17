//! S3 bucket/object listing over plain HTTPS with hand-rolled SigV4 signing.
//!
//! DuckDB's httpfs can read `s3://` files but cannot enumerate them, so the
//! sidebar talks to the S3 REST API directly (ListBuckets / ListObjectsV2).
//! All functions here are blocking; UI code must call them via `smol::unblock`.

use anyhow::{anyhow, Result};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::i18n::trf;

/// Entries returned per prefix level before the listing stops paginating.
const MAX_LIST_ENTRIES: usize = 1000;

/// In-memory S3 credentials. Lives only for the session, never written to disk.
#[derive(Clone, Debug)]
pub struct S3Config {
    pub endpoint: String,
    pub region: String,
    pub key_id: String,
    pub secret: String,
}

#[derive(Clone, Debug)]
pub struct S3Object {
    pub key: String,
    pub size: i64,
}

/// One level of a bucket listing: "subdirectories" plus the files directly
/// under the requested prefix.
#[derive(Clone, Debug, Default)]
pub struct S3Listing {
    pub prefixes: Vec<String>,
    pub objects: Vec<S3Object>,
}

pub fn list_buckets(config: &S3Config) -> Result<Vec<String>> {
    let body = s3_get(config, None, &[])?;
    let parsed: ListAllMyBucketsResult = quick_xml::de::from_str(&body)?;
    let mut names: Vec<String> = parsed
        .buckets
        .map(|b| b.bucket.into_iter().map(|entry| entry.name).collect())
        .unwrap_or_default();
    names.sort();
    Ok(names)
}

pub fn list_objects(config: &S3Config, bucket: &str, prefix: &str) -> Result<S3Listing> {
    let mut listing = S3Listing::default();
    let mut token: Option<String> = None;
    loop {
        let mut query = vec![
            ("list-type".to_string(), "2".to_string()),
            ("delimiter".to_string(), "/".to_string()),
            ("prefix".to_string(), prefix.to_string()),
            ("max-keys".to_string(), "1000".to_string()),
        ];
        if let Some(token) = &token {
            query.push(("continuation-token".to_string(), token.clone()));
        }
        let body = s3_get(config, Some(bucket), &query)?;
        let page: ListBucketResult = quick_xml::de::from_str(&body)?;
        listing
            .prefixes
            .extend(page.common_prefixes.into_iter().map(|p| p.prefix));
        listing.objects.extend(
            page.contents
                .into_iter()
                // A "folder marker" object duplicates its own prefix entry.
                .filter(|c| c.key != prefix)
                .map(|c| S3Object {
                    key: c.key,
                    size: c.size,
                }),
        );
        let done = !page.is_truncated
            || listing.prefixes.len() + listing.objects.len() >= MAX_LIST_ENTRIES;
        if done {
            break;
        }
        token = page.next_continuation_token;
        if token.is_none() {
            break;
        }
    }
    listing.prefixes.sort();
    Ok(listing)
}

struct Endpoint {
    scheme: &'static str,
    host: String,
    /// AWS S3 requires virtual-hosted style; S3-compatible services
    /// (MinIO, R2, OSS) commonly serve path style.
    virtual_hosted: bool,
}

fn parse_endpoint(raw: &str) -> Endpoint {
    let (scheme, rest) = match raw.split_once("://") {
        Some(("http", rest)) => ("http", rest),
        Some((_, rest)) => ("https", rest),
        None => ("https", raw),
    };
    let host = rest.trim_end_matches('/').to_string();
    let virtual_hosted = host.contains("amazonaws.com");
    Endpoint {
        scheme,
        host,
        virtual_hosted,
    }
}

/// Signed GET against the bucket (`None` = service-level ListBuckets) and
/// return the response body. S3 error responses carry the reason in XML.
///
/// A bucket outside the configured region answers 301/400 with its actual
/// region in the `x-amz-bucket-region` header (and `<Region>` in the body) —
/// retry once signed for that region.
fn s3_get(config: &S3Config, bucket: Option<&str>, query: &[(String, String)]) -> Result<String> {
    let mut region = config.region.clone();
    let mut retried = false;
    loop {
        match s3_get_once(config, &region, bucket, query)? {
            S3Response::Ok(body) => return Ok(body),
            S3Response::Failed {
                message,
                region_hint,
            } => {
                if !retried {
                    if let Some(actual) = region_hint.filter(|hint| *hint != region) {
                        region = actual;
                        retried = true;
                        continue;
                    }
                }
                return Err(anyhow!(message));
            }
        }
    }
}

enum S3Response {
    Ok(String),
    Failed {
        message: String,
        region_hint: Option<String>,
    },
}

fn s3_get_once(
    config: &S3Config,
    region: &str,
    bucket: Option<&str>,
    query: &[(String, String)],
) -> Result<S3Response> {
    let endpoint = parse_endpoint(&config.endpoint);
    let (host, path) = match bucket {
        Some(bucket) if endpoint.virtual_hosted => {
            (format!("{bucket}.{}", endpoint.host), "/".to_string())
        }
        Some(bucket) => (endpoint.host.clone(), format!("/{bucket}")),
        None => (endpoint.host.clone(), "/".to_string()),
    };
    let url = format!(
        "{}://{}{}{}",
        endpoint.scheme,
        host,
        path,
        encode_query(query)
    );
    let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let headers = vec![
        ("host".to_string(), host),
        ("x-amz-date".to_string(), amz_date.clone()),
    ];
    let auth = sigv4_authorization(
        "s3",
        region,
        &config.key_id,
        &config.secret,
        "GET",
        &path,
        query,
        &headers,
        &amz_date,
        "UNSIGNED-PAYLOAD",
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .get(&url)
        .header("x-amz-date", &amz_date)
        .header("x-amz-content-sha256", "UNSIGNED-PAYLOAD")
        .header("Authorization", &auth)
        .call()?;
    let status = response.status();
    let region_hint = response
        .headers()
        .get("x-amz-bucket-region")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response.body_mut().read_to_string()?;
    if status.is_success() {
        return Ok(S3Response::Ok(body));
    }
    let parsed = quick_xml::de::from_str::<ErrorResponse>(&body).ok();
    let reason = parsed
        .as_ref()
        .and_then(|e| e.message.clone())
        .unwrap_or_else(|| body.chars().take(200).collect());
    Ok(S3Response::Failed {
        message: trf(
            "error.s3.request_failed",
            &[&status.as_u16().to_string(), &reason],
        ),
        region_hint: region_hint.or_else(|| parsed.and_then(|e| e.region)),
    })
}

fn encode_query(query: &[(String, String)]) -> String {
    if query.is_empty() {
        return String::new();
    }
    let pairs: Vec<String> = query
        .iter()
        .map(|(k, v)| format!("{}={}", uri_encode(k), uri_encode(v)))
        .collect();
    format!("?{}", pairs.join("&"))
}

/// RFC 3986 unreserved characters pass through; everything else is
/// percent-encoded. Query values never need literal `/` preserved.
fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// AWS Signature Version 4, header-based auth. `headers` are the headers that
/// will be sent and signed (must include `host` and `x-amz-date`); they are
/// sorted internally. `payload_hash` is the hex SHA-256 of the body, or
/// `UNSIGNED-PAYLOAD` for S3 HTTPS requests.
#[allow(clippy::too_many_arguments)]
fn sigv4_authorization(
    service: &str,
    region: &str,
    key_id: &str,
    secret: &str,
    method: &str,
    path: &str,
    query: &[(String, String)],
    headers: &[(String, String)],
    amz_date: &str,
    payload_hash: &str,
) -> String {
    let date_stamp = &amz_date[..8];
    let scope = format!("{date_stamp}/{region}/{service}/aws4_request");

    let mut params: Vec<(String, String)> = query.to_vec();
    params.sort();
    let canonical_query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", uri_encode(k), uri_encode(v)))
        .collect();

    let mut signed: Vec<(String, String)> = headers.to_vec();
    signed.sort();
    let canonical_headers: String = signed.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers: Vec<&str> = signed.iter().map(|(k, _)| k.as_str()).collect();
    let signed_headers = signed_headers.join(";");
    let canonical_request = format!(
        "{method}\n{path}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        canonical_query.join("&")
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={key_id}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
    )
}

#[derive(serde::Deserialize)]
struct ListAllMyBucketsResult {
    #[serde(rename = "Buckets")]
    buckets: Option<Buckets>,
}

#[derive(serde::Deserialize)]
struct Buckets {
    #[serde(rename = "Bucket", default)]
    bucket: Vec<BucketEntry>,
}

#[derive(serde::Deserialize)]
struct BucketEntry {
    #[serde(rename = "Name")]
    name: String,
}

#[derive(serde::Deserialize)]
struct ListBucketResult {
    #[serde(rename = "CommonPrefixes", default)]
    common_prefixes: Vec<CommonPrefixes>,
    #[serde(rename = "Contents", default)]
    contents: Vec<Contents>,
    #[serde(rename = "IsTruncated", default)]
    is_truncated: bool,
    #[serde(rename = "NextContinuationToken")]
    next_continuation_token: Option<String>,
}

#[derive(serde::Deserialize)]
struct CommonPrefixes {
    #[serde(rename = "Prefix")]
    prefix: String,
}

#[derive(serde::Deserialize)]
struct Contents {
    #[serde(rename = "Key")]
    key: String,
    #[serde(rename = "Size")]
    size: i64,
}

#[derive(serde::Deserialize)]
struct ErrorResponse {
    #[serde(rename = "Message")]
    message: Option<String>,
    /// Bucket's actual region, present on 301 `PermanentRedirect` errors.
    #[serde(rename = "Region")]
    region: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> S3Config {
        S3Config {
            endpoint: "s3.amazonaws.com".to_string(),
            region: "us-east-1".to_string(),
            key_id: "AKIDEXAMPLE".to_string(),
            secret: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
        }
    }

    /// AWS's published SigV4 example (GET iam.amazonaws.com/?Action=ListUsers)
    /// pins the canonical-request/signature pipeline end to end.
    #[test]
    fn sigv4_matches_aws_documented_signature() {
        let query = vec![
            ("Action".to_string(), "ListUsers".to_string()),
            ("Version".to_string(), "2010-05-08".to_string()),
        ];
        let headers = vec![
            (
                "content-type".to_string(),
                "application/x-www-form-urlencoded; charset=utf-8".to_string(),
            ),
            ("host".to_string(), "iam.amazonaws.com".to_string()),
            ("x-amz-date".to_string(), "20150830T123600Z".to_string()),
        ];
        let auth = sigv4_authorization(
            "iam",
            "us-east-1",
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "GET",
            "/",
            &query,
            &headers,
            "20150830T123600Z",
            &sha256_hex(b""),
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
    }

    #[test]
    fn uri_encode_leaves_unreserved_and_encodes_rest() {
        assert_eq!(uri_encode("abc-DEF_019.~"), "abc-DEF_019.~");
        assert_eq!(uri_encode("a/b c+d"), "a%2Fb%20c%2Bd");
        assert_eq!(
            uri_encode("日志/文件.csv"),
            "%E6%97%A5%E5%BF%97%2F%E6%96%87%E4%BB%B6.csv"
        );
    }

    #[test]
    fn endpoint_parsing() {
        let aws = parse_endpoint("s3.amazonaws.com");
        assert_eq!(aws.scheme, "https");
        assert_eq!(aws.host, "s3.amazonaws.com");
        assert!(aws.virtual_hosted);

        let minio = parse_endpoint("http://localhost:9000/");
        assert_eq!(minio.scheme, "http");
        assert_eq!(minio.host, "localhost:9000");
        assert!(!minio.virtual_hosted);
    }

    #[test]
    fn encode_query_builds_sorted_agnostic_pairs() {
        let query = vec![
            ("list-type".to_string(), "2".to_string()),
            ("prefix".to_string(), "a/b".to_string()),
        ];
        assert_eq!(encode_query(&query), "?list-type=2&prefix=a%2Fb");
        assert_eq!(encode_query(&[]), "");
    }

    #[test]
    fn parses_list_buckets_response() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <ListAllMyBucketsResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
              <Buckets>
                <Bucket><Name>logs</Name><CreationDate>2024-01-01T00:00:00.000Z</CreationDate></Bucket>
                <Bucket><Name>data</Name><CreationDate>2024-01-02T00:00:00.000Z</CreationDate></Bucket>
              </Buckets>
            </ListAllMyBucketsResult>"#;
        let parsed: ListAllMyBucketsResult = quick_xml::de::from_str(xml).unwrap();
        let names: Vec<String> = parsed
            .buckets
            .unwrap()
            .bucket
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(names, vec!["logs", "data"]);
    }

    #[test]
    fn parses_list_objects_response() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
              <IsTruncated>false</IsTruncated>
              <Contents><Key>data/a.parquet</Key><Size>1024</Size></Contents>
              <Contents><Key>data/b.csv</Key><Size>20</Size></Contents>
              <CommonPrefixes><Prefix>data/raw/</Prefix></CommonPrefixes>
            </ListBucketResult>"#;
        let page: ListBucketResult = quick_xml::de::from_str(xml).unwrap();
        assert!(!page.is_truncated);
        assert_eq!(page.common_prefixes[0].prefix, "data/raw/");
        assert_eq!(page.contents.len(), 2);
        assert_eq!(page.contents[0].key, "data/a.parquet");
        assert_eq!(page.contents[0].size, 1024);
    }

    #[test]
    fn parses_error_response() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <Error><Code>SignatureDoesNotMatch</Code><Message>nope</Message></Error>"#;
        let parsed: ErrorResponse = quick_xml::de::from_str(xml).unwrap();
        assert_eq!(parsed.message.as_deref(), Some("nope"));
        assert_eq!(parsed.region, None);

        // 301 PermanentRedirect carries the bucket's actual region.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <Error><Code>PermanentRedirect</Code><Message>redirect</Message><Region>ap-east-1</Region></Error>"#;
        let parsed: ErrorResponse = quick_xml::de::from_str(xml).unwrap();
        assert_eq!(parsed.region.as_deref(), Some("ap-east-1"));
    }

    #[test]
    fn config_holds_credentials() {
        let config = test_config();
        assert_eq!(config.region, "us-east-1");
        assert!(!config.secret.is_empty());
    }
}
