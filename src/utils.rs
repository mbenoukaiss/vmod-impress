use std::ffi::OsStr;
use std::path::Path;

pub fn decompose_filename(path: &str) -> (Option<&str>, Option<&str>) {
    let path = Path::new(path);
    let extension = path.extension().and_then(OsStr::to_str);
    let stem = if let Some(extension) = &extension {
        path.to_str().map(|s| &s[0..(s.len() - extension.len() - 1)])
    } else {
        path.to_str()
    };

    (stem, extension)
}