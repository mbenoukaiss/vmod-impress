use image::{DynamicImage, EncodableLayout};
use turbojpeg::{OwnedBuf, Subsamp};
use crate::error::Error;
use crate::images::OptimizedImage;

pub struct Jpeg {
    data: OwnedBuf,
}

impl OptimizedImage for Jpeg {
    fn data(&self) -> &[u8] {
        self.data.as_bytes()
    }
}

impl From<OwnedBuf> for Jpeg {
    fn from(data: OwnedBuf) -> Self {
        Jpeg { data }
    }
}

pub fn to_jpeg(image: &DynamicImage, quality: f32, _prefer_quality: bool) -> Result<Jpeg, Error> {
    match image {
        DynamicImage::ImageRgb8(image) => Ok(turbojpeg::compress_image(image, quality as i32, Subsamp::None)?.into()),
        DynamicImage::ImageRgba8(image) => Ok(turbojpeg::compress_image(image, quality as i32, Subsamp::None)?.into()),
        _ => Error::err("Unsupported image format"),
    }

}
