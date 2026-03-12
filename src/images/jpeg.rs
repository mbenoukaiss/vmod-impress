use image::{DynamicImage, EncodableLayout};
use turbojpeg::{compress, Image, OwnedBuf, PixelFormat, Subsamp};
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
    //replicates turbojpeg::compress_image but without the `image` cargo feature,
    //which would force a specific image-crate version onto our dep tree
    let (pixels, format, width, height) = match image {
        DynamicImage::ImageRgb8(img) => (img.as_raw().as_slice(), PixelFormat::RGB, img.width(), img.height()),
        DynamicImage::ImageRgba8(img) => (img.as_raw().as_slice(), PixelFormat::RGBA, img.width(), img.height()),
        _ => return Error::err("Unsupported image format"),
    };

    let tj_image = Image {
        pixels,
        width: width as usize,
        pitch: format.size() * width as usize,
        height: height as usize,
        format,
    };

    Ok(compress(tj_image, quality as i32, Subsamp::None)?.into())
}
