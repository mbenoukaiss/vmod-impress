mod file_saver;
mod pre_optimizer;
mod watcher;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::ops::Deref;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc, RwLock};
use std::sync::mpsc::Sender;
use std::thread;
use chrono::{DateTime, Utc};
use headers_accept::Accept;
use image::ImageFormat;
use mediatype::MediaType;
use walkdir::WalkDir;
use crate::backend::FileTransfer;
use crate::cache::file_saver::OptimizeImage;
use crate::config::{Config, Extension};
use crate::error::Error;
use crate::utils;

pub type CacheData = Arc<RwLock<HashMap<String, CacheImage>>>;

pub struct Cache {
    config: Config,
    data: CacheData,
    create_image_tx: Sender<OptimizeImage>,
}

impl Cache {
    pub fn new(config: &Config) -> Self {
        let (tx, rx) = mpsc::channel();
        let data = CacheData::default();

        let thread_config = config.clone();
        let thread_data = data.clone();
        let thread_tx = tx.clone();
        thread::spawn(move || {
            Self::load_images(&thread_config, thread_data.clone());
            file_saver::spawn(thread_config.clone(), thread_data.clone(), rx);
            watcher::spawn(thread_config.clone(), thread_data.clone(), thread_tx.clone());
            pre_optimizer::spawn(thread_config.clone(), thread_data.clone(), thread_tx.clone());
        });

        Cache {
            config: config.clone(),
            data,
            create_image_tx: tx,
        }
    }

    fn load_images(config: &Config, images: CacheData) {
        let mut lock = images.write().unwrap();

        let supported_extensions = ImageFormat::all()
            .flat_map(ImageFormat::extensions_str)
            .map(Deref::deref)
            .collect::<HashSet<&str>>();

        let files = WalkDir::new(&config.root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| !e.file_type().is_dir());

        for file in files {
            let filename = file.path().to_string_lossy().to_string();
            let filename_without_root = file.path().strip_prefix(&config.root).unwrap().to_string_lossy().to_string();

            if let (Some(stem), Some(extension)) = utils::decompose_filename(&filename_without_root) {
                if !supported_extensions.contains(extension) {
                    continue;
                }

                let mut item = CacheImage::new(filename);

                //load optimized images from cache
                for size in config.sizes.keys() {
                    for extension in &config.extensions {
                        let mut path = PathBuf::from(&config.cache_directory);
                        path.push(&size);
                        path.push(stem);
                        path.set_extension(extension.extensions().first().unwrap());

                        if path.exists() {
                            item.add(size.to_owned(), extension.to_owned(), path);
                        }
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

        //convert unavailable extensions
        for extension in self.config.extensions.iter().filter(|ext| !cache.has(size, **ext)) {
            let _ = self.create_image_tx.send(OptimizeImage {
                image_id: image_id.to_owned(),
                size: size.to_owned(),
                extension: *extension,
            });
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
            let mut path = PathBuf::from(&self.config.root);
            path.push(file);

            if path.exists() {
                return self.read_image(path.to_str().unwrap(), true);
            } else {
                let _ = self.create_image_tx.send(OptimizeImage {
                    image_id: image_id.to_owned(),
                    size: size.to_owned(),
                    extension: appropriate_extension,
                });
            }
        }

        //return the image as is, it will be optimized later
        self.read_image(&cache.base_image_path, false)
    }

    fn read_image(&self, path: &str, is_optimized: bool) -> Result<Option<FetchResult>, Error> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        let format = ImageFormat::from_path(path)?;

        Ok(Some(FetchResult {
            data: FileTransfer::new(file, metadata.len()),
            last_modified: DateTime::from(metadata.modified() ? ),
            inode: metadata.ino(),
            mime: format.to_mime_type(),
            is_optimized,
        }))
    }
}

#[derive(Clone, Debug)]
pub struct CacheImage {
    pub base_image_path: String,
    pub optimized: HashMap<(String, Extension), String>, //associating size and extension to the path
}

impl CacheImage {
    pub fn new(base_image_path: String) -> Self {
        CacheImage {
            base_image_path,
            optimized: HashMap::new(),
        }
    }

    pub fn add<P: AsRef<Path>>(&mut self, size: String, ext: Extension, path: P) {
        self.optimized.insert((size, ext), path.as_ref().to_string_lossy().to_string());
    }

    pub fn get(&self, size: &str, ext: Extension) -> Option<&String> {
        self.optimized.get(&(size.to_string(), ext))
    }

    pub fn has(&self, size: &str, ext: Extension) -> bool {
        self.optimized.contains_key(&(size.to_string(), ext))
    }
}


pub struct FetchResult {
    pub data: FileTransfer,
    pub last_modified: DateTime<Utc>,
    pub inode: u64,
    pub mime: &'static str,
    pub is_optimized: bool,
}
