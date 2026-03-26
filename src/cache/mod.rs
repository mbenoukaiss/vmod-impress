mod cleaner;
mod file_saver;
mod pre_optimizer;
mod watcher;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::ops::Deref;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc, Mutex, RwLock};
use std::sync::mpsc::Sender;
use std::thread;
use chrono::{DateTime, Utc};
use headers_accept::Accept;
use image::ImageFormat;
use mediatype::MediaType;
use walkdir::WalkDir;
use crate::backend::FileTransfer;
use crate::cache::file_saver::{InFlight, OptimizeJob};
use crate::config::{Config, Extension, SharedConfig};
use crate::error::Error;
use crate::utils;

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

            file_saver::spawn(thread_config.clone(), thread_data.clone(), thread_in_flight.clone(), rx);
            watcher::spawn(thread_config.clone(), thread_data.clone(), thread_tx.clone(), thread_in_flight.clone());
            pre_optimizer::spawn(thread_config.clone(), thread_data.clone(), thread_tx.clone(), thread_in_flight.clone());

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
        let mut lock = images.write().unwrap();

        let supported_extensions = ImageFormat::all()
            .flat_map(ImageFormat::extensions_str)
            .map(Deref::deref)
            .collect::<HashSet<&str>>();

        let files = config.roots.iter()
            .flat_map(|root| WalkDir::new(root).into_iter()
                .filter_map(Result::ok)
                .filter(|e| !e.file_type().is_dir())
                .map(|e| (root.clone(), e)));

        for (root, file) in files {
            let filename = file.path().to_string_lossy().to_string();
            let filename_without_root = match file.path().strip_prefix(&root).ok().and_then(|p| p.to_str()) {
                Some(s) => s,
                None => {
                    //either WalkDir returned a path that does not start with its root (shouldn't happen)
                    //or the path contains non-UTF8 bytes; either way, skip it
                    error!("skipping unparseable path under root {:?}: {:?}", root, file.path());
                    continue;
                }
            };

            if let (Some(stem), Some(extension)) = utils::decompose_filename(filename_without_root) {
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

                        item.add(size.to_owned(), extension.to_owned(), path);
                    }
                }

                lock.insert(stem.to_owned(), item);
            }
        }
    }

    pub fn get(&self, image_id: &str, size: &str, accept: Option<Accept>) -> Result<Option<FetchResult>, Error> {
        let lock = self.data.read()?;
        let Some(cache) = lock.get(image_id) else {
            return Ok(None);
        };

        //batch all missing extensions into one OptimizeJob — saver reads + resizes
        //the source ONCE for the whole job, then encodes each extension
        let missing: Vec<Extension> = self.config.extensions.iter()
            .filter(|ext| !cache.has(size, **ext))
            .copied()
            .collect();
        if !missing.is_empty() {
            self.enqueue_optimize(image_id, size, missing);
        }

        let converted_extensions = self.config.extensions.iter()
            .filter(|ext| cache.has(size, **ext))
            .map(|ext| ext.to_media_type())
            .collect::<Vec<MediaType>>();

        let appropriate_extension = accept.as_ref()
            .and_then(|accept| accept.negotiate(converted_extensions.iter()))
            .and_then(|media_type| Extension::from_ext(media_type.subty.as_str()))
            .unwrap_or(self.config.default_format);

        if let Some(file) = cache.get(size, appropriate_extension) {
            let path = Path::new(file);

            if path.exists() {
                return read_image(file, true, appropriate_extension.mime_str());
            } else {
                //the image was in cache but the file did not exist, maybe it got deleted
                self.enqueue_optimize(image_id, size, vec![appropriate_extension]);
            }
        }

        //return the image as is, it will be optimized later
        read_image(&cache.base_image_path, false, cache.base_mime)
    }

    fn enqueue_optimize(&self, image_id: &str, size: &str, extensions: Vec<Extension>) {
        let key = (image_id.to_owned(), size.to_owned());
        //dedup: don't enqueue a second job for the same (image_id, size) if one
        //is already in flight; the in-flight job covers the work
        match self.in_flight.lock() {
            Ok(mut guard) => {
                if !guard.insert(key.clone()) {
                    return;
                }
            }
            Err(_) => return,
        }

        if let Err(e) = self.create_image_tx.send(OptimizeJob {
            image_id: key.0,
            size: key.1.clone(),
            extensions,
        }) {
            //channel closed (saver thread died); release the in_flight slot
            //so the slot doesn't leak forever
            error!("optimize channel closed: {}", e);
            if let Ok(mut guard) = self.in_flight.lock() {
                guard.remove(&(image_id.to_owned(), size.to_owned()));
            }
        }
    }
}

fn read_image(path: &str, is_optimized: bool, mime: &'static str) -> Result<Option<FetchResult>, Error> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;

    Ok(Some(FetchResult {
        data: FileTransfer::new(file, metadata.len()),
        last_modified: DateTime::from(metadata.modified()?),
        inode: metadata.ino(),
        mime,
        is_optimized,
    }))
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
    //the Borrow impl.
    pub optimized: [HashMap<String, String>; 3],
}

impl CacheImage {
    pub fn new(base_image_path: String, base_mime: &'static str) -> Self {
        CacheImage {
            base_image_path,
            base_mime,
            optimized: [HashMap::new(), HashMap::new(), HashMap::new()],
        }
    }

    pub fn add<P: AsRef<Path>>(&mut self, size: String, ext: Extension, path: P) {
        self.optimized[ext as usize].insert(size, path.as_ref().to_string_lossy().to_string());
    }

    pub fn get(&self, size: &str, ext: Extension) -> Option<&String> {
        self.optimized[ext as usize].get(size)
    }

    pub fn has(&self, size: &str, ext: Extension) -> bool {
        self.optimized[ext as usize].contains_key(size)
    }
}


pub struct FetchResult {
    pub data: FileTransfer,
    pub last_modified: DateTime<Utc>,
    pub inode: u64,
    pub mime: &'static str,
    pub is_optimized: bool,
}
