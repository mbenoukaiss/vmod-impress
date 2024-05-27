use std::error::Error as StdError;
use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufReader, Read, Take};
use chrono::{DateTime, Utc};
use varnish::vcl::backend::{Serve, Transfer};
use varnish::vcl::ctx::Ctx;
use varnish::vcl::http::HTTP;
use crate::cache::Cache;
use crate::config::{Config, Extension};
use crate::error::Error;

pub struct FileBackend {
    config: Config,
    cache: Cache,
}

impl FileBackend {
    pub fn new(config: Config, cache: Cache) -> Self {
        FileBackend { config, cache }
    }
}

impl FileBackend {
    fn get_data(&self, ctx: &mut Ctx) -> Result<Option<FileTransfer>, Error> {
        let bereq = ctx.http_bereq.as_ref().unwrap();
        let bereq_method = bereq.method().unwrap_or("");
        let bereq_url = bereq.url().unwrap();
        let beresp = ctx.http_beresp.as_mut().unwrap();
        let mut transfer = None;

        let pattern = self.config.url_regex.as_ref().expect("Badly initialized config");

        if let Some(captures) = pattern.captures(bereq_url) {
            if !self.config.sizes.get(&captures["size"]).map_or(false, |p| p.matches(&captures["path"])) {
                respond!(ctx, 404);
            }

            let extensions = self.get_supported_extensions(bereq);
            let Some((data, last_modified, mime)) = self.cache.get(&captures["path"], &captures["size"], extensions)? else {
                respond!(ctx, 404);
            };

            let (is_304, etag) = process_cache_headers(&bereq, &captures["path"], data.size(), &last_modified);
            if is_304 {
                beresp.set_status(304);
            }

            beresp.set_proto("HTTP/1.1")?;
            beresp.set_header("etag", &etag)?;
            beresp.set_header("last-modified", &last_modified.format("%a, %d %b %Y %H:%M:%S GMT").to_string())?;
            beresp.set_header("content-length", &data.size().to_string())?;
            beresp.set_header("content-type", mime)?;

            if bereq_method != "HEAD" && bereq_method != "GET" {
                beresp.set_status(405);
            } else {
                beresp.set_status(200);

                if bereq_method == "GET" {
                    transfer = Some(data);
                }
            }
        } else {
            respond!(ctx, 404);
        }

        Ok(transfer)
    }

    fn get_supported_extensions(&self, bereq: &HTTP) -> Vec<Extension> {
        let Some(accept) = bereq.header("accept") else {
            return vec![self.config.default_format];
        };

        let mut extensions = Vec::with_capacity(3);
        for extension in &self.config.extensions {
            let supports_format = extension.extensions()
                .into_iter()
                .find(|&&ext| accept.contains(ext))
                .is_some();

            if supports_format {
                extensions.push(*extension);
            }
        }

        if !extensions.contains(&self.config.default_format) {
            extensions.push(self.config.default_format);
        }

        extensions
    }
}

impl Serve<FileTransfer> for FileBackend<> {
    fn get_type(&self) -> &str {
        "impress"
    }

    fn get_headers(&self, ctx: &mut Ctx) -> Result<Option<FileTransfer>, Box<dyn StdError>> {
        match self.get_data(ctx) {
            Ok(transfer) => Ok(transfer),
            Err(e) => {
                let beresp = ctx.http_beresp.as_mut().unwrap();
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

fn process_cache_headers(bereq: &HTTP, path: &str, size: usize, last_modified: &DateTime<Utc>) -> (bool, String) {
    let etag = generate_etag(path, size, &last_modified);

    if let Some(inm) = bereq.header("if-none-match") {
        if inm == etag || (inm.starts_with("W/") && inm[2..] == etag) {
            return (true, etag);
        }
    } else if let Some(ims) = bereq.header("if-modified-since") {
        if let Ok(t) = DateTime::parse_from_rfc2822(ims) {
            if t > *last_modified {
                return (true, etag);
            }
        }
    }

    return (false, etag);
}

fn generate_etag(path: &str, size: usize, last_modified: &DateTime<Utc>) -> String {
    let mut h = DefaultHasher::new();
    (path, size, last_modified).hash(&mut h);
    h.finish().to_string()
}
