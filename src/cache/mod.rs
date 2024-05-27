mod file_saver;
mod pre_optimizer;
mod watcher;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc, RwLock};
use std::sync::mpsc::Sender;
use chrono::{DateTime, Utc};
use image::ImageFormat;
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

        Self::load_images(&config, data.clone());
        file_saver::spawn(config.clone(), data.clone(), rx);
        watcher::spawn(config.clone(), data.clone(), tx.clone());
        pre_optimizer::spawn(config.clone(), data.clone(), tx.clone());

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

    pub fn get(&self, image_id: &str, size: &str, supported_extensions: Vec<Extension>) -> Result<Option<(FileTransfer, DateTime<Utc>, &'static str)>, Error> {
        let lock = self.data.read()?;
        let Some(cache) = lock.get(image_id) else {
            return Ok(None);
        };

        for ext in supported_extensions {
            if let Some(file) = cache.get(size, ext) {
                let mut path = PathBuf::from(&self.config.root);
                path.push(file);

                if path.exists() {
                    return self.read_image(path.to_str().unwrap());
                } else {
                    let _ = self.create_image_tx.send(OptimizeImage {
                        image_id: image_id.to_owned(),
                        size: size.to_owned(),
                        extension: ext,
                    });
                }
            } else {
                let _ = self.create_image_tx.send(OptimizeImage {
                    image_id: image_id.to_owned(),
                    size: size.to_owned(),
                    extension: ext,
                });
            }
        }

        //return the image as is, it will be optimized later
        self.read_image(&cache.base_image_path)
    }

    fn read_image(&self, path: &str) -> Result<Option<(FileTransfer, DateTime<Utc>, &'static str)>, Error> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        let format = ImageFormat::from_path(path)?;

        Ok(Some((
            FileTransfer::new(file, metadata.len()),
            DateTime::from(metadata.modified()?),
            format.to_mime_type(),
        )))
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
}
