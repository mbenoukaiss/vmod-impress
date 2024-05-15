use std::sync::mpsc::Sender;
use std::thread;
use itertools::Itertools;
use crate::cache::CacheData;
use crate::cache::file_saver::CreateImageFile;
use crate::config::{Config, Size};
use crate::images::{self, OptimizationConfig};

pub fn spawn(config: Config, data: CacheData, create_image_tx: Sender<CreateImageFile>) {
    let data = (*data.read().unwrap()).clone();

    thread::spawn(move || {
        let sizes_to_optimize = config.sizes.iter()
            .filter(|(_, size)| size.pre_optimize.unwrap_or(false))
            .cartesian_product(config.extensions.iter())
            .map(|((size_name, size), extension)| (size_name, size, extension))
            .collect::<Vec<(&String, &Size, &String)>>();

        for (size_name, size, extension) in sizes_to_optimize {
            for (image_id, cache) in &data {
                if !size.matches(image_id) {
                    continue;
                }

                if !cache.optimized.contains_key(&(size_name.to_owned(), extension.to_owned())) {
                    let optimization_config = OptimizationConfig::new(&config, size_name, extension, true);

                    let image = images::read(&cache.base_image_path).unwrap();
                    let resized = images::resize(&image, size.width, size.height);
                    let optimized = images::optimize(&resized, optimization_config).unwrap();

                    create_image_tx.send(CreateImageFile {
                        image_id: image_id.to_owned(),
                        size: size_name.clone(),
                        extension: extension.clone(),
                        data: optimized.data().to_vec(),
                        last_modified: None,
                    }).unwrap();
                }
            }
        }
    });
}