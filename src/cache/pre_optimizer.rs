use std::sync::mpsc::Sender;
use std::thread;
use itertools::Itertools;
use crate::cache::CacheData;
use crate::cache::file_saver::OptimizeImage;
use crate::config::{Config, Extension, Size};

pub fn spawn(config: Config, data: CacheData, create_image_tx: Sender<OptimizeImage>) {
    let data = (*data.read().unwrap()).clone();

    thread::spawn(move || {
        let sizes_to_optimize = config.sizes.iter()
            .filter(|(_, size)| size.pre_optimize.unwrap_or(false))
            .cartesian_product(config.extensions.iter())
            .map(|((size_name, size), extension)| (size_name, size, extension))
            .collect::<Vec<(&String, &Size, &Extension)>>();

        for (size_name, size, &extension) in sizes_to_optimize {
            for (image_id, cache) in &data {
                if !size.matches(image_id) {
                    continue;
                }

                if !cache.optimized.contains_key(&(size_name.to_owned(), extension)) {
                    create_image_tx.send(OptimizeImage {
                        image_id: image_id.to_owned(),
                        size: size_name.to_owned(),
                        extension,
                    }).unwrap();
                }
            }
        }
    });
}
