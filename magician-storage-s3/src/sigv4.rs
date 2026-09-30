use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub struct SignInput<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    pub host: &'a str,
    pub payload_hash: &'a str,
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub region: &'a str,
    pub amz_date: &'a str,
    pub extra_amz_headers: &'a [(String, String)],
}

pub fn hex_sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn sign_s3_request(input: SignInput<'_>) -> String {
    let date_stamp = &input.amz_date[..8];
    let mut headers: Vec<(String, String)> = vec![
        ("host".into(), input.host.to_string()),
        (
            "x-amz-content-sha256".into(),
            input.payload_hash.to_string(),
        ),
        ("x-amz-date".into(), input.amz_date.to_string()),
    ];
    for (name, value) in input.extra_amz_headers {
        headers.push((name.to_ascii_lowercase(), value.clone()));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    let mut canonical_headers = String::new();
    let mut signed_names = Vec::new();
    for (name, value) in &headers {
        canonical_headers.push_str(name);
        canonical_headers.push(':');
        canonical_headers.push_str(value.trim());
        canonical_headers.push('\n');
        signed_names.push(name.clone());
    }
    let signed_headers = signed_names.join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        input.method,
        input.path,
        input.query,
        canonical_headers,
        signed_headers,
        input.payload_hash
    );
    let canonical_hash = hex_sha256(canonical.as_bytes());
    let scope = format!("{date_stamp}/{}/s3/aws4_request", input.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        input.amz_date, scope, canonical_hash
    );
    let signing_key = aws4_key(input.secret_key, date_stamp, input.region);
    let signature = hex::encode(hmac(&signing_key, string_to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        input.access_key
    )
}

fn aws4_key(secret: &str, date: &str, region: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, b"s3");
    hmac(&k_service, b"aws4_request")
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}
