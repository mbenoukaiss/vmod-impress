use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::{fs, mem, sync, thread};
use std::collections::HashMap;
use image::ImageFormat;
use notify::{Config as NotifyConfig, Error as NotifyError, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use notify::event::{AccessKind, AccessMode, ModifyKind, RemoveKind, RenameMode};
use crate::cache::{CacheData, CacheImage};
use crate::cache::file_saver::{InFlight, OptimizeJob};
use crate::config::{Config, SharedConfig};
use crate::error::Error;

pub fn spawn(config: SharedConfig, data: CacheData, create_image_tx: Sender<OptimizeJob>, in_flight: InFlight) {
    thread::spawn(move || {
        let (tx, rx) = sync::mpsc::channel();

        let mut watcher = match RecommendedWatcher::new(tx, NotifyConfig::default()) {
            Ok(w) => w,
            Err(e) => {
                error!("failed to create file watcher: {}; cache will not auto-invalidate on source changes", e);
                return;
            }
        };

        for root in &config.roots {
            if let Err(e) = watcher.watch(Path::new(root), RecursiveMode::Recursive) {
                //one bad root must not prevent watching the others
                error!("failed to watch root {:?}: {}", root, e);
            }
        }

        event_handler(config, data, rx, create_image_tx, in_flight);
    });
}

fn event_handler(config: SharedConfig, data: CacheData, rx: Receiver<Result<Event, NotifyError>>, create_image_tx: Sender<OptimizeJob>, in_flight: InFlight) {
    while let Ok(result) = rx.recv() {
        match result {
            Ok(event) => {
                let result = match event.kind {
                    EventKind::Access(AccessKind::Close(AccessMode::Write)) => handle_modification(event, &config, &data, &create_image_tx, &in_flight),
                    EventKind::Remove(RemoveKind::File) => handle_deletion(event, &config, &data),
                    EventKind::Modify(ModifyKind::Name(RenameMode::From)) => handle_deletion(event, &config, &data),
                    EventKind::Modify(ModifyKind::Name(RenameMode::To)) => handle_modification(event, &config, &data, &create_image_tx, &in_flight),
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

fn handle_modification(event: Event, config: &Config, data: &CacheData, create_image_tx: &Sender<OptimizeJob>, in_flight: &InFlight) -> Result<(), Error> {
    let image_path = get_image_path(&event)?;
    let image_id = get_image_id(&image_path, config);

    let to_delete = {
        let mut lock = data.write()?;

        if !lock.contains_key(&image_id) {
            let mime = ImageFormat::from_path(&image_path)
                .map(|f| f.to_mime_type())
                .unwrap_or("application/octet-stream");
            lock.insert(image_id.to_string(), CacheImage::new(image_path.to_owned(), mime));
        }

        if let Some(cache) = lock.get_mut(&image_id) {
            mem::take(&mut cache.optimized)
        } else {
            HashMap::new()
        }
    };

    for path in to_delete.values() {
        fs::remove_file(path)?;
    }

    //one OptimizeJob per (image_id, size) covering all configured extensions —
    //saver reads + resizes the source once for the whole job
    for (size_name, _size) in config.sizes.iter()
        .filter(|(_, size)| size.matches(&image_id) && size.pre_optimize.unwrap_or(false))
    {
        let key = (image_id.clone(), size_name.clone());
        match in_flight.lock() {
            Ok(mut guard) => {
                if !guard.insert(key.clone()) {
                    continue;
                }
            }
            Err(_) => continue,
        }

        if let Err(e) = create_image_tx.send(OptimizeJob {
            image_id: image_id.clone(),
            size: size_name.clone(),
            extensions: config.extensions.clone(),
        }) {
            error!("watcher: optimize channel closed: {}", e);
            if let Ok(mut guard) = in_flight.lock() {
                guard.remove(&key);
            }
            return Ok(());
        }
    }

    Ok(())
}

fn handle_deletion(event: Event, config: &Config, data: &CacheData) -> Result<(), Error> {
    let image_path = get_image_path(&event)?;
    let image_id = get_image_id(&image_path, config);

    let image = data.write()?.remove(&image_id);

    if let Some(image) = image {
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

   config.roots.iter()
       .fold(image_id.as_path(), |acc, root| acc.strip_prefix(root).unwrap_or(acc))
       .to_string_lossy()
       .to_string()
}
