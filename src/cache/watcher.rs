use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::{fs, sync, thread};
use std::collections::HashMap;
use itertools::Itertools;
use notify::{Config as NotifyConfig, Error as NotifyError, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use notify::event::{AccessKind, AccessMode, ModifyKind, RemoveKind, RenameMode};
use crate::cache::{CacheData, CacheImage};
use crate::cache::file_saver::CreateImageFile;
use crate::config::Config;
use crate::error::Error;
use crate::images;
use crate::images::OptimizationConfig;

pub fn spawn(config: Config, data: CacheData, create_image_tx: Sender<CreateImageFile>) {
    thread::spawn(move || {
        let (tx, rx) = sync::mpsc::channel();

        let mut watcher = RecommendedWatcher::new(tx, NotifyConfig::default()).unwrap();
        watcher.watch(Path::new(&config.root), RecursiveMode::Recursive).unwrap();

        event_handler(config, data, rx, create_image_tx);
    });
}

fn event_handler(config: Config, data: CacheData, rx: Receiver<Result<Event, NotifyError>>, create_image_tx: Sender<CreateImageFile>) {
    while let Ok(result) = rx.recv() {
        match result {
            Ok(event) => {
                let result = match event.kind {
                    EventKind::Access(AccessKind::Close(AccessMode::Write)) => handle_modification(event, &config, &data, create_image_tx.clone()),
                    EventKind::Remove(RemoveKind::File) => handle_deletion(event, &config, &data),
                    EventKind::Modify(ModifyKind::Name(RenameMode::From)) => handle_deletion(event, &config, &data),
                    EventKind::Modify(ModifyKind::Name(RenameMode::To)) => handle_modification(event, &config, &data, create_image_tx.clone()),
                    _ => Ok(()),
                };

                if let Err(e) = result {
                    error!("watch event handling error: {:?}", e);
                }
            }
            Err(e) => error!("watch error: {:?}", e),
        }
    }
}

fn handle_modification(event: Event, config: &Config, data: &CacheData, create_image_tx: Sender<CreateImageFile>) -> Result<(), Error> {
    let image_path = get_image_path(&event)?;
    let image_id = get_image_id(&image_path, &config);

    let mut lock = data.write().unwrap();

    if !lock.contains_key(&image_id) {
        lock.insert(image_id.clone(), CacheImage::new(image_path.to_owned()));
    }

    if let Some(cache) = lock.get_mut(&image_id) {
        for path in cache.optimized.values() {
            fs::remove_file(path)?;
        }

        cache.optimized = HashMap::new();

        let to_optimize = config.sizes.iter()
            .filter(|(_, size)| size.matches(&image_id) && size.pre_optimize.unwrap_or(false))
            .cartesian_product(config.extensions.iter());

        let base_image = images::read(&cache.base_image_path)?;

        for ((size_name, size), format) in to_optimize {
            let optimization_config = OptimizationConfig::new(&config, size_name, format, true);
            let resized = images::resize(&base_image, size.width, size.height);
            let optimized = images::optimize(&resized, optimization_config)?;

            create_image_tx.send(CreateImageFile {
                image_id: image_id.clone(),
                size: size_name.clone(),
                extension: format.clone(),
                data: optimized.data().to_vec(),
                last_modified: None,
            }).unwrap();
        }
    }

    Ok(())
}

fn handle_deletion(event: Event, config: &Config, data: &CacheData) -> Result<(), Error> {
    let image_path = get_image_path(&event)?;
    let image_id = get_image_id(&image_path, &config);

    let mut lock = data.write().unwrap();

    if let Some(image) = lock.remove(&image_id) {
        for (_, path) in image.optimized {
            fs::remove_file(path)?;
        }
    }

    Ok(())
}

fn get_image_path(event: &Event) -> Result<String, Error> {
    if let Some(path) = event.paths.first() {
        Ok(path.to_string_lossy().to_string())
    } else {
        Error::err("No path in event")
    }
}

fn get_image_id(path: &str, config: &Config) -> String {
    let mut image_id = PathBuf::from(path);
    image_id.set_extension("");

    image_id.strip_prefix(&config.root)
        .unwrap_or_else(|_| image_id.as_path())
        .to_string_lossy()
        .to_string()
}
