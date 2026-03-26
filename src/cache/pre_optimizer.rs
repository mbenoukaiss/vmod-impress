use std::sync::mpsc::Sender;
use std::thread;
use crate::cache::CacheData;
use crate::cache::file_saver::{InFlight, OptimizeJob};
use crate::config::SharedConfig;

pub fn spawn(config: SharedConfig, data: CacheData, create_image_tx: Sender<OptimizeJob>, in_flight: InFlight) {
    //skip the whole machinery if no size opts in — saves a thread + a clone of the cache map
    if !config.sizes.values().any(|s| s.pre_optimize.unwrap_or(false)) {
        return;
    }

    let data = match data.read() {
        Ok(guard) => (*guard).clone(),
        Err(e) => {
            //a poisoned RwLock here means an earlier panic poisoned the cache;
            //the runtime cache still works, only pre-warm is lost
            error!("pre-optimizer skipped: cache lock poisoned ({})", e);
            return;
        }
    };

    thread::spawn(move || {
        let pre_optimize_sizes: Vec<(&String, &crate::config::Size)> = config.sizes.iter()
            .filter(|(_, size)| size.pre_optimize.unwrap_or(false))
            .collect();

        for (image_id, cache) in &data {
            for (size_name, size) in &pre_optimize_sizes {
                if !size.matches(image_id) {
                    continue;
                }

                //one job per (image_id, size) covering all configured extensions
                //that don't already exist on disk
                let missing: Vec<_> = config.extensions.iter()
                    .filter(|ext| !cache.has(size_name, **ext))
                    .copied()
                    .collect();

                if missing.is_empty() {
                    continue;
                }

                let key = (image_id.clone(), (*size_name).clone());
                match in_flight.lock() {
                    Ok(mut guard) => {
                        if !guard.insert(key.clone()) {
                            //another caller already enqueued this (image_id, size)
                            continue;
                        }
                    }
                    Err(_) => continue,
                }

                if let Err(e) = create_image_tx.send(OptimizeJob {
                    image_id: image_id.clone(),
                    size: (*size_name).clone(),
                    extensions: missing,
                }) {
                    //file_saver thread died; release the slot we just claimed and stop
                    error!("pre-optimizer aborting: optimization channel closed ({})", e);
                    if let Ok(mut guard) = in_flight.lock() {
                        guard.remove(&key);
                    }
                    return;
                }
            }
        }
    });
}
