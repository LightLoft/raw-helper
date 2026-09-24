//! System decoders on macOS: ImageIO reads headers, non-raw formats and thumbnails; Core Image
//! develops the raw files rawler cannot read (`CIRAWFilter`), without tone curve, sharpening,
//! noise reduction or rotation, so that the engine keeps every creative decision.
//!
//! Output never goes through an intermediate buffer: Core Image renders straight into the shared
//! memory sent to the application.

use std::ptr::NonNull;

use loft_raw_protocol::shm::SharedBuffer;
use loft_raw_protocol::{
    Decoder, Exif, FileInfo, ImageLayout, PixelOrigin, PreviewSpace, Sample, SensorInfo,
    SensorLayout,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFData, CFDictionary, CFRetained, CFString, CGAffineTransform};
use objc2_core_graphics::{
    kCGColorSpaceDisplayP3, kCGColorSpaceExtendedLinearITUR_2020, CGColorSpace,
};
use objc2_core_image::{kCIFormatRGBA8, kCIFormatRGBAh, CIContext, CIFormat, CIImage, CIRAWFilter};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSNumber, NSString};
use objc2_image_io::{
    kCGImagePropertyExifDateTimeOriginal, kCGImagePropertyExifDictionary,
    kCGImagePropertyExifExposureTime, kCGImagePropertyExifFNumber, kCGImagePropertyExifFocalLength,
    kCGImagePropertyExifISOSpeedRatings, kCGImagePropertyExifLensModel,
    kCGImagePropertyOrientation, kCGImagePropertyTIFFDictionary, kCGImagePropertyTIFFMake,
    kCGImagePropertyTIFFModel, kCGImageSourceCreateThumbnailFromImageIfAbsent,
    kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceThumbnailMaxPixelSize,
    kCGImageSourceTypeIdentifierHint, CGImagePropertyOrientation, CGImageSource,
};
use objc2_uniform_type_identifiers::UTType;

thread_local! {
    /// One Core Image context for the life of the helper: creating one compiles GPU kernels
    /// (measured: 4.7 s instead of about 1 s per raw file when recreated every time).
    // SAFETY: default context, no options.
    static CONTEXT: Retained<CIContext> = unsafe { CIContext::contextWithOptions(None) };
}

/// Largest image the system develops at full size: above it, the image is developed at reduced
/// size (100 Mpx of half-float RGBA is 800 MB), in line with editing very large photos from a
/// proxy.
const MAX_DEVELOPED_PIXELS: f64 = 100e6;

/// Longest side of a thumbnail produced by the system when the file has no usable preview.
const THUMBNAIL_MAX_SIDE: i32 = 2560;

pub struct SystemFile {
    data: Retained<NSData>,
    source: CFRetained<CGImageSource>,
    raw: bool,
    /// Uniform type identifier derived from the file extension: without it, the system sniffs
    /// the bytes and mistakes some raw files for plain TIFF.
    type_hint: Option<Retained<NSString>>,
}

/// Type identifier for a file extension, if the system knows it. Only short alphanumeric
/// extensions are considered.
fn type_hint(extension: Option<&str>) -> Option<Retained<NSString>> {
    let extension = extension
        .filter(|e| (1..=8).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_alphanumeric()))?;
    UTType::typeWithFilenameExtension(&NSString::from_str(extension)).map(|t| t.identifier())
}

// CFString and NSString, CFDictionary and NSDictionary, CFData and NSData are toll-free bridged:
// the same object seen through either API. These casts only change the static type.
fn ns_string(value: &CFString) -> &NSString {
    // SAFETY: toll-free bridged types (see above).
    unsafe { &*(value as *const CFString).cast::<NSString>() }
}
fn cf_data(value: &NSData) -> &CFData {
    // SAFETY: toll-free bridged types (see above).
    unsafe { &*(value as *const NSData).cast::<CFData>() }
}
fn ns_dictionary(value: &CFDictionary) -> &NSDictionary<NSString, AnyObject> {
    // SAFETY: toll-free bridged types; ImageIO property dictionaries have string keys.
    unsafe { &*(value as *const CFDictionary).cast::<NSDictionary<NSString, AnyObject>>() }
}
fn cf_dictionary(value: &NSDictionary<NSString, AnyObject>) -> &CFDictionary {
    // SAFETY: toll-free bridged types (see above).
    unsafe { &*(value as *const NSDictionary<NSString, AnyObject>).cast::<CFDictionary>() }
}

fn object(dict: &NSDictionary<NSString, AnyObject>, key: &CFString) -> Option<Retained<AnyObject>> {
    dict.objectForKey(ns_string(key))
}
fn number(dict: &NSDictionary<NSString, AnyObject>, key: &CFString) -> Option<f64> {
    object(dict, key)?
        .downcast::<NSNumber>()
        .ok()
        .map(|n| n.doubleValue())
}
fn text(dict: &NSDictionary<NSString, AnyObject>, key: &CFString) -> Option<String> {
    object(dict, key)?
        .downcast::<NSString>()
        .ok()
        .map(|s| s.to_string())
}
fn sub_dictionary(
    dict: &NSDictionary<NSString, AnyObject>,
    key: &CFString,
) -> Option<Retained<NSDictionary<NSString, AnyObject>>> {
    let value = object(dict, key)?.downcast::<NSDictionary>().ok()?;
    // SAFETY: ImageIO sub-dictionaries have string keys, like their parent.
    Some(unsafe { Retained::cast_unchecked(value) })
}

impl SystemFile {
    /// Recognises the file with ImageIO and reads its headers. The bytes are copied once into an
    /// `NSData` owned by this object.
    pub fn open(bytes: &[u8], extension: Option<&str>) -> Result<(Self, FileInfo), String> {
        let data = NSData::with_bytes(bytes);
        let type_hint = type_hint(extension);
        let options = type_hint.as_ref().map(|hint| {
            let key = ns_string(unsafe { kCGImageSourceTypeIdentifierHint });
            let value: &AnyObject = hint;
            NSDictionary::from_slices(&[key], &[value])
        });
        let options = options.as_deref().map(cf_dictionary);
        // SAFETY: the options dictionary maps the documented key to a string.
        let source = unsafe { CGImageSource::with_data(cf_data(&data), options) }
            .ok_or("format not recognised")?;
        // SAFETY: queries on a valid image source.
        let (count, kind) = unsafe { (source.count(), source.r#type()) };
        if count == 0 {
            return Err("no image in file".into());
        }
        // Raw formats have uniform type identifiers ending in "raw-image".
        let raw = kind.is_some_and(|uti| uti.to_string().ends_with("raw-image"));
        let info = file_info(&source, raw);
        let file = Self {
            data,
            source,
            raw,
            type_hint,
        };
        Ok((file, info))
    }

    /// Thumbnail (embedded if present, else rendered by the system), colour-managed to Display P3.
    pub fn preview(&self) -> Result<(ImageLayout, SharedBuffer), String> {
        let keys = [
            ns_string(unsafe { kCGImageSourceCreateThumbnailFromImageIfAbsent }),
            ns_string(unsafe { kCGImageSourceThumbnailMaxPixelSize }),
            ns_string(unsafe { kCGImageSourceCreateThumbnailWithTransform }),
        ];
        let values: [Retained<AnyObject>; 3] = [
            NSNumber::new_bool(true).into(),
            NSNumber::new_i32(THUMBNAIL_MAX_SIDE).into(),
            // Pixels stay in file orientation; the orientation is reported separately.
            NSNumber::new_bool(false).into(),
        ];
        let values: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
        let options = NSDictionary::from_slices(&keys, &values);
        // SAFETY: the options dictionary holds the documented key and value types.
        let thumbnail = unsafe {
            self.source
                .thumbnail_at_index(0, Some(cf_dictionary(&options)))
        }
        .ok_or("no thumbnail")?;
        // SAFETY: plain wrapping of a valid image.
        let image = unsafe { CIImage::imageWithCGImage(&thumbnail) };
        let (width, height, buffer) = render(&image, unsafe { kCIFormatRGBA8 }, 4, unsafe {
            kCGColorSpaceDisplayP3
        })?;
        Ok((
            ImageLayout {
                width,
                height,
                color_space: PreviewSpace::DisplayP3,
            },
            buffer,
        ))
    }

    /// Full image as scene-linear half-float RGBA, BT.2020 primaries (see `PixelOrigin`).
    pub fn develop(&self) -> Result<(SensorLayout, SensorInfo, SharedBuffer), String> {
        let image = if self.raw {
            // SAFETY: valid data and identifier hint.
            let hint = self.type_hint.as_deref();
            let filter =
                unsafe { CIRAWFilter::filterWithImageData_identifierHint(&self.data, hint) }
                    .ok_or("raw format not supported by the system")?;
            // SAFETY: plain property setters on a live filter.
            unsafe {
                filter.setBoostAmount(0.0);
                filter.setLocalToneMapAmount(0.0);
                filter.setContrastAmount(0.0);
                filter.setDetailAmount(0.0);
                filter.setSharpnessAmount(0.0);
                filter.setLuminanceNoiseReductionAmount(0.0);
                filter.setColorNoiseReductionAmount(0.0);
                filter.setGamutMappingEnabled(false);
                filter.setOrientation(CGImagePropertyOrientation::Up);
            }
            // SAFETY: reading the output of a configured filter.
            unsafe { filter.outputImage() }.ok_or("system raw decoding failed")?
        } else {
            // SAFETY: no options dictionary.
            let decoded =
                unsafe { self.source.image_at_index(0, None) }.ok_or("image decoding failed")?;
            // SAFETY: plain wrapping of a valid image.
            unsafe { CIImage::imageWithCGImage(&decoded) }
        };
        // SAFETY: reading the extent of a valid image.
        let extent = unsafe { image.extent() };
        let pixels = extent.size.width * extent.size.height;
        let scale = if pixels > MAX_DEVELOPED_PIXELS {
            (MAX_DEVELOPED_PIXELS / pixels).sqrt()
        } else {
            1.0
        };
        let image = if scale < 1.0 {
            let transform = CGAffineTransform {
                a: scale,
                b: 0.0,
                c: 0.0,
                d: scale,
                tx: 0.0,
                ty: 0.0,
            };
            // SAFETY: plain scaling of a valid image.
            unsafe { image.imageByApplyingTransform(transform) }
        } else {
            image
        };
        let (width, height, buffer) = render(&image, unsafe { kCIFormatRGBAh }, 8, unsafe {
            kCGColorSpaceExtendedLinearITUR_2020
        })?;
        let info = SensorInfo {
            origin: PixelOrigin::SystemLinearBt2020,
            scale: scale as f32,
            bits_per_sample: 16,
            orientation: orientation(&self.source),
            ..SensorInfo::default()
        };
        let layout = SensorLayout {
            width,
            height,
            components: 4,
            sample: Sample::F16,
        };
        Ok((layout, info, buffer))
    }
}

/// Renders a Core Image image straight into a new shared buffer.
fn render(
    image: &CIImage,
    format: CIFormat,
    bytes_per_pixel: usize,
    color_space: &CFString,
) -> Result<(u32, u32, SharedBuffer), String> {
    // SAFETY: reading the extent of a valid image.
    let extent = unsafe { image.extent() };
    let (width, height) = (extent.size.width as usize, extent.size.height as usize);
    if width == 0
        || height == 0
        || !extent.size.width.is_finite()
        || !extent.size.height.is_finite()
    {
        return Err("image has no finite extent".into());
    }
    let mut buffer =
        SharedBuffer::create(width * height * bytes_per_pixel).map_err(|e| e.to_string())?;
    let space = CGColorSpace::with_name(Some(color_space)).ok_or("colour space unavailable")?;
    let context = CONTEXT.with(|context| context.clone());
    let data = NonNull::new(buffer.map.as_mut_ptr().cast()).ok_or("null buffer")?;
    // SAFETY: `data` points to `width * height * bytes_per_pixel` writable bytes, exactly the
    // bitmap described by `row_bytes`, `extent` and `format`.
    unsafe {
        context.render_toBitmap_rowBytes_bounds_format_colorSpace(
            image,
            data,
            (width * bytes_per_pixel) as isize,
            extent,
            format,
            Some(&space),
        );
    }
    Ok((width as u32, height as u32, buffer))
}

fn properties(source: &CGImageSource) -> Option<CFRetained<CFDictionary>> {
    // SAFETY: no options dictionary.
    unsafe { source.properties_at_index(0, None) }
}

fn orientation(source: &CGImageSource) -> u16 {
    properties(source)
        .and_then(|p| number(ns_dictionary(&p), unsafe { kCGImagePropertyOrientation }))
        .map_or(0, |o| o as u16)
}

fn file_info(source: &CGImageSource, raw: bool) -> FileInfo {
    let mut info = FileInfo {
        decoder: Decoder::System,
        raw,
        ..FileInfo::default()
    };
    let Some(props) = properties(source) else {
        return info;
    };
    let props = ns_dictionary(&props);
    // SAFETY (all key reads below): ImageIO key constants are valid static strings.
    info.orientation =
        number(props, unsafe { kCGImagePropertyOrientation }).map_or(0, |o| o as u16);
    if let Some(tiff) = sub_dictionary(props, unsafe { kCGImagePropertyTIFFDictionary }) {
        info.make = text(&tiff, unsafe { kCGImagePropertyTIFFMake }).unwrap_or_default();
        info.model = text(&tiff, unsafe { kCGImagePropertyTIFFModel }).unwrap_or_default();
    }
    if let Some(exif) = sub_dictionary(props, unsafe { kCGImagePropertyExifDictionary }) {
        let iso = object(&exif, unsafe { kCGImagePropertyExifISOSpeedRatings })
            .and_then(|v| v.downcast::<NSArray>().ok())
            .and_then(|a| a.firstObject())
            .and_then(|v| v.downcast::<NSNumber>().ok())
            .map(|n| n.unsignedIntValue());
        let exposure = number(&exif, unsafe { kCGImagePropertyExifExposureTime }).map(|t| {
            if t > 0.0 && t < 1.0 {
                (1, (1.0 / t).round() as u32)
            } else {
                ((t * 10.0).round() as u32, 10)
            }
        });
        info.exif = Exif {
            iso,
            exposure_time: exposure,
            f_number: number(&exif, unsafe { kCGImagePropertyExifFNumber }).map(|v| v as f32),
            focal_length: number(&exif, unsafe { kCGImagePropertyExifFocalLength })
                .map(|v| v as f32),
            lens_model: text(&exif, unsafe { kCGImagePropertyExifLensModel }),
            date_time_original: text(&exif, unsafe { kCGImagePropertyExifDateTimeOriginal }),
        };
    }
    info
}

/// A 1x1 PNG: enough to initialise ImageIO, Core Image and the GPU before the sandbox closes.
const WARM_UP_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Decodes and renders a tiny image once, so that frameworks, GPU device and kernels are loaded
/// while the process may still open them; failures are ignored (decoding then reports them).
pub fn warm_up() {
    if let Ok((file, _)) = SystemFile::open(WARM_UP_PNG, Some("png")) {
        let _ = file.develop();
        let _ = file.preview();
    }
}
