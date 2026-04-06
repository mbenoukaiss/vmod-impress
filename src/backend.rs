use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Take};
use std::str::FromStr;
use chrono::{DateTime, Utc};
use headers_accept::Accept;
use varnish::vcl::{Ctx, HttpHeaders, StrOrBytes, VclBackend, VclError, VclResponse};
use crate::cache::Cache;
use crate::config::SharedConfig;
use crate::error::Error;

fn utf8_header(h: Option<StrOrBytes<'_>>) -> Option<&str> {
    match h {
        Some(StrOrBytes::Utf8(s)) => Some(s),
        _ => None,
    }
}

pub struct FileBackend {
    config: SharedConfig,
    cache: Cache,
}

impl FileBackend {
    pub fn new(config: SharedConfig, cache: Cache) -> Self {
        FileBackend {
            config,
            cache,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ResponseShape {
    NotAllowed,
    NotModified,
    Ok { send_body: bool },
}

impl FileBackend {
    fn get_data(&self, ctx: &mut Ctx) -> Result<Option<FileTransfer>, Error> {
        //hold bereq's borrow for the whole read-side of the request so we
        //don't allocate a String for every header value just to outlive
        //the borrow. shape is computed inside the same scope while the
        //bereq-borrowed &str slices are still alive; the scope returns
        //owned values (FetchResult, ResponseShape) that no longer touch bereq,
        //so we can switch to &mut beresp afterwards via disjoint-field borrow
        let (shape, result) = {
            let bereq = ctx.http_bereq.as_ref().ok_or_else(|| Error::new("Failed to get request data"))?;
            let url_raw = utf8_header(bereq.url()).ok_or_else(|| Error::new("Failed to get URL"))?;
            let method = utf8_header(bereq.method()).unwrap_or("");
            let if_none_match = utf8_header(bereq.header("if-none-match"));
            let if_modified_since = utf8_header(bereq.header("if-modified-since"));
            let accept = self.parse_accept_header(bereq);
            //escape-free URLs come back Borrowed; we only allocate when the URL
            //actually contains percent-encodings
            let url = urlencoding::decode(url_raw)?;

            let pattern = self.config.url_regex.as_ref().expect("Badly initialized config");
            let result = match pattern.captures(&url) {
                Some(captures) if self.config.sizes.get(&captures["size"]).map_or(false, |p| p.matches(&captures["path"])) => {
                    self.cache.get(&captures["path"], &captures["size"], accept)?
                }
                _ => None,
            };

            let shape = result.as_ref().map(|r| decide_response_shape(
                method,
                if_none_match,
                if_modified_since,
                &r.etag,
                r.last_modified,
            ));
            (shape, result)
        };

        let beresp = ctx.http_beresp.as_mut().ok_or_else(|| Error::new("Failed to get response"))?;
        beresp.set_proto("HTTP/1.1")?;

        let result = match result {
            Some(r) => r,
            None => {
                beresp.set_status(404);
                return Ok(None);
            }
        };
        //result was Some so shape was computed
        let shape = shape.expect("shape must be Some when result is Some");

        //serve stale-while-revalidate: Varnish reads stale-while-revalidate=N
        //from beresp.Cache-Control and uses it as beresp.grace, so it serves
        //stale to clients while firing a cheap background revalidation through
        //our 304-aware backend. The previous "immutable" suppressed that.
        let cache_control: &str = if result.is_optimized {
            &self.config.cache_control_optimized
        } else {
            &self.config.cache_control_fallback
        };

        match shape {
            ResponseShape::NotAllowed => {
                beresp.set_status(405);
                Ok(None)
            }
            ResponseShape::NotModified => {
                beresp.set_status(304);
                beresp.set_header("ETag", &result.etag)?;
                beresp.set_header("Vary", "Accept")?;
                beresp.set_header("Cache-Control", cache_control)?;
                Ok(None)
            }
            ResponseShape::Ok { send_body } => {
                beresp.set_status(200);
                beresp.set_header("ETag", &result.etag)?;
                beresp.set_header("Last-Modified", &result.last_modified_str)?;
                beresp.set_header("Content-Length", &result.content_length_str)?;
                beresp.set_header("Content-Type", result.mime)?;
                beresp.set_header("Vary", "Accept")?;
                beresp.set_header("Cache-Control", cache_control)?;
                Ok(if send_body { Some(result.data) } else { None })
            }
        }
    }

    fn parse_accept_header(&self, bereq: &HttpHeaders) -> Option<Accept> {
        match utf8_header(bereq.header("accept")) {
            Some(accept) if accept.trim() != "*/*" => Accept::from_str(accept).ok(),
            _ => None
        }
    }
}

impl VclBackend<FileTransfer> for FileBackend {
    fn get_response(&self, ctx: &mut Ctx) -> Result<Option<FileTransfer>, VclError> {
        match self.get_data(ctx) {
            Ok(transfer) => Ok(transfer),
            Err(e) if is_not_found(&e) => {
                let beresp = ctx.http_beresp.as_mut().ok_or_else(|| VclError::new("Failed to get response".to_owned()))?;
                beresp.set_status(404);
                Ok(None)
            }
            Err(e) => {
                let beresp = ctx.http_beresp.as_mut().ok_or_else(|| VclError::new("Failed to get response".to_owned()))?;
                beresp.set_status(500);
                let _ = beresp.set_header("error", &e.to_string());

                Ok(None)
            }
        }
    }
}

pub struct FileTransfer(Take<BufReader<File>>);

impl FileTransfer {
    pub fn new(file: File, size: u64) -> FileTransfer {
        FileTransfer(BufReader::new(file).take(size))
    }

    pub fn size(&self) -> usize {
        self.0.limit() as usize
    }
}

impl VclResponse for FileTransfer {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, VclError> {
        self.0.read(buf).map_err(|e| VclError::new(e.to_string()))
    }

    fn len(&self) -> Option<usize> {
        Some(self.size())
    }
}

fn decide_response_shape(
    method: &str,
    if_none_match: Option<&str>,
    if_modified_since: Option<&str>,
    etag: &str,
    last_modified: DateTime<Utc>,
) -> ResponseShape {
    if method != "GET" && method != "HEAD" {
        return ResponseShape::NotAllowed;
    }

    if let Some(inm) = if_none_match {
        if etag_matches(inm, etag) {
            return ResponseShape::NotModified;
        }
    } else if let Some(ims) = if_modified_since {
        if let Ok(parsed) = DateTime::parse_from_rfc2822(ims) {
            if parsed.with_timezone(&Utc).timestamp() >= last_modified.timestamp() {
                return ResponseShape::NotModified;
            }
        }
    }

    ResponseShape::Ok { send_body: method == "GET" }
}

fn etag_matches(client: &str, ours: &str) -> bool {
    let normalize = |s: &str| {
        s.strip_prefix("W/").unwrap_or(s).trim_matches('"').to_owned()
    };
    normalize(client) == normalize(ours)
}

fn is_not_found(error: &Error) -> bool {
    if let Error::Other(boxed) = error {
        if let Some(io_err) = boxed.downcast_ref::<std::io::Error>() {
            return io_err.kind() == ErrorKind::NotFound;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn post_returns_not_allowed() {
        assert_eq!(
            decide_response_shape("POST", None, None, "\"abc\"", t(0)),
            ResponseShape::NotAllowed,
        );
    }

    #[test]
    fn put_returns_not_allowed() {
        assert_eq!(
            decide_response_shape("PUT", None, None, "\"abc\"", t(0)),
            ResponseShape::NotAllowed,
        );
    }

    #[test]
    fn get_no_conditional_returns_ok_with_body() {
        assert_eq!(
            decide_response_shape("GET", None, None, "\"abc\"", t(0)),
            ResponseShape::Ok { send_body: true },
        );
    }

    #[test]
    fn head_no_conditional_returns_ok_no_body() {
        assert_eq!(
            decide_response_shape("HEAD", None, None, "\"abc\"", t(0)),
            ResponseShape::Ok { send_body: false },
        );
    }

    #[test]
    fn matching_etag_returns_not_modified() {
        assert_eq!(
            decide_response_shape("GET", Some("\"abc\""), None, "\"abc\"", t(0)),
            ResponseShape::NotModified,
        );
    }

    #[test]
    fn weak_etag_matches_strong() {
        assert_eq!(
            decide_response_shape("GET", Some("W/\"abc\""), None, "\"abc\"", t(0)),
            ResponseShape::NotModified,
        );
    }

    #[test]
    fn unquoted_client_etag_matches_quoted_server() {
        assert_eq!(
            decide_response_shape("GET", Some("abc"), None, "\"abc\"", t(0)),
            ResponseShape::NotModified,
        );
    }

    #[test]
    fn non_matching_etag_returns_ok() {
        assert_eq!(
            decide_response_shape("GET", Some("\"xyz\""), None, "\"abc\"", t(0)),
            ResponseShape::Ok { send_body: true },
        );
    }

    #[test]
    fn etag_mismatch_does_not_fall_through_to_ims() {
        // RFC 7232 §6: If-None-Match takes precedence; If-Modified-Since must
        // be ignored when If-None-Match is present, even if it doesn't match.
        let ims_after = "Sun, 06 Nov 2050 08:49:37 GMT";
        assert_eq!(
            decide_response_shape("GET", Some("\"xyz\""), Some(ims_after), "\"abc\"", t(0)),
            ResponseShape::Ok { send_body: true },
        );
    }

    #[test]
    fn ims_equal_to_last_modified_returns_not_modified() {
        // RFC 7232 §3.3: 304 when last-modified <= client date.
        // We compare second-precision since HTTP dates have second precision.
        let last_modified = t(1_700_000_000);
        let ims = last_modified.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        assert_eq!(
            decide_response_shape("GET", None, Some(&ims), "\"abc\"", last_modified),
            ResponseShape::NotModified,
        );
    }

    #[test]
    fn ims_after_last_modified_returns_not_modified() {
        let last_modified = t(1_700_000_000);
        let ims_later = (last_modified + chrono::Duration::seconds(60)).format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        assert_eq!(
            decide_response_shape("GET", None, Some(&ims_later), "\"abc\"", last_modified),
            ResponseShape::NotModified,
        );
    }

    #[test]
    fn ims_before_last_modified_returns_ok() {
        let last_modified = t(1_700_000_000);
        let ims_earlier = (last_modified - chrono::Duration::seconds(60)).format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        assert_eq!(
            decide_response_shape("GET", None, Some(&ims_earlier), "\"abc\"", last_modified),
            ResponseShape::Ok { send_body: true },
        );
    }

    #[test]
    fn malformed_ims_falls_through_to_ok() {
        assert_eq!(
            decide_response_shape("GET", None, Some("not a date"), "\"abc\"", t(1_700_000_000)),
            ResponseShape::Ok { send_body: true },
        );
    }

    #[test]
    fn head_with_matching_etag_returns_not_modified() {
        // 304 has no body regardless of method, so HEAD + match is the same shape as GET + match.
        assert_eq!(
            decide_response_shape("HEAD", Some("\"abc\""), None, "\"abc\"", t(0)),
            ResponseShape::NotModified,
        );
    }
}
