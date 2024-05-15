mod file_saver;
mod pre_optimizer;
mod watcher;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc, RwLock};
use std::sync::mpsc::Sender;
use chrono::{DateTime, Utc};
use image::ImageFormat;
use walkdir::WalkDir;
use crate::backend::FileTransfer;
use crate::cache::file_saver::CreateImageFile;
use crate::config::Config;
use crate::error::Error;
use crate::images;
use crate::images::OptimizationConfig;
use crate::utils;

pub type CacheData = Arc<RwLock<HashMap<String, CacheImage>>>;

pub struct Cache {
    config: Config,
    data: CacheData,
    create_image_tx: Sender<CreateImageFile>,
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
                        path.set_extension(extension);

                        if path.exists() {
                            item.add(size.to_owned(), extension.to_owned(), path);
                        }
                    }
                }

                lock.insert(stem.to_owned(), item);
            }
        }
    }

    pub fn get(&self, image_id: &str, size: &str, ext: &str) -> Result<Option<(FileTransfer, DateTime<Utc>)>, Error> {
        let lock = self.data.read()?;
        let Some(cache) = lock.get(image_id) else {
            return Ok(None);
        };


        if let Some(file) = cache.get(size, ext) {
            let mut path = PathBuf::from(&self.config.root);
            path.push(file);

            if let Ok(file) = File::open(path) {
                self.read_image(file)
            } else {
                self.convert_image(cache, image_id, size, ext)
            }
        } else {
            self.convert_image(cache, image_id, size, ext)
        }
    }

    fn read_image(&self, file: File) -> Result<Option<(FileTransfer, DateTime<Utc>)>, Error> {
        let metadata = file.metadata()?;

        Ok(Some((
            FileTransfer::File(BufReader::new(file), metadata.len() as usize),
            DateTime::from(metadata.modified()?),
        )))
    }

    fn convert_image(&self, cache: &CacheImage, image_id: &str, size: &str, ext: &str) -> Result<Option<(FileTransfer, DateTime<Utc>)>, Error> {
        let image = images::read(&cache.base_image_path)?;

        let Some(format) = self.config.sizes.get(size) else {
            return Error::err("Size not found in config");
        };

        let optimization_config = OptimizationConfig::new(&self.config, size, ext, false);
        let image = images::resize(&image, format.width, format.height);
        let optimized = images::optimize(&image, optimization_config)?;
        let modified = Utc::now();

        //if this fails, the images saving thread has crashed, images that have never
        //been loaded will have poor performances but continue serving images on the fly
        let _ = self.create_image_tx.send(CreateImageFile {
            image_id: image_id.to_owned(),
            size: size.to_owned(),
            extension: ext.to_owned(),
            data: optimized.data().to_vec(),
            last_modified: Some(modified.into()),
        });

        Ok(Some((FileTransfer::Memory(optimized), modified)))
    }
}

#[derive(Clone, Debug)]
pub struct CacheImage {
    pub base_image_path: String,
    pub optimized: HashMap<(String, String), String>, //associating size and extension to the path
}

impl CacheImage {
    pub fn new(base_image_path: String) -> Self {
        CacheImage {
            base_image_path,
            optimized: HashMap::new(),
        }
    }

    pub fn add<P: AsRef<Path>>(&mut self, size: String, ext: String, path: P) {
        self.optimized.insert((size, ext), path.as_ref().to_string_lossy().to_string());
    }

    pub fn get(&self, size: &str, ext: &str) -> Option<&String> {
        self.optimized.get(&(size.to_string(), ext.to_string()))
    }
}
