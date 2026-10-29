// Keep these expressions in sync with the connector fixture.
use regex::Regex;
use std::sync::LazyLock;
static REDACT: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
    Regex::new(r###"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)"###).expect("fixed redaction expression"),
    Regex::new(r###"(?i)\b(?:authorization[\x22\x27]?\s*[:=]\s*|bearer\s+)[^\r\n,;]+"###).expect("fixed redaction expression"),
    Regex::new(r###"(?i)\b(?:[a-z_]*(?:token|password|passwd|secret|api_key|apikey)[a-z_]*|cloudflare_api_key)[\x22\x27]?\s*[=:]\s*(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s&;,"'}]+)"###).expect("fixed redaction expression"),
    Regex::new(r###"[a-zA-Z][a-zA-Z0-9+.-]*://[^\s/@]+@"###).expect("fixed redaction expression"),
    Regex::new(r###"\b(?:gh[pousr]_[A-Za-z0-9_]+|github_pat_[A-Za-z0-9_]+|cfut_[A-Za-z0-9_-]+|tskey-[A-Za-z0-9_-]+|(?:eyJ[A-Za-z0-9_-]*|[A-Za-z0-9_-]{10,})\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+)"###).expect("fixed redaction expression"),
    Regex::new(r###"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"###).expect("fixed redaction expression"),
]
});
static NORMALIZE: LazyLock<Vec<(Regex, &str)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(
                r###"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b"###,
            )
            .expect("fixed normalization expression"),
            "<uuid>",
        ),
        (
            Regex::new(r###"(?:[A-Za-z]:\\|/)[^\s\x22\x27,;]+"###)
                .expect("fixed normalization expression"),
            "<path>",
        ),
        (
            Regex::new(r###"(?i)\b(?:0x[0-9a-f]+|[0-9a-f]{8,})\b"###)
                .expect("fixed normalization expression"),
            "<hex>",
        ),
        (
            Regex::new(r###":\d+\b"###).expect("fixed normalization expression"),
            ":<port>",
        ),
        (
            Regex::new(r###"\b\d+\b"###).expect("fixed normalization expression"),
            "<n>",
        ),
    ]
});
pub fn redact(msg: &str) -> String {
    REDACT.iter().fold(msg.to_owned(), |s, re| {
        re.replace_all(&s, "[REDACTED]").into_owned()
    })
}
pub fn normalize(msg: &str) -> String {
    NORMALIZE
        .iter()
        .fold(msg.to_owned(), |s, (re, replacement)| {
            re.replace_all(&s, *replacement).into_owned()
        })
}
pub fn fingerprint(component: &str, code: &str, msg: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(format!("{component}|{code}|{}", normalize(msg)))
    )[..16]
        .to_owned()
}
