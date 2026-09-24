//! Decoding with rawler: headers, embedded preview, sensor samples. No error handling policy
//! here (panics propagate): the process loop guards each call, and the fuzz targets want to see
//! them.

use loft_raw_protocol::shm::SharedBuffer;
use loft_raw_protocol::{
    ColorMatrix, Exif, FileInfo, ImageLayout, Sample, SensorInfo, SensorLayout,
};
use rawler::decoders::{Decoder, RawDecodeParams};
use rawler::formats::tiff::Rational;
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use rawler::{RawImage, RawImageData};

pub fn decoder(source: &RawSource) -> Result<Box<dyn Decoder>, String> {
    rawler::get_decoder(source).map_err(|e| e.to_string())
}

/// Headers only: the decoder is identified and the EXIF read, no pixel data is touched.
pub fn info(source: &RawSource) -> Result<FileInfo, String> {
    let decoder = decoder(source)?;
    let metadata = decoder
        .raw_metadata(source, &RawDecodeParams::default())
        .map_err(|e| e.to_string())?;
    let e = metadata.exif;
    Ok(FileInfo {
        decoder: loft_raw_protocol::Decoder::Raw,
        raw: true,
        make: metadata.make,
        model: metadata.model,
        orientation: e.orientation.unwrap_or(0),
        exif: Exif {
            iso: e.iso_speed.or(e.iso_speed_ratings.map(u32::from)),
            exposure_time: e.exposure_time.map(|r| (r.n, r.d)),
            f_number: e.fnumber.map(ratio),
            focal_length: e.focal_length.map(ratio),
            lens_model: e.lens_model,
            date_time_original: e.date_time_original,
        },
    })
}

pub fn ratio(value: Rational) -> f32 {
    if value.d == 0 {
        0.0
    } else {
        value.n as f32 / value.d as f32
    }
}

pub fn describe(image: &RawImage) -> SensorInfo {
    let area = |rect: &Option<rawler::imgop::Rect>| {
        rect.as_ref()
            .map(|r| [r.p.x as u32, r.p.y as u32, r.d.w as u32, r.d.h as u32])
    };
    let cfa = match &image.photometric {
        RawPhotometricInterpretation::Cfa(config) => config.cfa.name.clone(),
        _ => String::new(),
    };
    SensorInfo {
        origin: loft_raw_protocol::PixelOrigin::Sensor,
        scale: 1.0,
        bits_per_sample: image.bps as u8,
        cfa,
        white_balance: image.wb_coeffs,
        black_levels: image.blacklevel.levels.iter().map(|r| ratio(*r)).collect(),
        white_levels: image.whitelevel.0.iter().map(|&w| w as f32).collect(),
        color_matrices: image
            .color_matrix
            .iter()
            .map(|(illuminant, values)| ColorMatrix {
                illuminant: *illuminant as u16,
                values: values.clone(),
            })
            .collect(),
        active_area: area(&image.active_area),
        crop_area: area(&image.crop_area),
        orientation: image.orientation.to_u16(),
    }
}

pub fn preview(source: &RawSource) -> Result<(ImageLayout, SharedBuffer), String> {
    let decoder = decoder(source)?;
    let params = RawDecodeParams::default();
    let image = decoder
        .preview_image(source, &params)
        .ok()
        .flatten()
        .or_else(|| decoder.full_image(source, &params).ok().flatten())
        .or_else(|| decoder.thumbnail_image(source, &params).ok().flatten())
        .ok_or("no embedded preview")?;
    let rgba = image.to_rgba8();
    let layout = ImageLayout {
        width: rgba.width(),
        height: rgba.height(),
        color_space: loft_raw_protocol::PreviewSpace::Srgb,
    };
    let mut buffer = SharedBuffer::create(layout.bytes()).map_err(|e| e.to_string())?;
    buffer.map.copy_from_slice(rgba.as_raw());
    Ok((layout, buffer))
}

pub fn sensor(source: &RawSource) -> Result<(SensorLayout, SensorInfo, SharedBuffer), String> {
    let decoder = decoder(source)?;
    let image = decoder
        .raw_image(source, &RawDecodeParams::default(), false)
        .map_err(|e| e.to_string())?;
    let (sample, bytes): (Sample, &[u8]) = match &image.data {
        RawImageData::Integer(data) => (Sample::U16, bytemuck::cast_slice(data)),
        RawImageData::Float(data) => (Sample::F32, bytemuck::cast_slice(data)),
    };
    let layout = SensorLayout {
        width: image.width as u32,
        height: image.height as u32,
        components: image.cpp as u8,
        sample,
    };
    if layout.bytes() != bytes.len() {
        return Err("sample count does not match the image size".into());
    }
    let mut buffer = SharedBuffer::create(bytes.len()).map_err(|e| e.to_string())?;
    buffer.map.copy_from_slice(bytes);
    Ok((layout, describe(&image), buffer))
}
