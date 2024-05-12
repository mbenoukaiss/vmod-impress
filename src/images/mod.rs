mod webp;

use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::SystemTime;
use image::DynamicImage;
use image::imageops::FilterType;
use crate::error::Error;

pub fn read<T>(path: T) -> Result<DynamicImage, Error> where T: AsRef<Path> {
    let image = image::open(path)?;
    if matches!(&image, DynamicImage::ImageRgb8(_)) || matches!(&image, DynamicImage::ImageRgba8(_)) {
        Ok(image)
    } else {
        Ok(DynamicImage::ImageRgba8(image.to_rgba8()))
    }
}

pub fn resize(image: &DynamicImage, width: u32, height: u32) -> DynamicImage {
    image.resize(width, height, FilterType::Lanczos3)
}

pub fn optimize(image: &DynamicImage, config: OptimizationConfig) -> Result<Box<dyn OptimizedImage>, Error> {
    match config {
        OptimizationConfig::Webp { quality } => Ok(Box::new(webp::to_webp(&image, quality))),
    }
}

pub fn supports(format: &str) -> bool {
    match format {
        "webp" => true,
        _ => false,
    }
}

pub fn write<T>(path: T, data: &[u8], last_modified: Option<SystemTime>) -> Result<(), Error> where T: AsRef<Path> {
    fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();

    let mut file = File::create_new(path)?;
    file.write(data)?;

    if let Some(last_modified) = last_modified {
        file.set_modified(last_modified)?;
    }

    Ok(())
}

pub enum OptimizationConfig {
    Webp { quality: f32 },
}

pub trait OptimizedImage {
    fn data(&self) -> &[u8];
}
