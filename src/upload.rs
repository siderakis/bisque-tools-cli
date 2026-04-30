//! Resumable media upload module.
//!
//! Streams a local file directly to Google's resumable upload session in
//! 8 MB chunks. The proxy initiated the session and gave us back a
//! pre-authorized URI; we never need to talk to the proxy again until the
//! upload is done. See `design-docs/mcp-resumable-media-uploads.md`.
//!
//! Correctness/reliability features (NOT security gates — see design doc
//! Part "Match the existing trust model"):
//! - Validate the session URI host against a provider-supplied regex
//!   before any PUT, plus require https and reject embedded credentials.
//!   This defends against a misconfigured/compromised proxy redirecting
//!   bytes elsewhere; it is NOT meant to defend against a steered LLM.
//! - Trust the server's `Range:` response on `308`, not our own `end+1`.
//!   The server may have accepted less than we sent.
//! - On ambiguous transport failures (timeout / reset where the server
//!   may have committed bytes), probe with `Content-Range: bytes */total`
//!   before retrying. Skips chunks the server already has.
//! - Per-chunk retry with capped exponential backoff. Honors Retry-After
//!   in both delta-seconds and HTTP-date forms. Retries on 5xx, 429, 408.
//! - Maps known Google error reasons (quotaExceeded, invalidContentType,
//!   etc.) to structured client errors so the LLM gets actionable output.

use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use ureq::Response;

const DEFAULT_CHUNK_SIZE: u64 = 8 * 1024 * 1024;
const MAX_RETRIES: u32 = 5;
const BASE_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(16);

/// O_NOFOLLOW value on Linux/macOS. We open with this flag to refuse
/// following a symlink at the final path component, which closes the
/// stat-then-open TOCTOU race.
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = libc_compat::LINUX_O_NOFOLLOW;
#[cfg(target_os = "macos")]
const O_NOFOLLOW: i32 = libc_compat::MACOS_O_NOFOLLOW;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const O_NOFOLLOW: i32 = 0; // best-effort on other unix; Windows handled separately below.

/// Hardcoded constants so we don't pull in libc as a dep.
#[allow(dead_code)] // Only one of these two consts is referenced per platform.
mod libc_compat {
    pub const LINUX_O_NOFOLLOW: i32 = 0o400_000;
    pub const MACOS_O_NOFOLLOW: i32 = 0x0100;
}

/// Result of a successful upload: the final resource JSON returned by the
/// upstream API on the 200/201 that completed the session.
pub struct UploadOutcome {
    pub status: u16,
    pub resource: Value,
}

/// Open a file with `O_NOFOLLOW` (closes the stat-then-open race) and
/// return the file plus its metadata read from the descriptor.
pub fn safe_open(path: &Path) -> Result<(File, Metadata)> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    opts.custom_flags(O_NOFOLLOW);
    let file = opts
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let meta = file
        .metadata()
        .with_context(|| format!("failed to fstat {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok((file, meta))
}

/// Validate a session URI against the provider-supplied host pattern.
/// Rejects non-https, embedded user/password, and host mismatch.
pub fn validate_session_uri(uri: &str, host_pattern: &Regex) -> Result<()> {
    let parsed = url::Url::parse(uri).context("session URI is not a valid URL")?;
    if parsed.scheme() != "https" {
        bail!(
            "session URI uses non-https scheme: {}",
            parsed.scheme()
        );
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("session URI carries embedded credentials, refusing to PUT");
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("session URI has no host"))?;
    if !host_pattern.is_match(host) {
        bail!(
            "session URI host {:?} does not match allowlist pattern {}",
            host,
            host_pattern.as_str()
        );
    }
    Ok(())
}

/// Sniff content type from the file extension. Falls back to
/// `application/octet-stream` for unknown extensions. Deliberately small —
/// we are not duplicating libmagic. The upstream API decides what's valid.
pub fn content_type_from_extension(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let mime = match ext.as_deref() {
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("mpeg") | Some("mpg") => "video/mpeg",
        Some("avi") => "video/x-msvideo",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("xml") => "text/xml",
        Some("vtt") => "text/vtt",
        Some("srt") => "application/x-subrip",
        Some("zip") => "application/zip",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    };
    mime.to_string()
}

/// Run the chunked upload loop. Returns when the session terminates with
/// 200/201 or an unrecoverable error.
pub fn run_upload(
    file: &mut File,
    session_uri: &str,
    total_size: u64,
    chunk_size: u64,
    host_pattern: &Regex,
    display_path: &str,
    integration_name: &str,
) -> Result<UploadOutcome> {
    validate_session_uri(session_uri, host_pattern)?;

    eprintln!(
        "uploading {} ({} bytes) to {}",
        display_path, total_size, integration_name
    );

    let chunk_size = if chunk_size == 0 {
        DEFAULT_CHUNK_SIZE
    } else {
        chunk_size
    };

    let mut start: u64 = 0;
    let mut buf = vec![0u8; chunk_size as usize];

    while start < total_size {
        // Always seek before read. After a 308 with smaller-than-sent Range
        // OR after a status probe jumped us forward, the fd may be out of
        // sync with `start`. Seek explicitly every iteration.
        file.seek(SeekFrom::Start(start))
            .context("failed to seek file before reading next chunk")?;

        let end = (start + chunk_size).min(total_size) - 1;
        let len = (end - start + 1) as usize;
        let slice = &mut buf[..len];
        file.read_exact(slice)
            .with_context(|| format!("failed to read chunk at offset {}", start))?;

        let outcome = put_chunk_with_retry(session_uri, slice, start, end, total_size)?;
        match outcome {
            ChunkOutcome::Continue { next_offset } => {
                start = next_offset;
                eprintln!(
                    "  → uploaded {}/{} ({}%)",
                    start,
                    total_size,
                    (start * 100) / total_size.max(1)
                );
            }
            ChunkOutcome::Complete(resource) => {
                eprintln!("  → upload complete");
                return Ok(UploadOutcome {
                    status: resource.status,
                    resource: resource.body,
                });
            }
        }
    }

    // Reached total_size via 308 responses but never got 200/201. One
    // final probe disambiguates. The session may legitimately be complete
    // (all bytes received, finalize pending) or may have truncated.
    match probe(session_uri, total_size)? {
        ProbeOutcome::Complete(resource) => Ok(UploadOutcome {
            status: resource.status,
            resource: resource.body,
        }),
        ProbeOutcome::Incomplete { server_offset } => {
            bail!(
                "upload truncated: sent {} bytes, server reports only {} received",
                total_size,
                server_offset
            )
        }
    }
}

enum ChunkOutcome {
    Continue { next_offset: u64 },
    Complete(FinalResource),
}

enum ProbeOutcome {
    Complete(FinalResource),
    Incomplete { server_offset: u64 },
}

struct FinalResource {
    status: u16,
    body: Value,
}

/// Per-chunk retry with backoff. Honors Retry-After. Retries 5xx, 429, 408,
/// and transient transport errors. After a transport error, probes the
/// server for current offset before retrying so we don't replay bytes the
/// server already accepted.
fn put_chunk_with_retry(
    session_uri: &str,
    chunk: &[u8],
    start: u64,
    end: u64,
    total: u64,
) -> Result<ChunkOutcome> {
    let mut delay = BASE_BACKOFF;
    for attempt in 0..MAX_RETRIES {
        match put_chunk(session_uri, chunk, start, end, total) {
            Ok(resp) => match resp.status() {
                308 => {
                    let next = parse_range_end(&resp)?
                        .map(|n| n + 1)
                        .unwrap_or(0);
                    return Ok(ChunkOutcome::Continue { next_offset: next });
                }
                200 | 201 => {
                    let status = resp.status();
                    let body: Value = resp
                        .into_json()
                        .context("failed to parse final resource JSON")?;
                    return Ok(ChunkOutcome::Complete(FinalResource {
                        status,
                        body,
                    }));
                }
                code if is_retryable_status(code) && attempt + 1 < MAX_RETRIES => {
                    let wait = retry_after(&resp).unwrap_or_else(|| jitter(delay));
                    eprintln!(
                        "  ! chunk PUT {}-{} returned {}, retrying in {:?}",
                        start, end, code, wait
                    );
                    thread::sleep(wait);
                    delay = (delay * 2).min(MAX_BACKOFF);
                }
                code => {
                    let body = read_error_body(resp);
                    return Err(normalize_upload_error(code, body));
                }
            },
            Err(err) => {
                if attempt + 1 < MAX_RETRIES && is_transient(&err) {
                    // Ambiguous: server may or may not have committed bytes.
                    // Probe before retrying.
                    if let Ok(p) = probe(session_uri, total) {
                        match p {
                            ProbeOutcome::Complete(r) => {
                                return Ok(ChunkOutcome::Complete(r));
                            }
                            ProbeOutcome::Incomplete { server_offset }
                                if server_offset > start =>
                            {
                                return Ok(ChunkOutcome::Continue {
                                    next_offset: server_offset,
                                });
                            }
                            _ => {}
                        }
                    }
                    eprintln!(
                        "  ! chunk PUT {}-{} transport error ({}), retrying in {:?}",
                        start, end, err, delay
                    );
                    thread::sleep(jitter(delay));
                    delay = (delay * 2).min(MAX_BACKOFF);
                } else {
                    return Err(anyhow!("chunk PUT failed: {}", err));
                }
            }
        }
    }
    bail!("chunk PUT exhausted retries at offset {}", start)
}

fn put_chunk(
    session_uri: &str,
    chunk: &[u8],
    start: u64,
    end: u64,
    total: u64,
) -> Result<Response, ureq::Error> {
    ureq::put(session_uri)
        .set("Content-Length", &chunk.len().to_string())
        .set(
            "Content-Range",
            &format!("bytes {}-{}/{}", start, end, total),
        )
        .send_bytes(chunk)
}

/// PUT with `Content-Range: bytes */total` and empty body.
/// Server replies 308 with `Range:` showing what it has, or 200/201 if
/// the upload is already complete and the body is the final resource.
fn probe(session_uri: &str, total: u64) -> Result<ProbeOutcome> {
    let resp_result = ureq::put(session_uri)
        .set("Content-Length", "0")
        .set("Content-Range", &format!("bytes */{}", total))
        .send_bytes(&[]);
    let resp = match resp_result {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r, // 308 surfaces here too
        Err(e) => bail!("status probe failed: {}", e),
    };
    match resp.status() {
        308 => {
            let server_offset = parse_range_end(&resp)?.map(|n| n + 1).unwrap_or(0);
            Ok(ProbeOutcome::Incomplete { server_offset })
        }
        200 | 201 => {
            let status = resp.status();
            let body: Value = resp
                .into_json()
                .context("status probe: failed to parse final resource JSON")?;
            Ok(ProbeOutcome::Complete(FinalResource { status, body }))
        }
        code => bail!("status probe returned unexpected status {}", code),
    }
}

/// `Range: bytes=X-Y` → returns Y. Returns None if the header is absent
/// (server has nothing committed yet).
fn parse_range_end(resp: &Response) -> Result<Option<u64>> {
    let Some(raw) = resp.header("range") else {
        return Ok(None);
    };
    let body = raw
        .strip_prefix("bytes=")
        .ok_or_else(|| anyhow!("malformed Range header: {}", raw))?;
    let end = body
        .split('-')
        .nth(1)
        .ok_or_else(|| anyhow!("malformed Range header: {}", raw))?;
    end.parse::<u64>()
        .map(Some)
        .map_err(|_| anyhow!("malformed Range header end: {}", raw))
}

/// Retry-After per RFC 7231: integer delta-seconds OR HTTP-date.
fn retry_after(resp: &Response) -> Option<Duration> {
    let raw = resp.header("retry-after")?;
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs.min(MAX_BACKOFF.as_secs() * 2)));
    }
    // HTTP-date form
    let when = httpdate::parse_http_date(raw).ok()?;
    when.duration_since(SystemTime::now()).ok()
}

fn jitter(base: Duration) -> Duration {
    // ±25% jitter to avoid thundering herd. Crude PRNG via SystemTime.
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let frac = (nanos as f64 / 1_000_000_000.0) * 0.5; // 0..0.5
    let mult = 0.75 + frac; // 0.75..1.25
    Duration::from_millis((base.as_millis() as f64 * mult) as u64)
}

fn is_retryable_status(code: u16) -> bool {
    matches!(code, 408 | 429 | 500..=599)
}

fn is_transient(err: &ureq::Error) -> bool {
    matches!(err, ureq::Error::Transport(_))
}

fn read_error_body(resp: Response) -> Option<Value> {
    let raw = resp.into_string().ok()?;
    serde_json::from_str(&raw).ok().or_else(|| {
        // Not JSON; wrap raw text so the LLM at least sees something.
        Some(Value::String(raw))
    })
}

#[derive(Deserialize)]
struct GoogleErrorEnvelope {
    error: Option<GoogleErrorBody>,
}

#[derive(Deserialize)]
#[allow(dead_code)] // Surfaced via raw body in error messages.
struct GoogleErrorBody {
    code: Option<u16>,
    message: Option<String>,
    errors: Option<Vec<GoogleErrorDetail>>,
}

#[derive(Deserialize)]
struct GoogleErrorDetail {
    reason: Option<String>,
    message: Option<String>,
}

fn first_reason(body: &Value) -> Option<String> {
    let env: GoogleErrorEnvelope = serde_json::from_value(body.clone()).ok()?;
    env.error?
        .errors?
        .into_iter()
        .find_map(|d| d.reason.or(d.message))
}

/// Map known upload error reasons to a structured anyhow error so the
/// caller (and ultimately the LLM) gets a clear category. Unknown errors
/// pass through with the raw body preserved.
fn normalize_upload_error(status: u16, body: Option<Value>) -> anyhow::Error {
    let reason = body.as_ref().and_then(first_reason);
    let raw_summary = body
        .as_ref()
        .map(|v| v.to_string())
        .unwrap_or_else(|| "<no body>".to_string());
    match (status, reason.as_deref()) {
        (403, Some("quotaExceeded")) | (403, Some("uploadLimitExceeded")) => {
            anyhow!(
                "upload rejected: quota exceeded ({}); raw: {}",
                reason.unwrap(),
                raw_summary
            )
        }
        (400, Some("invalidContentType")) => {
            anyhow!(
                "upload rejected: invalid content type for this integration; raw: {}",
                raw_summary
            )
        }
        (400, Some("invalidRange")) => anyhow!(
            "upload rejected: server-side offset mismatch; raw: {}",
            raw_summary
        ),
        _ => anyhow!("upload failed with status {}: {}", status, raw_summary),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern() -> Regex {
        Regex::new(r"^[a-z0-9-]+\.googleapis\.com$").unwrap()
    }

    #[test]
    fn validates_https_googleapis_host() {
        assert!(validate_session_uri(
            "https://www.googleapis.com/upload/youtube/v3/videos?upload_id=abc",
            &pattern()
        )
        .is_ok());
    }

    #[test]
    fn rejects_non_https_scheme() {
        assert!(validate_session_uri(
            "http://www.googleapis.com/upload/youtube/v3/videos",
            &pattern()
        )
        .is_err());
    }

    #[test]
    fn rejects_attacker_host_suffix() {
        assert!(validate_session_uri(
            "https://googleapis.com.attacker.example/upload/youtube/v3/videos",
            &pattern()
        )
        .is_err());
    }

    #[test]
    fn rejects_embedded_credentials() {
        assert!(validate_session_uri(
            "https://attacker:pass@www.googleapis.com/upload/youtube/v3/videos",
            &pattern()
        )
        .is_err());
    }

    #[test]
    fn content_type_from_known_extension() {
        let p = Path::new("/tmp/foo.mp4");
        assert_eq!(content_type_from_extension(p), "video/mp4");
    }

    #[test]
    fn content_type_falls_back_to_octet_stream() {
        let p = Path::new("/tmp/foo.unknownext");
        assert_eq!(
            content_type_from_extension(p),
            "application/octet-stream"
        );
    }

    #[test]
    fn is_retryable_status_table() {
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(408));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(404));
    }
}
