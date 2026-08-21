//! Fit an image under a base64 byte ceiling by downscaling and re-encoding.
//! Decoding is driven by the file's magic bytes, encoding by the media type
//! the caller declared — so a `.png` that is really a JPEG comes back as a
//! real PNG and its `mimeType` stops lying.

use image::GenericImageView;
use image::ImageFormat;
use rig_core::completion::message::ImageMediaType;
use std::io::Cursor;

#[derive(Debug)]
pub(crate) struct Fitted {
    pub bytes: Vec<u8>,
    /// `Some` iff the image was downscaled. Carries everything the notice needs.
    pub resize: Option<ResizeReport>,
}

#[derive(Debug)]
pub(crate) struct ResizeReport {
    pub from: (u32, u32),
    pub to: (u32, u32),
    pub from_b64: usize,
    pub to_b64: usize,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum FitError {
    #[error(
        "cannot resize {0}: only PNG and JPEG can be downscaled (an animated GIF would lose its animation)"
    )]
    UnsupportedFormat(&'static str),
    #[error("not a decodable image (the bytes do not match any supported format)")]
    Undecodable,
    #[error("decode failed: {0}")]
    Decode(String),
    #[error("encode failed: {0}")]
    Encode(String),
    #[error("still {last_b64} base64 bytes at {edge}px — cannot fit under {ceiling}")]
    CannotFit {
        last_b64: usize,
        edge: u32,
        ceiling: usize,
    },
}

/// Base64 length of `raw` bytes: every 3 input bytes cost 4 characters and a
/// final partial group still costs 4 — the factor the raw-byte check got wrong.
const fn base64_len(raw: usize) -> usize {
    raw.div_ceil(3) * 4
}

/// Anthropic downsamples anything longer server-side; more pixels buy nothing.
const FIRST_EDGE: u32 = 1568;
const MIN_EDGE: u32 = 64;
/// 1568 → 784 → 392 → 196 → 98 → 49 — terminates by construction.
const MAX_STEPS: u32 = 6;

/// The declared format's name, for the refusal message.
fn declared_format_name(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::PNG => "PNG",
        ImageMediaType::JPEG => "JPEG",
        ImageMediaType::GIF => "GIF",
        ImageMediaType::WEBP => "WEBP",
        ImageMediaType::HEIC => "HEIC",
        ImageMediaType::HEIF => "HEIF",
        ImageMediaType::SVG => "SVG",
    }
}

pub(crate) fn fit_under_ceiling(
    bytes: Vec<u8>,
    media_type: ImageMediaType,
    ceiling_b64: usize,
) -> Result<Fitted, FitError> {
    let from_b64 = base64_len(bytes.len());
    // Common path: under the ceiling the bytes ship as-is — decoding is pure cost.
    if from_b64 <= ceiling_b64 {
        return Ok(Fitted {
            bytes,
            resize: None,
        });
    }

    let target = match media_type {
        ImageMediaType::PNG => ImageFormat::Png,
        ImageMediaType::JPEG => ImageFormat::Jpeg,
        // `image` decodes only frame 1; "resizing" an animated GIF would destroy it.
        _ => return Err(FitError::UnsupportedFormat(declared_format_name(media_type))),
    };

    // Magic bytes, not the extension: a lying extension is corrected for free.
    let true_format = image::guess_format(&bytes).map_err(|_| FitError::Undecodable)?;
    let img = image::load_from_memory_with_format(&bytes, true_format)
        .map_err(|e| FitError::Decode(e.to_string()))?;
    let from = img.dimensions();

    // Never upscale: a small image with a bloated payload fits by re-encoding alone.
    let mut edge = FIRST_EDGE.min(from.0.max(from.1));
    let mut last_b64 = 0;
    for _ in 0..MAX_STEPS {
        // Halving, not a computed scale factor: compressed size is entropy, not a
        // function of pixel count, so any "factor" would be a guess.
        // Triangle: no ringing on 1-px UI lines — the right trade for screenshots.
        let cand = img.resize(edge, edge, image::imageops::FilterType::Triangle);
        let mut cursor = Cursor::new(Vec::new());
        cand.write_to(&mut cursor, target)
            .map_err(|e| FitError::Encode(e.to_string()))?;
        let out = cursor.into_inner();
        let to_b64 = base64_len(out.len());
        if to_b64 <= ceiling_b64 {
            return Ok(Fitted {
                bytes: out,
                resize: Some(ResizeReport {
                    from,
                    to: cand.dimensions(),
                    from_b64,
                    to_b64,
                }),
            });
        }
        last_b64 = to_b64;
        edge /= 2;
        if edge < MIN_EDGE {
            break;
        }
    }
    Err(FitError::CannotFit {
        last_b64,
        edge,
        ceiling: ceiling_b64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use image::codecs::jpeg::JpegEncoder;
    use image::{ImageFormat, Rgb, RgbImage};
    use std::time::{Duration, Instant};

    /// The production default ceiling (api.anthropic.com's documented limit).
    const CEILING_5_MIB: usize = 5 * 1024 * 1024;

    /// Deterministic xorshift so fixtures are byte-stable across runs.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> u8 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 & 0xFF) as u8
        }
    }

    fn noise_rgb(width: u32, height: u32, seed: u64) -> RgbImage {
        let mut n = Noise(seed);
        let mut raw = Vec::with_capacity((width * height * 3) as usize);
        for _ in 0..(width * height) {
            raw.push(n.next());
            raw.push(n.next());
            raw.push(n.next());
        }
        RgbImage::from_raw(width, height, raw).expect("exact pixel count")
    }

    fn encode_png(img: &RgbImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Png)
            .expect("encode png");
        buf
    }

    /// Splice `count` 1 MiB private ancillary chunks in front of IEND. PNG
    /// decoders skip unknown non-critical chunks, so the image still decodes —
    /// but the payload is ~9 MB of junk that only re-encoding can strip.
    fn pad_png_with_junk_chunks(png: &[u8], count: usize) -> Vec<u8> {
        let iend = png.len() - 12; // trailing IEND: 4 len + 4 type + 4 crc
        let mut out = png[..iend].to_vec();
        for i in 0..count {
            let data = vec![0x5A ^ (i as u8); 1_048_576];
            out.extend(png_chunk(b"Pkbj", &data));
        }
        out.extend_from_slice(&png[iend..]);
        out
    }

    fn png_chunk(typ: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::with_capacity(12 + data.len());
        chunk.extend((data.len() as u32).to_be_bytes());
        chunk.extend(typ);
        chunk.extend(data);
        let mut crc_input = Vec::with_capacity(4 + data.len());
        crc_input.extend_from_slice(typ);
        crc_input.extend_from_slice(data);
        chunk.extend(crc32(&crc_input).to_be_bytes());
        chunk
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (0xFFFF_FFFF ^ (crc & 1)));
            }
        }
        !crc
    }

    #[test]
    fn under_ceiling_returns_bytes_untouched_and_never_decodes() {
        // Undecodable bytes under the ceiling must pass through without
        // touching the decoder — this is what keeps `loads_image_as_image_json`
        // (which feeds "fake png bytes") passing unchanged.
        let input = b"not an image at all".to_vec();
        let fitted = fit_under_ceiling(input.clone(), ImageMediaType::PNG, 1024 * 1024)
            .expect("under-ceiling input must pass");
        assert_eq!(fitted.bytes, input);
        assert!(fitted.resize.is_none());
    }

    #[test]
    fn oversized_png_is_downscaled_to_fit_and_reports_both_dimensions() {
        let img = noise_rgb(4128, 2208, 0x4128_2208);
        let raw = encode_png(&img);
        assert!(
            base64_len(raw.len()) > CEILING_5_MIB,
            "fixture must start over the ceiling: {} bytes",
            raw.len()
        );

        let fitted = fit_under_ceiling(raw, ImageMediaType::PNG, CEILING_5_MIB)
            .expect("must fit under the ceiling");
        let report = fitted.resize.expect("oversized input must be downscaled");

        assert!(base64_len(fitted.bytes.len()) <= CEILING_5_MIB);
        assert_eq!(report.from, (4128, 2208));
        assert!(
            report.to.0 <= 1568 && report.to.1 <= 1568,
            "long edge must be clamped: {:?}",
            report.to
        );
        // Aspect preserved within 1 px of the exact 4128:2208 ratio.
        let expected_short = (report.to.0 as f64 * 2208.0 / 4128.0).round() as i64;
        assert!(
            (report.to.1 as i64 - expected_short).abs() <= 1,
            "aspect drifted: {:?}",
            report.to
        );
        assert_eq!(report.to_b64, base64_len(fitted.bytes.len()));
        // Output re-decodes as a valid PNG with exactly the reported size.
        assert_eq!(
            image::guess_format(&fitted.bytes).expect("png magic"),
            ImageFormat::Png
        );
        let decoded = image::load_from_memory(&fitted.bytes).expect("output must re-decode");
        assert_eq!((decoded.width(), decoded.height()), report.to);
    }

    #[test]
    fn base64_len_matches_the_real_encoder() {
        for n in [0usize, 1, 2, 3, 4, 100, 8_747_301] {
            let encoded = STANDARD.encode(vec![0u8; n]);
            assert_eq!(base64_len(n), encoded.len(), "n = {n}");
        }
        // The exact numbers from the production 400.
        assert_eq!(base64_len(8_747_301), 11_663_068);
    }

    #[test]
    fn small_dimensions_huge_payload_fits_by_re_encoding_alone() {
        // 64×64 padded to ~9 MB of junk ancillary chunks: re-encoding alone
        // must strip the junk — a dimensions-only implementation silently
        // fails this.
        let img = noise_rgb(64, 64, 0x40);
        let png = encode_png(&img);
        let padded = pad_png_with_junk_chunks(&png, 9);
        assert!(
            padded.len() > 9 * 1024 * 1024,
            "fixture: {} bytes",
            padded.len()
        );
        assert!(
            base64_len(padded.len()) > CEILING_5_MIB,
            "fixture must start over the ceiling"
        );
        image::load_from_memory(&padded).expect("fixture must decode");

        let fitted = fit_under_ceiling(padded, ImageMediaType::PNG, CEILING_5_MIB)
            .expect("re-encoding alone must fit");
        let report = fitted.resize.expect("a resize report is expected");
        assert_eq!(report.from, (64, 64));
        assert_eq!(report.to, (64, 64), "no downscale needed at 64×64");
        assert!(base64_len(fitted.bytes.len()) <= CEILING_5_MIB);
        assert!(
            fitted.bytes.len() < 100_000,
            "junk must be stripped: {} bytes",
            fitted.bytes.len()
        );
    }

    #[test]
    fn garbage_bytes_over_ceiling_error_undecodable() {
        let garbage = vec![0xABu8; 9 * 1024 * 1024];
        let err = fit_under_ceiling(garbage, ImageMediaType::PNG, CEILING_5_MIB)
            .expect_err("garbage over the ceiling must fail");
        assert!(matches!(err, FitError::Undecodable), "got: {err}");
        // The error must not echo the payload.
        let msg = err.to_string();
        assert!(!msg.contains('\u{FFFD}'), "raw bytes echoed: {msg}");
        assert!(msg.len() < 100, "payload echoed: {} chars", msg.len());
    }

    #[test]
    fn lying_extension_decodes_by_magic_bytes_and_encodes_to_declared_type() {
        // Real JPEG bytes declared as PNG: decoded by magic, re-encoded as PNG,
        // so the mimeType stops lying.
        let img = noise_rgb(4128, 2208, 0x1111_2222);
        let raw = img.into_raw();
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 100)
            .encode(&raw, 4128, 2208, image::ExtendedColorType::Rgb8)
            .expect("encode jpeg");
        assert!(
            base64_len(jpeg.len()) > 1024 * 1024,
            "fixture must start over the ceiling: {} bytes",
            jpeg.len()
        );

        let fitted = fit_under_ceiling(jpeg, ImageMediaType::PNG, 1024 * 1024).expect("must fit");
        assert!(
            fitted.bytes.starts_with(b"\x89PNG"),
            "output must be a real PNG"
        );
        image::load_from_memory(&fitted.bytes).expect("output must decode");
    }

    #[test]
    fn gif_and_webp_over_ceiling_are_refused_with_a_reason() {
        // 1×1 GIF89a with a valid LZW stream (clear, pixel 0, EOI); the gif
        // codec is not compiled in, so the refusal must come from the declared
        // media type, not from decoding.
        let gif = [
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, // "GIF89a"
            0x01, 0x00, 0x01, 0x00, // 1×1
            0x80, 0x00, 0x00, // GCT, 2 colors
            0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01,
            0x00, 0x00, // image descriptor
            0x02, 0x01, 0x44, 0x00, // LZW: clear, pixel 0, EOI
            0x3B, // trailer
        ];
        // RIFF/WEBP magic with an inert VP8L payload.
        let webp: [u8; 32] = [
            0x52, 0x49, 0x46, 0x46, 0x24, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50,
            0x38, 0x4C, 0x24, 0x00, 0x00, 0x00, 0x2F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];

        let gif_err = fit_under_ceiling(gif.to_vec(), ImageMediaType::GIF, 8)
            .expect_err("GIF over the ceiling must be refused");
        assert!(
            matches!(gif_err, FitError::UnsupportedFormat(_)),
            "got: {gif_err}"
        );
        assert!(
            gif_err.to_string().contains("animation"),
            "GIF refusal must explain why: {gif_err}"
        );

        let webp_err = fit_under_ceiling(webp.to_vec(), ImageMediaType::WEBP, 8)
            .expect_err("WebP over the ceiling must be refused");
        assert!(
            matches!(webp_err, FitError::UnsupportedFormat(_)),
            "got: {webp_err}"
        );
    }

    #[test]
    fn halving_terminates_on_an_impossible_ceiling() {
        // A ceiling of 8 base64 bytes is unattainable; the halving loop must
        // still terminate (edge < 64 or 6 steps) and report CannotFit.
        let img = RgbImage::from_pixel(2000, 2000, Rgb([120, 30, 200]));
        let png = encode_png(&img);
        let start = Instant::now();
        let err =
            fit_under_ceiling(png, ImageMediaType::PNG, 8).expect_err("ceiling 8 is impossible");
        let elapsed = start.elapsed();
        assert!(matches!(err, FitError::CannotFit { .. }), "got: {err}");
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    }
}
