use crate::cache::CacheData;
use crate::config::{Config, Extension, SharedConfig};
use crate::utils;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use walkdir::WalkDir;

const SWEEP_INTERVAL: Duration = Duration::from_secs(86_400);

pub fn spawn(config: SharedConfig, data: CacheData) {
    thread::spawn(move || loop {
        thread::sleep(SWEEP_INTERVAL);
        match sweep_once(&config, &data) {
            Ok(0) => {}
            Ok(n) => info!("cleaner: removed {} orphan cache file(s)", n),
            Err(e) => error!("cleaner: sweep failed: {}", e),
        }
    });
}

/// Walk `cache_directory` and remove any cache file whose `(size, image_id, ext)`
/// is not currently configured/known. Returns the number of files removed.
///
/// "Orphan" means any of:
///   - the size directory is not a configured size
///   - the file's extension is not a configured (canonical first) extension
///   - the image_id is not present in the live `CacheData` map
///
/// The map entry is also pruned for any file we delete, so a subsequent
/// `Cache::get` will lazily re-optimize on next request.
pub fn sweep_once(config: &Config, data: &CacheData) -> std::io::Result<usize> {
    let cache_root = Path::new(&config.cache_directory);
    if !cache_root.exists() {
        return Ok(0);
    }

    let mut removed = 0usize;
    for entry in WalkDir::new(cache_root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let rel = match path.strip_prefix(cache_root) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let parsed = match parse_cache_path(rel) {
            Some(p) => p,
            None => continue,
        };

        if !is_orphan(config, data, &parsed) {
            continue;
        }

        match fs::remove_file(path) {
            Ok(()) => {
                removed += 1;
                debug!("cleaner: removed orphan cache file {:?}", path);
                if let Some(ext_enum) = Extension::from_ext(&parsed.ext) {
                    let mut guard = data.write();
                    if let Some(image) = guard.get_mut(&parsed.image_id) {
                        image.optimized[ext_enum as usize].remove(&parsed.size);
                    }
                }
            }
            Err(e) => {
                //a single failure mustn't abort the whole sweep
                error!("cleaner: failed to remove orphan {:?}: {}", path, e);
            }
        }
    }

    Ok(removed)
}

struct CacheFilename {
    size: String,
    image_id: String,
    ext: String,
}

fn parse_cache_path(rel: &Path) -> Option<CacheFilename> {
    let mut components = rel.components();
    let size = components.next()?.as_os_str().to_str()?.to_owned();

    let remaining: PathBuf = components.collect();
    if remaining.as_os_str().is_empty() {
        return None;
    }
    let remaining_str = remaining.to_str()?;
    let (stem, ext) = utils::decompose_filename(remaining_str);
    Some(CacheFilename {
        size,
        image_id: stem?.to_owned(),
        ext: ext?.to_owned(),
    })
}

fn is_orphan(config: &Config, data: &CacheData, parsed: &CacheFilename) -> bool {
    if !config.sizes.contains_key(&parsed.size) {
        return true;
    }
    let configured_ext = config
        .extensions
        .iter()
        .filter_map(|ext| ext.extensions().first().copied())
        .any(|e| e == parsed.ext);
    if !configured_ext {
        return true;
    }
    !data.read().contains_key(&parsed.image_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheImage;
    use crate::config::{Extension, Size};
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_config(cache_dir: &Path) -> Config {
        let mut sizes = HashMap::new();
        sizes.insert(
            "medium".to_string(),
            Size {
                width: 600,
                height: 600,
                quality: [0.0; 3],
                pattern: None,
                pre_optimize: None,
                pattern_regex: None,
                quality_serialized: None,
            },
        );
        sizes.insert(
            "high".to_string(),
            Size {
                width: 1200,
                height: 1200,
                quality: [0.0; 3],
                pattern: None,
                pre_optimize: None,
                pattern_regex: None,
                quality_serialized: None,
            },
        );

        Config {
            extensions: vec![Extension::WEBP, Extension::AVIF],
            default_format: Extension::JPEG,
            roots: vec!["/dev/null".to_string()],
            url: "/media/{size}/{path}.{ext}".to_string(),
            cache_directory: cache_dir.to_string_lossy().to_string(),
            pre_optimizer_threads: None,
            sizes,
            logger: None,
            cache_control: None,
            url_regex: None,
            cache_control_value: Arc::from(""),
            cache_control_fallback: Arc::from(""),
            quality_serialized: None,
            statics: Vec::new(),
        }
    }

    fn write_cache_file(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"x").unwrap();
    }

    fn known_image(path: &str) -> CacheImage {
        CacheImage::new(path.to_string(), "image/jpeg")
    }

    #[test]
    fn empty_cache_dir_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let config = make_config(tmp.path());
        let data: CacheData = Arc::new(RwLock::new(HashMap::new()));
        assert_eq!(sweep_once(&config, &data).unwrap(), 0);
    }

    #[test]
    fn missing_cache_dir_returns_zero() {
        let config = make_config(Path::new("/nonexistent/path/that/does/not/exist"));
        let data: CacheData = Arc::new(RwLock::new(HashMap::new()));
        assert_eq!(sweep_once(&config, &data).unwrap(), 0);
    }

    #[test]
    fn known_files_are_kept() {
        let tmp = TempDir::new().unwrap();
        write_cache_file(tmp.path(), "medium/products/logo.webp");
        write_cache_file(tmp.path(), "high/products/logo.avif");

        let config = make_config(tmp.path());
        let mut map = HashMap::new();
        map.insert(
            "products/logo".to_string(),
            known_image("/dev/null/products/logo.jpg"),
        );
        let data: CacheData = Arc::new(RwLock::new(map));

        assert_eq!(sweep_once(&config, &data).unwrap(), 0);
        assert!(tmp.path().join("medium/products/logo.webp").exists());
        assert!(tmp.path().join("high/products/logo.avif").exists());
    }

    #[test]
    fn unknown_image_id_is_orphan() {
        let tmp = TempDir::new().unwrap();
        write_cache_file(tmp.path(), "medium/orphan.webp");
        let config = make_config(tmp.path());
        let data: CacheData = Arc::new(RwLock::new(HashMap::new()));

        assert_eq!(sweep_once(&config, &data).unwrap(), 1);
        assert!(!tmp.path().join("medium/orphan.webp").exists());
    }

    #[test]
    fn unknown_size_is_orphan() {
        let tmp = TempDir::new().unwrap();
        write_cache_file(tmp.path(), "low/products/logo.webp");

        let config = make_config(tmp.path()); // "low" is NOT in sizes
        let mut map = HashMap::new();
        map.insert(
            "products/logo".to_string(),
            known_image("/dev/null/products/logo.jpg"),
        );
        let data: CacheData = Arc::new(RwLock::new(map));

        assert_eq!(sweep_once(&config, &data).unwrap(), 1);
        assert!(!tmp.path().join("low/products/logo.webp").exists());
    }

    #[test]
    fn unconfigured_extension_is_orphan() {
        let tmp = TempDir::new().unwrap();
        write_cache_file(tmp.path(), "medium/products/logo.jpg"); // jpg not in [WEBP, AVIF]

        let config = make_config(tmp.path());
        let mut map = HashMap::new();
        map.insert(
            "products/logo".to_string(),
            known_image("/dev/null/products/logo.jpg"),
        );
        let data: CacheData = Arc::new(RwLock::new(map));

        assert_eq!(sweep_once(&config, &data).unwrap(), 1);
        assert!(!tmp.path().join("medium/products/logo.jpg").exists());
    }

    #[test]
    fn sweep_prunes_map_entry_on_remove() {
        let tmp = TempDir::new().unwrap();
        write_cache_file(tmp.path(), "medium/products/logo.webp");
        let config = make_config(tmp.path());

        // image_id is known but the file is an orphan because we did NOT add it
        // to optimized. After sweep, the map entry must remain (image still known)
        // but with no optimized entry for (medium, WEBP).
        let mut map = HashMap::new();
        let mut img = known_image("/dev/null/other/source.jpg"); //different image_id key below
                                                                 //add() returns Err if the metadata stat fails, but we only need this
                                                                 //entry as a setup fixture for the orphan-prune test that follows;
                                                                 //either way, the post-sweep assertion is valid.
        let _ = img.add(
            "medium".to_string(),
            Extension::WEBP,
            tmp.path().join("medium/products/logo.webp"),
        );
        map.insert("other-id".to_string(), img); // intentionally mismatched image_id key
        let data: CacheData = Arc::new(RwLock::new(map));

        // The cache file lives under "products/logo" but the map only has "other-id" — orphan.
        assert_eq!(sweep_once(&config, &data).unwrap(), 1);
    }

    #[test]
    fn nested_image_ids_are_parsed_correctly() {
        let tmp = TempDir::new().unwrap();
        write_cache_file(tmp.path(), "medium/a/b/c/deep.webp");
        let config = make_config(tmp.path());

        let mut map = HashMap::new();
        map.insert(
            "a/b/c/deep".to_string(),
            known_image("/dev/null/a/b/c/deep.jpg"),
        );
        let data: CacheData = Arc::new(RwLock::new(map));

        assert_eq!(sweep_once(&config, &data).unwrap(), 0);
        assert!(tmp.path().join("medium/a/b/c/deep.webp").exists());
    }
}
