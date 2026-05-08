mod cleaner;
mod file_saver;
mod pre_optimizer;
mod watcher;

use crate::backend::FileTransfer;
use crate::cache::file_saver::{InFlight, OptimizeJob};
use crate::config::{Config, Extension, SharedConfig};
use crate::error::Error;
use crate::static_files::Transfer;
use crate::utils;
use chrono::{DateTime, Utc};
use headers_accept::Accept;
use image::ImageFormat;
use mediatype::MediaType;
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Deref;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{mpsc, Arc};
use std::thread;
use walkdir::WalkDir;

pub type CacheData = Arc<RwLock<HashMap<String, CacheImage>>>;

pub struct Cache {
    config: SharedConfig,
    data: CacheData,
    create_image_tx: Sender<OptimizeJob>,
    in_flight: InFlight,
}

impl Cache {
    pub fn new(config: SharedConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let data = CacheData::default();
        let in_flight: InFlight = Arc::new(Mutex::new(HashSet::new()));

        let thread_config = config.clone();
        let thread_data = data.clone();
        let thread_tx = tx.clone();
        let thread_in_flight = in_flight.clone();

        //done in a thread to avoid varnish hanging for seconds on startup, but could also
        //lead to 404s if requests are made right after varnish was started
        //could be improved by fetching from disk before returning a 404 ? or too complex for not much ?
        thread::spawn(move || {
            Self::load_images(&thread_config, thread_data.clone());

            match cleaner::sweep_once(&thread_config, &thread_data) {
                Ok(0) => {}
                Ok(n) => info!("startup orphan sweep: removed {} stale cache file(s)", n),
                Err(e) => error!("startup orphan sweep failed: {}", e),
            }

            file_saver::spawn(
                thread_config.clone(),
                thread_data.clone(),
                thread_in_flight.clone(),
                rx,
            );
            watcher::spawn(
                thread_config.clone(),
                thread_data.clone(),
                thread_tx.clone(),
                thread_in_flight.clone(),
            );
            pre_optimizer::spawn(
                thread_config.clone(),
                thread_data.clone(),
                thread_tx.clone(),
                thread_in_flight.clone(),
            );

            cleaner::spawn(thread_config.clone(), thread_data.clone());
        });

        Cache {
            config,
            data,
            create_image_tx: tx,
            in_flight,
        }
    }

    fn load_images(config: &Config, images: CacheData) {
        let mut lock = images.write();

        let supported_extensions = ImageFormat::all()
            .flat_map(ImageFormat::extensions_str)
            .map(Deref::deref)
            .collect::<HashSet<&str>>();

        let files = config.roots.iter().flat_map(|root| {
            WalkDir::new(root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| !e.file_type().is_dir())
                .map(|e| (root.clone(), e))
        });

        for (root, file) in files {
            let filename = file.path().to_string_lossy().to_string();
            let filename_without_root = match file
                .path()
                .strip_prefix(&root)
                .ok()
                .and_then(|p| p.to_str())
            {
                Some(s) => s,
                None => {
                    //either WalkDir returned a path that does not start with its root (shouldn't happen)
                    //or the path contains non-UTF8 bytes; either way, skip it
                    error!(
                        "skipping unparseable path under root {:?}: {:?}",
                        root,
                        file.path()
                    );
                    continue;
                }
            };

            if let (Some(stem), Some(extension)) = utils::decompose_filename(filename_without_root)
            {
                if !supported_extensions.contains(extension) {
                    continue;
                }

                //source mtime is used to invalidate cache files that were written
                //before the source's last modification (i.e. the source changed during
                //downtime and the live notify watcher never saw it)
                let source_mtime = fs::metadata(&filename).ok().and_then(|m| m.modified().ok());

                //MIME of the source, derived once at load time
                let source_mime = ImageFormat::from_path(&filename)
                    .map(|f| f.to_mime_type())
                    .unwrap_or("application/octet-stream");

                let mut item = CacheImage::new(filename, source_mime);

                //load optimized images from cache, dropping anything older than the source
                for size in config.sizes.keys() {
                    for extension in &config.extensions {
                        let mut path = PathBuf::from(&config.cache_directory);
                        path.push(size);
                        path.push(stem);
                        path.set_extension(extension.extensions().first().unwrap());

                        if !path.exists() {
                            continue;
                        }

                        let cache_mtime = fs::metadata(&path).ok().and_then(|m| m.modified().ok());
                        let stale = match (source_mtime, cache_mtime) {
                            (Some(s), Some(c)) => c < s,
                            //metadata read failed; keep the cache entry rather than risk
                            //deleting good data — eventual consistency via watcher / SWR
                            _ => false,
                        };

                        if stale {
                            if let Err(e) = fs::remove_file(&path) {
                                error!("failed to remove stale cache file {:?}: {}", path, e);
                            }
                            continue;
                        }

                        if let Err(e) = item.add(size.to_owned(), extension.to_owned(), &path) {
                            error!("failed to read metadata for cache file {:?}: {}", path, e);
                        }
                    }
                }

                lock.insert(stem.to_owned(), item);
            }
        }
    }

    pub fn get(
        &self,
        image_id: &str,
        size: &str,
        accept: Option<Accept>,
    ) -> Result<Option<FetchResult>, Error> {
        let lock = self.data.read();
        let Some(cache) = lock.get(image_id) else {
            return Ok(None);
        };

        //batch all missing extensions into one OptimizeJob — saver reads + resizes
        //the source ONCE for the whole job, then encodes each extension
        let missing: Vec<Extension> = self
            .config
            .extensions
            .iter()
            .filter(|ext| !cache.has(size, **ext))
            .copied()
            .collect();
        if !missing.is_empty() {
            self.enqueue_optimize(image_id, size, missing);
        }

        //negotiate only matters when the client sent an Accept header. Without
        //one we go straight to default_format, skipping the whole MediaType
        //buffer build. With one we fill a stack array (config.extensions has
        //at most 3 entries) instead of heap-allocating a Vec per request.
        let appropriate_extension = if let Some(accept) = accept.as_ref() {
            let mut media_buf: [MediaType<'static>; 3] =
                std::array::from_fn(|_| Extension::JPEG.to_media_type());
            let mut n = 0;
            for ext in &self.config.extensions {
                if cache.has(size, *ext) {
                    media_buf[n] = ext.to_media_type();
                    n += 1;
                }
            }
            accept
                .negotiate(media_buf[..n].iter())
                .and_then(|media_type| Extension::from_ext(media_type.subty.as_str()))
                .unwrap_or(self.config.default_format)
        } else {
            self.config.default_format
        };

        if let Some(variant) = cache.get(size, appropriate_extension) {
            let path = Path::new(&variant.path);

            if path.exists() {
                return read_image_optimized(
                    variant,
                    appropriate_extension.mime_str(),
                    self.config.cache_control_value.clone(),
                );
            } else {
                //the image was in cache but the file did not exist, maybe it got deleted
                self.enqueue_optimize(image_id, size, vec![appropriate_extension]);
            }
        }

        //return the image as is, it will be optimized later
        read_image_fallback(
            &cache.base_image_path,
            cache.base_mime,
            self.config.cache_control_fallback.clone(),
        )
    }

    fn enqueue_optimize(&self, image_id: &str, size: &str, extensions: Vec<Extension>) {
        let key = (image_id.to_owned(), size.to_owned());
        //dedup: don't enqueue a second job for the same (image_id, size) if one
        //is already in flight; the in-flight job covers the work
        if !self.in_flight.lock().insert(key.clone()) {
            return;
        }

        if let Err(e) = self.create_image_tx.send(OptimizeJob {
            image_id: key.0,
            size: key.1.clone(),
            extensions,
        }) {
            //channel closed (saver thread died); release the in_flight slot
            //so the slot doesn't leak forever
            error!("optimize channel closed: {}", e);
            self.in_flight
                .lock()
                .remove(&(image_id.to_owned(), size.to_owned()));
        }
    }
}

fn read_image_optimized(
    variant: &CacheVariant,
    mime: &'static str,
    cache_control: Arc<str>,
) -> Result<Option<FetchResult>, Error> {
    let file = File::open(&variant.path)?;
    Ok(Some(FetchResult {
        data: Transfer::File(FileTransfer::new(file, variant.size)),
        last_modified: variant.last_modified,
        last_modified_str: variant.last_modified_str.clone(),
        etag: variant.etag.clone(),
        content_length_str: variant.content_length_str.clone(),
        mime,
        is_optimized: true,
        cache_control,
    }))
}

fn read_image_fallback(
    path: &str,
    mime: &'static str,
    cache_control: Arc<str>,
) -> Result<Option<FetchResult>, Error> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let size = metadata.len();
    let last_modified: DateTime<Utc> = DateTime::from(metadata.modified()?);
    let inode = metadata.ino();
    let last_modified_str = format_http_date(last_modified);
    let content_length_str = size.to_string();
    let etag = compute_etag(inode, size, last_modified.timestamp(), false);

    Ok(Some(FetchResult {
        data: Transfer::File(FileTransfer::new(file, size)),
        last_modified,
        last_modified_str: Arc::from(last_modified_str.as_str()),
        etag: Arc::from(etag.as_str()),
        content_length_str: Arc::from(content_length_str.as_str()),
        mime,
        is_optimized: false,
        cache_control,
    }))
}

pub(crate) fn format_http_date(dt: DateTime<Utc>) -> String {
    dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

pub(crate) fn compute_etag(inode: u64, size: u64, mtime_secs: i64, is_optimized: bool) -> String {
    let mut h = DefaultHasher::new();
    //match the original tuple shape (u64, usize, i64, bool) so etags
    //don't change across the refactor — clients with cached If-None-Match
    //keep getting 304s on unchanged variants
    (inode, size as usize, mtime_secs, is_optimized).hash(&mut h);
    format!("\"{}\"", h.finish())
}

#[derive(Clone, Debug)]
pub struct CacheVariant {
    pub path: String,
    pub size: u64,
    pub last_modified: DateTime<Utc>,
    //Pre-built header-value strings stored as Arc<str>: cloning is a refcount
    //bump (no allocation, no copy) and the same bytes are shared across
    //every request that hits this variant.
    pub last_modified_str: Arc<str>,
    pub content_length_str: Arc<str>,
    pub etag: Arc<str>,
}

impl CacheVariant {
    pub fn from_path<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let path_ref = path.as_ref();
        let metadata = fs::metadata(path_ref)?;
        let last_modified: DateTime<Utc> = DateTime::from(metadata.modified()?);
        let size = metadata.len();
        let inode = metadata.ino();

        let last_modified_str = format_http_date(last_modified);
        let content_length_str = size.to_string();
        let etag = compute_etag(inode, size, last_modified.timestamp(), true);

        Ok(CacheVariant {
            path: path_ref.to_string_lossy().to_string(),
            size,
            last_modified,
            last_modified_str: Arc::from(last_modified_str.as_str()),
            content_length_str: Arc::from(content_length_str.as_str()),
            etag: Arc::from(etag.as_str()),
        })
    }
}

#[derive(Clone, Debug)]
pub struct CacheImage {
    pub base_image_path: String,
    //MIME of the source image, used when serving the un-optimized fallback.
    //Stored once at insert so the request hot path doesn't re-stat the path
    //or re-call ImageFormat::from_path on every request.
    pub base_mime: &'static str,
    //Indexed by Extension as usize (JPEG=0, WEBP=1, AVIF=2). The hot-path
    //lookup is `self.optimized[ext as usize].get(size_str)` which doesn't
    //need to allocate a key — `HashMap<String, _>::get(&str)` works via
    //the Borrow impl. CacheVariant carries the path plus pre-built
    //header strings so the per-request response build is allocation-free.
    pub optimized: [HashMap<String, CacheVariant>; 3],
}

impl CacheImage {
    pub fn new(base_image_path: String, base_mime: &'static str) -> Self {
        CacheImage {
            base_image_path,
            base_mime,
            optimized: [HashMap::new(), HashMap::new(), HashMap::new()],
        }
    }

    pub fn add<P: AsRef<Path>>(
        &mut self,
        size: String,
        ext: Extension,
        path: P,
    ) -> std::io::Result<()> {
        let variant = CacheVariant::from_path(path)?;
        self.optimized[ext as usize].insert(size, variant);
        Ok(())
    }

    pub fn get(&self, size: &str, ext: Extension) -> Option<&CacheVariant> {
        self.optimized[ext as usize].get(size)
    }

    pub fn has(&self, size: &str, ext: Extension) -> bool {
        self.optimized[ext as usize].contains_key(size)
    }
}

pub struct FetchResult {
    pub data: Transfer,
    pub last_modified: DateTime<Utc>,
    pub last_modified_str: Arc<str>,
    pub etag: Arc<str>,
    pub content_length_str: Arc<str>,
    pub mime: &'static str,
    /// Whether the body bytes are the result of running an optimizer.
    /// Consumed only by tests today (production picks Cache-Control via the
    /// pre-built header below); kept as a field so any future telemetry,
    /// vary-by-optimization logging, or per-outcome metrics can read it
    /// without re-deriving from the producer's local state.
    #[allow(dead_code)]
    pub is_optimized: bool,
    /// Pre-picked Cache-Control header value. The producer of FetchResult
    /// is responsible for choosing optimized vs fallback so the response
    /// path doesn't have to branch.
    pub cache_control: Arc<str>,
}
