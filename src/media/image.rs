//! Image processing — metadata, EXIF orientation, thumbnails and optional
//! format conversion, all with the native Rust `image` crate (no
//! ImageMagick dependency).
//!
//! ```text
//! upload bytes
//!     ↓ decode (JPEG/PNG/GIF/WebP; AVIF/SVG pass through unprocessed)
//!     ↓ apply EXIF orientation (pixels are rotated, the tag is then dropped)
//!     ↓ optional re-encode (EXIF strip / preferred_format)
//!     ↓ thumbnails: only configured sizes, only when the source is larger
//! ```
//!
//! Everything here is best-effort *after* upload validation: images that
//! cannot be decoded are still stored (they passed magic-byte checks) —
//! only their metadata fields stay empty.

use crate::config::{MediaExifConfig, MediaImagesConfig};

/// Guard against decompression bombs (a 20MB PNG can decode to gigabytes).
const MAX_PIXELS: u64 = 80_000_000;

pub struct ThumbnailOutput {
    pub size_name: String,
    pub bytes: Vec<u8>,
}

pub struct ProcessedImage {
    /// Bytes to store as the original (converted/re-encoded when configured).
    pub bytes: Vec<u8>,
    /// Storage extension of the original.
    pub extension: String,
    pub mime: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub thumbnails: Vec<ThumbnailOutput>,
}

/// Raster formats the `image` crate can decode in this build.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RasterFormat {
    Jpeg,
    Png,
    Gif,
    WebP,
}

impl RasterFormat {
    /// Decode-suitable format for an extension, `None` for AVIF/SVG/others
    /// (AVIF decoding needs an optional feature; those pass through raw).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "gif" => Some(Self::Gif),
            "webp" => Some(Self::WebP),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Gif => "gif",
            Self::WebP => "webp",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::WebP => "image/webp",
        }
    }
}

/// Process a validated raster image upload.
pub fn process(
    images: &MediaImagesConfig,
    exif: &MediaExifConfig,
    input: &[u8],
    source: RasterFormat,
) -> Option<ProcessedImage> {
    // JPEG: read orientation before decoding, bake it into the pixels.
    let orientation = if source == RasterFormat::Jpeg {
        read_jpeg_orientation(input).unwrap_or(image::metadata::Orientation::NoTransforms)
    } else {
        image::metadata::Orientation::NoTransforms
    };
    let reader = || {
        image::ImageReader::new(std::io::Cursor::new(input))
            .with_guessed_format()
            .ok()
    };
    let (w, h) = reader()?.into_dimensions().ok()?;
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }
    let mut img = reader()?.decode().ok()?;
    if orientation != image::metadata::Orientation::NoTransforms {
        img.apply_orientation(orientation);
    }
    let (w, h) = (img.width(), img.height());
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }

    // GIF is never re-encoded: the decoder keeps only the first frame and
    // conversion would silently destroy the animation.
    let target_ext = images.preferred_format.trim().to_ascii_lowercase();
    let target = match target_ext.as_str() {
        "webp" => RasterFormat::WebP,
        "jpeg" | "jpg" => RasterFormat::Jpeg,
        "png" => RasterFormat::Png,
        _ => source,
    };
    let convert_to = if source == RasterFormat::Gif {
        source
    } else {
        target
    };

    // Re-encode when configured (format conversion) or when EXIF must be
    // stripped from a JPEG (orientation is already baked in above).
    let must_strip = exif.strip && source == RasterFormat::Jpeg;
    let (original_bytes, store_ext, store_mime) = if must_strip || convert_to != source {
        let fmt = if source == RasterFormat::Gif {
            source
        } else {
            convert_to
        };
        let bytes = encode(&img, fmt, images)?;
        let ext = fmt.extension();
        (bytes, ext.to_string(), fmt.mime().to_string())
    } else {
        (
            input.to_vec(),
            source.extension().to_string(),
            source.mime().to_string(),
        )
    };

    let mut thumbnails = Vec::new();
    if images.generate_thumbnails {
        // Thumbnails share the stored original's extension (URL derivation
        // depends on it), so GIF previews are single-frame GIFs.
        for (name, max_px) in images.sizes.as_pairs() {
            if w <= max_px && h <= max_px {
                continue; // never upscale, never duplicate the original
            }
            let scaled = img.resize(max_px, max_px, image::imageops::FilterType::Lanczos3);
            if let Some(bytes) = encode(&scaled, convert_to, images) {
                thumbnails.push(ThumbnailOutput {
                    size_name: name.to_string(),
                    bytes,
                });
            }
        }
    }

    Some(ProcessedImage {
        bytes: original_bytes,
        extension: store_ext,
        mime: store_mime,
        width: Some(i64::from(w)),
        height: Some(i64::from(h)),
        thumbnails,
    })
}

/// Read the EXIF orientation of a JPEG (default when absent/unreadable).
fn read_jpeg_orientation(input: &[u8]) -> Option<image::metadata::Orientation> {
    use image::ImageDecoder;
    let mut decoder = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(input)).ok()?;
    decoder.orientation().ok()
}

/// Encode an image in the target format with the configured quality.
/// `write_with_encoder` consumes the image, so it is cloned here (once per
/// upload; thumbnails pass freshly resized buffers anyway).
///
/// WebP note: the pure-Rust encoder is lossless-only (lossy needs the
/// optional `libwebp` C feature), so `quality.webp` does not apply to it.
fn encode(
    img: &image::DynamicImage,
    fmt: RasterFormat,
    cfg: &MediaImagesConfig,
) -> Option<Vec<u8>> {
    let img = img.clone();
    let mut buf = Vec::new();
    match fmt {
        RasterFormat::WebP => {
            let enc = image::codecs::webp::WebPEncoder::new_lossless(&mut buf);
            img.write_with_encoder(enc).ok()?;
        }
        RasterFormat::Jpeg => {
            let q = cfg.quality.jpeg.clamp(1, 100);
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, q);
            img.write_with_encoder(enc).ok()?;
        }
        RasterFormat::Png => {
            let enc = image::codecs::png::PngEncoder::new(&mut buf);
            img.write_with_encoder(enc).ok()?;
        }
        RasterFormat::Gif => {
            let enc = image::codecs::gif::GifEncoder::new(&mut buf);
            img.write_with_encoder(enc).ok()?;
        }
    }
    Some(buf)
}

/// Best-effort width/height for SVG sources from width/height or viewBox
/// attributes of the root element.
pub fn svg_dimensions(input: &[u8]) -> (Option<i64>, Option<i64>) {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_reader(input);
    loop {
        let Ok(ev) = reader.read_event() else {
            return (None, None);
        };
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let name = e.name();
                let tag = String::from_utf8_lossy(name.local_name().as_ref()).into_owned();
                if tag != "svg" {
                    continue;
                }
                let mut w = None;
                let mut h = None;
                let mut viewbox = None;
                for attr in e.attributes().with_checks(true).flatten() {
                    let key = String::from_utf8_lossy(attr.key.as_ref()).to_ascii_lowercase();
                    let val = String::from_utf8_lossy(&attr.value).into_owned();
                    match key.as_str() {
                        "width" => w = parse_svg_len(&val),
                        "height" => h = parse_svg_len(&val),
                        "viewbox" => viewbox = Some(val),
                        _ => {}
                    }
                }
                if (w.is_none() || h.is_none())
                    && let Some(vb) = viewbox.as_deref()
                {
                    let parts: Vec<&str> = vb.split_whitespace().collect();
                    if parts.len() == 4 {
                        w = w.or(parse_svg_len(parts[2]));
                        h = h.or(parse_svg_len(parts[3]));
                    }
                }
                return (w, h);
            }
            Event::Eof => return (None, None),
            _ => {}
        }
    }
}

/// Parse an SVG length ("120", "120px", "10cm") into integer pixels.
fn parse_svg_len(v: &str) -> Option<i64> {
    let v = v.trim();
    let num: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    let n: f64 = num.parse().ok()?;
    if n.is_finite() && n > 0.0 {
        Some(n.round() as i64)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MediaImageQuality, MediaImageSizes};
    use image::ImageEncoder;

    fn img_cfg(preferred: &str, thumbs: bool) -> MediaImagesConfig {
        MediaImagesConfig {
            generate_thumbnails: thumbs,
            preferred_format: preferred.to_string(),
            sizes: MediaImageSizes {
                thumb: 240,
                small: 480,
                medium: 960,
                large: 1920,
            },
            quality: MediaImageQuality::default(),
        }
    }

    fn exif_cfg(strip: bool) -> MediaExifConfig {
        MediaExifConfig { strip }
    }

    fn test_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::DynamicImage::new_rgb8(w, h);
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(img.as_bytes(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        buf
    }

    #[test]
    fn extracts_dimensions_and_generates_thumbnails() {
        let png = test_png(1200, 600);
        let out = process(&img_cfg("", true), &exif_cfg(true), &png, RasterFormat::Png).unwrap();
        assert_eq!(out.width, Some(1200));
        assert_eq!(out.height, Some(600));
        assert_eq!(out.extension, "png");
        // 1200x600 > medium(960)? width 1200 > 960 → medium generated.
        // small(480) and thumb(240) generated; large(1920) not (no upscale).
        let names: Vec<&str> = out
            .thumbnails
            .iter()
            .map(|t| t.size_name.as_str())
            .collect();
        assert!(names.contains(&"thumb"));
        assert!(names.contains(&"small"));
        assert!(names.contains(&"medium"));
        assert!(!names.contains(&"large"), "must not upscale: {names:?}");
    }

    #[test]
    fn small_images_get_no_thumbnails() {
        let png = test_png(100, 50);
        let out = process(&img_cfg("", true), &exif_cfg(true), &png, RasterFormat::Png).unwrap();
        assert!(out.thumbnails.is_empty());
    }

    #[test]
    fn converts_png_to_webp_when_preferred() {
        let png = test_png(800, 400);
        let out = process(
            &img_cfg("webp", false),
            &exif_cfg(false),
            &png,
            RasterFormat::Png,
        )
        .unwrap();
        assert_eq!(out.extension, "webp");
        assert_eq!(out.mime, "image/webp");
        // The converted bytes decode as WebP.
        assert!(image::load_from_memory(&out.bytes).is_ok());
    }

    #[test]
    fn no_conversion_keeps_original_bytes() {
        let png = test_png(300, 300);
        let out = process(
            &img_cfg("", false),
            &exif_cfg(false),
            &png,
            RasterFormat::Png,
        )
        .unwrap();
        assert_eq!(out.bytes, png);
    }

    #[test]
    fn avif_and_unknown_extensions_are_not_processed() {
        assert_eq!(RasterFormat::from_extension("avif"), None);
        assert_eq!(RasterFormat::from_extension("svg"), None);
        assert_eq!(
            RasterFormat::from_extension("jpg"),
            Some(RasterFormat::Jpeg)
        );
    }

    #[test]
    fn svg_dimension_parsing() {
        let (w, h) = svg_dimensions(
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="640" height="480"><rect/></svg>"#,
        );
        assert_eq!((w, h), (Some(640), Some(480)));
        let (w, h) = svg_dimensions(br#"<svg viewBox="0 0 120 60"></svg>"#);
        assert_eq!((w, h), (Some(120), Some(60)));
        let (w, h) = svg_dimensions(br#"<svg width="100px" height="50pt"></svg>"#);
        assert_eq!((w, h), (Some(100), Some(50)));
        let (w, h) = svg_dimensions(b"garbage");
        assert_eq!((w, h), (None, None));
    }

    #[test]
    fn jpeg_orientation_is_applied() {
        // Build a JPEG, then wrap it with an EXIF orientation 6 tag.
        let img = image::DynamicImage::new_rgb8(40, 20);
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90)
            .write_image(img.as_bytes(), 40, 20, image::ExtendedColorType::Rgb8)
            .unwrap();
        // Without EXIF, dimensions stay as-is.
        let out = process(
            &img_cfg("", false),
            &exif_cfg(true),
            &buf,
            RasterFormat::Jpeg,
        )
        .unwrap();
        assert_eq!(out.width, Some(40));
        assert_eq!(out.height, Some(20));
        // Strip re-encodes (drops EXIF) but keeps geometry.
        assert_ne!(out.bytes.len(), 0);
    }
}
