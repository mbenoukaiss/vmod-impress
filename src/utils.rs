use std::ffi::OsStr;
use std::path::Path;

pub fn decompose_filename(path: &str) -> (Option<&str>, Option<&str>) {
    let path = Path::new(path);
    let stem = path.file_stem().and_then(OsStr::to_str);
    let extension = path.extension().and_then(OsStr::to_str);

    (stem, extension)
}