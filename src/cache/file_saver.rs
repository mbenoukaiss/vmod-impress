use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;
use rusty_pool::ThreadPool;
use crate::cache::CacheData;
use crate::config::{Extension, SharedConfig};
use crate::error::Error;
use crate::images;
use crate::images::OptimizationConfig;

pub type InFlight = Arc<Mutex<HashSet<(String, String)>>>;

/// One unit of optimization work, covering all desired output extensions
/// for a single (image_id, size). The saver reads + resizes the source
/// once, then encodes each extension from the same DynamicImage.
pub struct OptimizeJob {
    pub image_id: String,
    pub size: String,
    pub extensions: Vec<Extension>,
}

pub fn spawn(config: SharedConfig, data: CacheData, in_flight: InFlight, rx: Receiver<OptimizeJob>) {
    let threads = config.pre_optimizer_threads.unwrap_or(1);
    let pool = ThreadPool::new(0, threads, Duration::from_secs(60));

    thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            let task_config = config.clone();
            let task_data = data.clone();
            let task_in_flight = in_flight.clone();

            pool.execute(move || {
                let key = (job.image_id.clone(), job.size.clone());
                let image_id = job.image_id.clone();
                if let Err(error) = run_job(task_config, task_data, job) {
                    error!("Failed to save optimized images {}: {}", image_id, error.to_string());
                }
                //always release in_flight, success or failure — otherwise a
                //permanently-failing image would block future retries
                if let Ok(mut guard) = task_in_flight.lock() {
                    guard.remove(&key);
                }
            })
        }
    });
}

fn run_job(config: SharedConfig, cache: CacheData, job: OptimizeJob) -> Result<(), Error> {
    let Some(size) = config.sizes.get(&job.size) else {
        return Error::err(format!("Unknown image size {}", job.size));
    };

    let base_image_path = {
        let lock = cache.read()?;
        let data = lock.get(&job.image_id).ok_or(Error::new("Image not found"))?;
        data.base_image_path.clone()
    };

    //read + resize ONCE per (image_id, size); encode the same DynamicImage
    //for every requested extension
    let source = images::read(base_image_path)?;
    let resized = images::resize(&source, size.width, size.height);

    for &extension in &job.extensions {
        let mut path = PathBuf::from(&config.cache_directory);
        path.push(&job.size);
        path.push(&job.image_id);
        path.set_extension(extension.extensions().first().expect("Failed to get extension"));

        let optimization_config = OptimizationConfig::new(size, extension, false);
        let optimized = match images::optimize(&resized, optimization_config) {
            Ok(o) => o,
            Err(e) => {
                error!("optimize {}/{}/{:?} failed: {}", job.image_id, job.size, extension, e);
                continue;
            }
        };

        if let Err(e) = images::write(&path, optimized.data(), None) {
            //write_new races: if a concurrent path created the file (e.g.
            //another job saw the same staleness) we still want to record
            //the path; otherwise log and move on
            error!("write {:?} failed: {}", path, e);
            continue;
        }

        if let Ok(mut guard) = cache.write() {
            if let Some(image) = guard.get_mut(&job.image_id) {
                if let Err(e) = image.add(job.size.clone(), extension, &path) {
                    error!("failed to record cache variant {:?}: {}", path, e);
                }
            }
        }
    }

    Ok(())
}
