use std::error::Error as StdError;
use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufReader, Read, Take};
use std::str::FromStr;
use chrono::DateTime;
use headers_accept::Accept;
use varnish::vcl::backend::{Serve, Transfer};
use varnish::vcl::ctx::Ctx;
use varnish::vcl::http::HTTP;
use crate::cache::{Cache, FetchResult};
use crate::config::Config;
use crate::error::Error;

pub struct FileBackend {
    config: Config,
    cache: Cache,
}

impl FileBackend {
    pub fn new(config: Config, cache: Cache) -> Self {
        FileBackend {
            config,
            cache,
        }
    }
}

impl FileBackend {
    fn get_data(&self, ctx: &mut Ctx) -> Result<Option<FileTransfer>, Error> {
        let bereq = ctx.http_bereq.as_ref().ok_or_else(|| Error::new("Failed to get request data"))?;
        let bereq_method = bereq.method().unwrap_or("");
        let bereq_url = urlencoding::decode(bereq.url().ok_or_else(|| Error::new("Failed to get URL"))?)?;
        let beresp = ctx.http_beresp.as_mut().ok_or_else(|| Error::new("Failed to get response"))?;
        let mut transfer = None;

        let pattern = self.config.url_regex.as_ref().expect("Badly initialized config");

        if let Some(captures) = pattern.captures(bereq_url.as_ref()) {
            if !self.config.sizes.get(&captures["size"]).map_or(false, |p| p.matches(&captures["path"])) {
                respond!(ctx, 404);
            }

            let accept = self.parse_accept_header(bereq);
            let Some(result) = self.cache.get(&captures["path"], &captures["size"], accept)? else {
                respond!(ctx, 404);
            };

            let (is_304, etag) = process_cache_headers(&bereq, &result);
            if is_304 {
                beresp.set_status(304);
            }

            beresp.set_proto("HTTP/1.1")?;
            beresp.set_header("ETag", &etag)?;
            beresp.set_header("Last-Modified", &result.last_modified.format("%a, %d %b %Y %H:%M:%S GMT").to_string())?;
            beresp.set_header("Content-Length", &result.data.size().to_string())?;
            beresp.set_header("Content-Type", result.mime)?;
            beresp.set_header("Vary", "Accept")?;
            beresp.set_header("Cache-Control", if result.is_optimized {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            })?;

            if bereq_method != "HEAD" && bereq_method != "GET" {
                beresp.set_status(405);
            } else {
                beresp.set_status(200);

                if bereq_method == "GET" {
                    transfer = Some(result.data);
                }
            }
        } else {
            respond!(ctx, 404);
        }

        Ok(transfer)
    }

    fn parse_accept_header(&self, bereq: &HTTP) -> Option<Accept> {
        match bereq.header("accept") {
            Some(accept) if accept.trim() != "*/*" => Accept::from_str(accept).ok(),
            _ => None
        }
    }
}

impl Serve<FileTransfer> for FileBackend {
    fn get_type(&self) -> &str {
        "impress"
    }

    fn get_headers(&self, ctx: &mut Ctx) -> Result<Option<FileTransfer>, Box<dyn StdError>> {
        match self.get_data(ctx) {
            Ok(transfer) => Ok(transfer),
            Err(e) => {
                let beresp = ctx.http_beresp.as_mut().ok_or_else(|| Error::new("Failed to get response"))?;
                beresp.set_status(500);
                beresp.set_header("error", &e.to_string())?;

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

impl Transfer for FileTransfer {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn StdError>> {
        self.0.read(buf).map_err(|e| e.into())
    }

    fn len(&self) -> Option<usize> {
        Some(self.size())
    }
}

fn process_cache_headers(bereq: &HTTP, result: &FetchResult) -> (bool, String) {
    let etag = generate_etag(result);

    if let Some(inm) = bereq.header("if-none-match") {
        if inm == etag || (inm.starts_with("W/") && inm[2..] == etag) {
            return (true, etag);
        }
    } else if let Some(ims) = bereq.header("if-modified-since") {
        if let Ok(t) = DateTime::parse_from_rfc2822(ims) {
            if t > result.last_modified {
                return (true, etag);
            }
        }
    }

    return (false, etag);
}

fn generate_etag(result: &FetchResult) -> String {
    let mut h = DefaultHasher::new();
    (result.inode, result.data.size(), result.last_modified.timestamp(), result.is_optimized).hash(&mut h);
    h.finish().to_string()
}
