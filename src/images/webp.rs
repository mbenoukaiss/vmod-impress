use std::ffi::c_int;
use image::DynamicImage;
use webp::{Encoder, WebPConfig, WebPMemory};
use crate::error::Error;
use crate::images::OptimizedImage;

pub struct Webp {
    data: WebPMemory,
}

impl OptimizedImage for Webp {
    fn data(&self) -> &[u8] {
        self.data.as_ref()
    }
}

impl Into<Webp> for WebPMemory {
    fn into(self) -> Webp {
        Webp {
            data: self,
        }
    }
}

pub fn to_webp(image: &DynamicImage, quality: f32, autofilter: bool) -> Result<Webp, Error> {
    let mut config = WebPConfig::new().map_err(|_| Error::new("Failed to create webp config"))?;
    config.quality = quality;
    config.lossless = 0;
    config.alpha_quality = 50;
    config.alpha_compression = 1;
    config.alpha_filtering = 0;
    config.autofilter = autofilter as c_int;
    config.filter_sharpness = 4;
    config.filter_strength = 50;
    config.filter_type = 0;
    config.use_sharp_yuv = 0;
    config.method = 3;

    Encoder::from_image(image)
        .expect("Unsupported format")
        .encode_advanced(&config)
        .map(Into::into)
        .map_err(|err| Error::new(format!("Failed to create webp config, got code {}", err as i32)))
}
