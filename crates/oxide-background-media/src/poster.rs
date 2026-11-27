use crate::{
    MediaError, MediaFrame, MediaInfo, MemoryBudget, OutputParams, decoder::Decoder, pixels,
};
use std::io::Read;
use std::path::Path;

fn sized_still(
    path: &Path,
    output: OutputParams,
    budget: &MemoryBudget,
) -> Result<Option<(MediaInfo, image::DynamicImage, crate::PixelLease)>, MediaError> {
    if output.fit == crate::BackgroundFit::Tile {
        return Ok(None);
    }
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return Ok(None);
    }
    let source = reader.into_dimensions()?;
    let desired = crate::output_dimensions(source, output)?;
    let ratio = (desired.0 as f64 / source.0 as f64)
        .max(desired.1 as f64 / source.1 as f64)
        .min(1.0);
    if ratio >= 1.0 {
        return Ok(None);
    }
    // Fill may change the aspect ratio. Decode enough pixels for both axes, then
    // let the existing processor apply fit, blur and output limits once more.
    let target = (
        (source.0 as f64 * ratio).ceil() as u32,
        (source.1 as f64 * ratio).ceil() as u32,
    );
    let file = std::fs::File::open(path)?;
    let length =
        usize::try_from(file.metadata()?.len()).map_err(|_| MediaError::ResourceExhausted)?;
    let capacity = length.checked_add(1).ok_or(MediaError::ResourceExhausted)?;
    let _input = budget.reserve(capacity)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(length as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > length {
        return Err(MediaError::ResourceExhausted);
    }
    let Some(info) = oxide_image_decoder::ImageInfo::read(&bytes, None)
        .map_err(|error| MediaError::Decode(error.to_string()))?
    else {
        return Ok(None);
    };
    let info = info.without_orientation();
    let scratch = if info.format == image::ImageFormat::Jpeg {
        // Account for native JPEG coefficient storage, even when IDCT output is reduced.
        pixels::pixel_bytes(source.0, source.1)?
            .checked_mul(3)
            .and_then(|n| n.checked_div(2))
    } else {
        (source.0 as usize)
            .checked_mul(16)
            .and_then(|n| n.checked_add(target.0 as usize * 128 + 65536))
    }
    .ok_or(MediaError::ResourceExhausted)?;
    let lease = budget.reserve(
        scratch
            .checked_add(pixels::pixel_bytes(target.0, target.1)? * 3)
            .ok_or(MediaError::ResourceExhausted)?,
    )?;
    let Some(pixels) = info
        .decode(&bytes, target)
        .map_err(|error| MediaError::Decode(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some((
        MediaInfo {
            width: source.0,
            height: source.1,
            animated: false,
            frame_count: Some(1),
            loops: crate::LoopCount::Finite(1),
        },
        image::DynamicImage::ImageRgba8(pixels),
        lease,
    )))
}

// Galleries request many covers together. Admit one full-resolution poster decode at a time.
static POSTER_DECODE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Inspect playback capability without decoding pixels. Call on a background thread.
pub fn is_animated_media(path: &Path) -> Result<bool, MediaError> {
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    match reader.format() {
        Some(image::ImageFormat::Gif) => Ok(crate::gif_metadata::inspect(
            &mut std::io::BufReader::new(std::fs::File::open(path)?),
        )?
        .animated),
        Some(image::ImageFormat::WebP) => Ok(image_webp::WebPDecoder::new(
            std::io::BufReader::new(std::fs::File::open(path)?),
        )?
        .num_frames()
            > 1),
        Some(_) => Ok(false),
        None => {
            if path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| {
                    value.eq_ignore_ascii_case("mp4") || value.eq_ignore_ascii_case("m4v")
                })
            {
                Ok(true)
            } else {
                Err(MediaError::Unsupported)
            }
        }
    }
}

/// Decode exactly one composed frame. Call on a background thread; video objects stay local.
pub fn decode_poster(
    path: &Path,
    output: OutputParams,
) -> Result<(MediaInfo, MediaFrame), MediaError> {
    let _decode = POSTER_DECODE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let budget = MemoryBudget::default();
    if let Some((info, image, _lease)) = sized_still(path, output, &budget)? {
        return finish_poster(info, image, output, &budget, false);
    }
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let mut decoder = Decoder::open(path, &budget, &cancelled)?;
    let (image, _, _) = decoder
        .next(&cancelled)?
        .ok_or_else(|| MediaError::Decode("media contains no frame".into()))?;
    finish_poster(
        decoder.info.clone(),
        image,
        output,
        &budget,
        decoder.is_bgra(),
    )
}

fn finish_poster(
    info: MediaInfo,
    image: image::DynamicImage,
    output: OutputParams,
    budget: &MemoryBudget,
    bgra: bool,
) -> Result<(MediaInfo, MediaFrame), MediaError> {
    let (pixels, lease, (width, height)) = pixels::process(image, output, budget, bgra)?;
    Ok((
        info,
        MediaFrame {
            generation: 0,
            sequence: 1,
            timestamp: std::time::Duration::ZERO,
            duration: std::time::Duration::ZERO,
            processing_time: std::time::Duration::ZERO,
            width,
            height,
            pixels,
            native: None,
            _lease: Some(lease),
            recycle: None,
        },
    ))
}
