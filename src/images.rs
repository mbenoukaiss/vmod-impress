use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::Read;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, mpsc, RwLock};
use std::sync::mpsc::{Receiver, Sender};
use std::{fs, thread};
use std::thread::JoinHandle;
use image::{DynamicImage, ImageFormat};
use image::imageops::FilterType;
use walkdir::WalkDir;
use webp::{Encoder, WebPConfig};
use crate::config::Config;
use crate::error::Error;
use crate::utils;

pub struct Cache {
    config: Config,
    data: Arc<RwLock<HashMap<String, CacheImage>>>,
    save_queue: Sender<(String, String, String, Vec<u8>)>,
}

impl Cache {
    pub fn new(config: &Config) -> Self {
        let (tx, rx) = mpsc::channel();
        let data = Arc::new(RwLock::new(Cache::load_images(&config)));

        Cache::spawn_worker_thread(config.clone(), data.clone(), rx);

        Cache {
            config: config.clone(),
            data,
            save_queue: tx,
        }
    }

    fn load_images(config: &Config) -> HashMap<String, CacheImage> {
        let mut output = HashMap::new();

        let supported_extensions = ImageFormat::all()
            .flat_map(ImageFormat::extensions_str)
            .map(Deref::deref)
            .collect::<HashSet<&str>>();

        let files = WalkDir::new(&config.root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| !e.file_type().is_dir());

        for file in files {
            let filename = String::from(file.file_name().to_string_lossy());

            if let (Some(stem), Some(extension)) = utils::decompose_filename(&filename) {
                if !supported_extensions.contains(extension) {
                    continue;
                }

                let mut item = CacheImage::new(filename.to_owned());

                //load optimized images from cache
                for size in config.sizes.keys() {
                    for extension in &config.formats {
                        let mut path = PathBuf::from(&config.cache_directory);
                        path.push(&size);
                        path.push(stem);
                        path.set_extension(extension);

                        if path.exists() {
                            item.add(size.to_owned(), extension.to_owned(), path.to_str().unwrap().to_owned());
                        }
                    }
                }

                output.insert(stem.to_owned(), item);
            }
        }

        output
    }

    fn spawn_worker_thread(
        config: Config,
        data: Arc<RwLock<HashMap<String, CacheImage>>>,
        rx: Receiver<(String, String, String, Vec<u8>)>,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            while let Ok((base_image, size, ext, image_data)) = rx.recv() {
                let mut lock = data.write().unwrap();
                let cache = lock.get_mut(&base_image).unwrap();
                let mut path = PathBuf::from(&config.cache_directory);
                path.push(&size);

                fs::create_dir_all(&path).unwrap();

                path.push(&base_image);
                path.set_extension(&ext);

                cache.add(size, ext, path.to_str().unwrap().to_owned());

                fs::write(path, image_data).unwrap();
            }
        })
    }

    pub fn get(&self, image_id: &str, size: &str, ext: &str) -> Result<Option<(Vec<u8>, Option<Metadata>)>, Error> {
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

    fn read_image(&self, mut file: File) -> Result<Option<(Vec<u8>, Option<Metadata>)>, Error> {
        let metadata = file.metadata()?;
        let mut buffer = vec![0; metadata.len() as usize];
        file.read(&mut buffer).expect("buffer overflow");

        Ok(Some((buffer, Some(metadata))))
    }

    fn convert_image(&self, cache: &CacheImage, image_id: &str, size: &str, ext: &str) -> Result<Option<(Vec<u8>, Option<Metadata>)>, Error> {
        let mut path = PathBuf::from(&self.config.root);
        path.push(&cache.base_image);

        let image = read_image(path)?;
        if ext != "webp" {
            panic!("Unsupported format");
        }

        let Some(format) = self.config.sizes.get(size) else {
            return Error::err("Size not found in config");
        };

        let image = resize_image(image, format.width, format.height);
        let webp = convert_webp(&image, format.quality.unwrap_or(self.config.default_quality));

        //if this fails, the image saving thread has crashed, images that have never
        //been loaded will have poor performances but continue serving images on the fly
        //TODO: restore thread ?
        let _ = self.save_queue.send((
            image_id.to_owned(),
            size.to_owned(),
            ext.to_owned(),
            webp.clone(),
        ));

        Ok(Some((webp, None)))
    }
}

#[derive(Debug)]
pub struct CacheImage {
    pub base_image: String,
    pub optimized: HashMap<(String, String), String>,
}

impl CacheImage {
    pub fn new(base_image: String) -> Self {
        CacheImage {
            base_image,
            optimized: HashMap::new(),
        }
    }

    pub fn add(&mut self, size: String, ext: String, path: String) {
        self.optimized.insert((size, ext), path);
    }

    pub fn get(&self, size: &str, ext: &str) -> Option<&String> {
        self.optimized.get(&(size.to_string(), ext.to_string()))
    }
}

fn read_image(path: PathBuf) -> Result<DynamicImage, Error> {
    let image = image::open(path)?;
    if matches!(&image, DynamicImage::ImageRgb8(_)) || matches!(&image, DynamicImage::ImageRgba8(_)) {
        Ok(image)
    } else {
        Ok(DynamicImage::ImageRgba8(image.to_rgba8()))
    }
}

fn resize_image(image: DynamicImage, width: u32, height: u32) -> DynamicImage {
    image.resize(width, height, FilterType::Triangle)
}

fn convert_webp(image: &DynamicImage, quality: f32) -> Vec<u8> {
    //TODO: check libwebp configs to find the best compromise
    let mut config = WebPConfig::new().unwrap();
    config.quality = quality;
    config.lossless = 0;
    config.alpha_quality = 0;
    config.alpha_compression = 0;
    config.alpha_filtering = 0;
    config.autofilter = 0;
    config.filter_sharpness = 0;
    config.filter_strength = 0;
    config.filter_type = 0;
    config.use_sharp_yuv = 0;
    config.method = 3;

    Encoder::from_image(image)
        .expect("Unsupported format")
        .encode_advanced(&config)
        .unwrap()
        .to_vec()
}
