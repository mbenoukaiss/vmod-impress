use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::SystemTime;
use crate::cache::CacheData;
use crate::config::{Config, Extension};
use crate::error::Error;
use crate::images;

pub struct CreateImageFile {
    pub image_id: String,
    pub size: String,
    pub extension: Extension,
    pub data: Vec<u8>,
    pub last_modified: Option<SystemTime>,
}

pub fn spawn(config: Config, data: CacheData, rx: Receiver<CreateImageFile>) {
    thread::spawn(move || {
        while let Ok(image) = rx.recv() {
            let image_id = image.image_id.clone();
            if let Err(error) = save_image(&config, &data, image) {
                error!("Failed to save optimized images {}: {}", image_id, error.to_string());
            }
        }
    });
}

fn save_image(config: &Config, cache: &CacheData, image: CreateImageFile) -> Result<(), Error> {
    let mut path = PathBuf::from(&config.cache_directory);
    path.push(&image.size);
    path.push(&image.image_id);
    path.set_extension(image.extension.extensions().first().unwrap());

    images::write(&path, &image.data, image.last_modified)?;

    let mut lock = cache.write().unwrap();
    let cache = lock.get_mut(&image.image_id).unwrap();
    cache.add(image.size, image.extension, &path);

    Ok(())
}
