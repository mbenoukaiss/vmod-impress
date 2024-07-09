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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_filename_with_extension() {
        let path = "file.txt";
        let (stem, extension) = decompose_filename(path);
        assert_eq!(stem, Some("file"));
        assert_eq!(extension, Some("txt"));
    }

    #[test]
    fn test_filename_with_multiple_dots() {
        let path = "archive.tar.gz";
        let (stem, extension) = decompose_filename(path);
        assert_eq!(stem, Some("archive.tar"));
        assert_eq!(extension, Some("gz"));
    }

    #[test]
    fn test_filename_with_path_and_multiple_dots() {
        let path = "/test/directory/multiple/archive.tar.gz";
        let (stem, extension) = decompose_filename(path);
        assert_eq!(stem, Some("/test/directory/multiple/archive.tar"));
        assert_eq!(extension, Some("gz"));
    }

    #[test]
    fn test_no_extension() {
        let path = "filename";
        let (stem, extension) = decompose_filename(path);
        assert_eq!(stem, Some("filename"));
        assert_eq!(extension, None);
    }

    #[test]
    fn test_dot_file() {
        let path = ".gitignore";
        let (stem, extension) = decompose_filename(path);
        assert_eq!(stem, Some(".gitignore"));
        assert_eq!(extension, None);
    }

    #[test]
    fn test_empty_string() {
        let path = "";
        let (stem, extension) = decompose_filename(path);
        assert_eq!(stem, Some(""));
        assert_eq!(extension, None);
    }

    #[test]
    fn test_only_extension() {
        let path = ".ext";
        let (stem, extension) = decompose_filename(path);
        assert_eq!(stem, Some(".ext"));
        assert_eq!(extension, None);
    }

    #[test]
    fn test_filename_with_no_stem() {
        let path = ".bashrc";
        let (stem, extension) = decompose_filename(path);
        assert_eq!(stem, Some(".bashrc"));
        assert_eq!(extension, None);
    }

    #[test]
    fn test_path_with_directories() {
        let path = "/path/to/file.txt";
        let (stem, extension) = decompose_filename(path);
        assert_eq!(stem, Some("/path/to/file"));
        assert_eq!(extension, Some("txt"));
    }
}
