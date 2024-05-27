use image::DynamicImage;
use libavif::{AvifData, AvifImage, Encoder, RgbPixels, YuvFormat};
use crate::images::OptimizedImage;

pub struct Avif {
    data: AvifData<'static>,
}

impl OptimizedImage for Avif {
    fn data(&self) -> &[u8] {
        self.data.as_slice()
    }
}

impl Into<Avif> for AvifData<'static> {
    fn into(self) -> Avif {
        Avif {
            data: self,
        }
    }
}

pub fn to_avif(image: &DynamicImage, quality: f32, prefer_quality: bool) -> Avif {
    let image = {
        let width = image.width();
        let height = image.height();
        let data = image.as_bytes();

        if (width * height) as usize == data.len() {
            AvifImage::from_luma8(width, height, data).unwrap()
        } else {
            RgbPixels::new(width, height, data).unwrap().to_image(YuvFormat::Yuv444)
        }
    };

    Encoder::new()
        .set_quality(quality as u8) //TODO: allow different quality for avif and webp, 40
        .set_alpha_quality(50)
        .set_max_threads(1)
        .set_speed(if prefer_quality { 0 } else { 6 })
        .encode(&image)
        .expect("Failed to encode to AVIF")
        .into()
}
