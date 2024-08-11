mod avif;
mod webp;
mod jpeg;

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
    let optimized: Box<dyn OptimizedImage> = match config {
        OptimizationConfig::Webp { quality, prefer_quality } => Box::new(webp::to_webp(&image, quality, prefer_quality)?),
        OptimizationConfig::Avif { quality, prefer_quality } => Box::new(avif::to_avif(&image, quality, prefer_quality)?),
        OptimizationConfig::Jpeg { quality, prefer_quality } => Box::new(jpeg::to_jpeg(&image, quality, prefer_quality)?),
    };

    Ok(optimized)
}

pub fn write<T>(path: T, data: &[u8], last_modified: Option<SystemTime>) -> Result<(), Error> where T: AsRef<Path> {
    let directory = path.as_ref().parent().expect("Logic error: file should be in a directory");

    fs::create_dir_all(directory)?;

    let mut file = File::create_new(path)?;
    file.write(data)?;

    if let Some(last_modified) = last_modified {
        file.set_modified(last_modified)?;
    }

    Ok(())
}

pub enum OptimizationConfig {
    Webp { quality: f32, prefer_quality: bool },
    Avif { quality: f32, prefer_quality: bool },
    Jpeg { quality: f32, prefer_quality: bool },
}

pub trait OptimizedImage {
    fn data(&self) -> &[u8];
}
