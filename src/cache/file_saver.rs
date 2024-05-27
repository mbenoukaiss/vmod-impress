use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::{Duration, SystemTime};
use rusty_pool::ThreadPool;
use crate::cache::CacheData;
use crate::config::{Config, Extension};
use crate::error::Error;
use crate::images;
use crate::images::OptimizationConfig;

pub struct OptimizeImage {
    pub image_id: String,
    pub size: String,
    pub extension: Extension,
}

pub fn spawn(config: Config, data: CacheData, rx: Receiver<OptimizeImage>) {
    let threads = config.pre_optimizer_threads.unwrap_or(1);
    let pool = ThreadPool::new(0, threads, Duration::from_secs(60));

    thread::spawn(move || {
        while let Ok(image) = rx.recv() {
            pool.execute(|| {
                let image_id = image.image_id.clone();
                if let Err(error) = save_image(&config, &data, image) {
                    error!("Failed to save optimized images {}: {}", image_id, error.to_string());
                }
            })
        }
    });
}

fn save_image(config: &Config, cache: &CacheData, image: OptimizeImage) -> Result<(), Error> {
    let mut path = PathBuf::from(&config.cache_directory);
    path.push(&image.size);
    path.push(&image.image_id);
    path.set_extension(image.extension.extensions().first().unwrap());

    let Some(size) = config.sizes.get(&image.size) else {
        return Error::err(format!("Unknown image size {}", image.size))
    };

    let base_image_path = {
        let lock = cache.read()?;
        let data = lock.get(&image.image_id).ok_or(Error::err("Image not found"))?;

        data.base_image_path.clone()
    };

    let optimization_config = OptimizationConfig::new(size, image.extension, false);
    let optimized = images::read(&base_image_path)?;
    let optimized = images::resize(&optimized, size.width, size.height);
    let optimized = images::optimize(&optimized, optimization_config)?;

    images::write(&path, &optimized.data(), None)?;

    let mut lock = cache.write().unwrap();
    let cache = lock.get_mut(&image.image_id).unwrap();
    cache.add(image.size, image.extension, &path);

    Ok(())
}
