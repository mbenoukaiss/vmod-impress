use std::time::Instant;
use image::DynamicImage;
use image::imageops::FilterType;
use webp::{Encoder, WebPConfig};
use crate::error::Error;

mod error;

fn main() -> Result<(), Error> {
    let now = Instant::now();
    let image = read_image("media/photo.jpeg")?;
    let image = resize_image(image, 1000, 500);
    let webp = convert_webp(&image, 75.0);
    let elapsed_time = now.elapsed();
    println!("Running it took {:?} seconds.", elapsed_time);

    save_webp("cache/photo.webp", webp);
    Ok(())
}

fn read_image(path: &str) -> Result<DynamicImage, Error> {
    let image = image::open(path)?;
    if matches!(&image, DynamicImage::ImageRgb8(_)) || matches!(&image, DynamicImage::ImageRgba8(_)) {
        Ok(image)
    } else {
        Ok(DynamicImage::ImageRgba8(image.to_rgba8()))
    }
}

fn resize_image(image: DynamicImage, width: u32, height: u32) -> DynamicImage {
    image.resize(width, height, FilterType::Triangle)
}

fn convert_webp(image: &DynamicImage, quality: f32) -> Vec<u8> {
    //TODO: check libwebp configs to find the best compromise
    let mut config = WebPConfig::new().unwrap();
    config.quality = quality;
    config.lossless = 0;
    config.alpha_quality = 0;
    config.alpha_compression = 0;
    config.alpha_filtering = 0;
    config.autofilter = 0;
    config.filter_sharpness = 0;
    config.filter_strength = 0;
    config.filter_type = 0;
    config.use_sharp_yuv = 0;
    config.method = 3;

    Encoder::from_image(image)
        .expect("Unsupported format")
        .encode_advanced(&config)
        .unwrap()
        .to_vec()
}

fn save_webp(path: &str, data: Vec<u8>) {
    std::fs::write(path, &*data).unwrap();
}
