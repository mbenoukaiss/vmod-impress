use std::error::Error as StdError;
use std::fs::{Metadata};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Cursor, Read};
use std::os::unix::fs::MetadataExt;
use chrono::{DateTime, Utc};
use varnish::vcl::backend::{Serve, Transfer};
use varnish::vcl::ctx::Ctx;
use varnish::vcl::http::HTTP;
use crate::config::Config;
use crate::respond;
use crate::error::Error;
use crate::images::Cache;

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
            let Some((data, metadata)) = self.cache.get(&captures["path"], &captures["size"], "webp")? else {
                respond!(ctx, 404);
            };

            //TODO: check if the image has already been converted
            //TODO: if not convert it and send the converted image to an update queue
            //TODO: send the image to the client

            if let Some(metadata) = metadata {
                let (is_304, etag, modified) = get_idk_rename(bereq, metadata);

                if is_304 {
                    beresp.set_status(304);
                }

                beresp.set_header("etag", &etag)?;
                beresp.set_header("last-modified", &modified.format("%a, %d %b %Y %H:%M:%S GMT").to_string())?;
            }

            beresp.set_proto("HTTP/1.1")?;
            beresp.set_header("content-length", &data.len().to_string())?;
            beresp.set_header("content-type", "image/webp")?;

            if bereq_method != "HEAD" && bereq_method != "GET" {
                beresp.set_status(405);
            } else {
                beresp.set_status(200);

                if bereq_method == "GET" {
                    transfer = Some(FileTransfer::new(data));
                }
            }
        } else {
            respond!(ctx, 404);
        }

        Ok(transfer)
    }
}

impl Serve<FileTransfer> for FileBackend<> {
    fn get_type(&self) -> &str {
        "shrink"
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

pub struct FileTransfer {
    reader: Cursor<Vec<u8>>,
}

impl FileTransfer {
    pub fn new(data: Vec<u8>) -> Self {
        FileTransfer {
            reader: Cursor::new(data),
        }
    }
}

impl Transfer for FileTransfer {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn StdError>> {
        self.reader.read(buf).map_err(|e| e.into())
    }

    fn len(&self) -> Option<usize> {
        Some(self.reader.get_ref().len())
    }
}

fn get_idk_rename(bereq: &HTTP, metadata: Metadata) -> (bool, String, DateTime<Utc>) {
    let modified: DateTime<Utc> = DateTime::from(metadata.modified().unwrap());
    let etag = generate_etag(&metadata);

    if let Some(inm) = bereq.header("if-none-match") {
        if inm == etag || (inm.starts_with("W/") && inm[2..] == etag) {
            return (true, etag, modified);
        }
    } else if let Some(ims) = bereq.header("if-modified-since") {
        if let Ok(t) = DateTime::parse_from_rfc2822(ims) {
            if t > modified {
                return (true, etag, modified);
            }
        }
    }

    return (false, etag, modified);
}

fn generate_etag(metadata: &Metadata) -> String {
    #[derive(Hash)]
    struct ShortMd {
        inode: u64,
        size: u64,
        modified: std::time::SystemTime,
    }

    let smd = ShortMd {
        inode: metadata.ino(),
        size: metadata.size(),
        modified: metadata.modified().unwrap(),
    };
    let mut h = DefaultHasher::new();
    smd.hash(&mut h);
    format!("\"{}\"", h.finish())
}
