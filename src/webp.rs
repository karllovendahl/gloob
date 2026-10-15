use std::io::Cursor;

use anyhow::{Context, Result, ensure};
use image_webp::WebPDecoder;

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub canvas_pixels: u64,
    pub total_pixels: u64,
    pub frames: u32,
}

pub(crate) const COPY_LIMITS: Limits = Limits {
    canvas_pixels: 16_000_000,
    total_pixels: 100_000_000,
    frames: 1_000,
};

// check container boundaries and every embedded bitstream before decoding. the codec's
// memory limit does not cover all allocations, including vp8's declared frame buffers.
pub(crate) fn decoder(bytes: &[u8], limits: Limits) -> Result<WebPDecoder<Cursor<&[u8]>>> {
    ensure!(
        bytes.len() <= 25 * 1024 * 1024,
        "webp exceeds the 25 mib size limit"
    );
    ensure!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "file is not a webp"
    );
    let size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    ensure!(
        size.checked_add(8) == Some(bytes.len()),
        "invalid webp container length"
    );
    let mut canvas = None;
    let mut frames = 0u32;
    chunks(&bytes[12..], |tag, data| {
        match tag {
            b"VP8X" => {
                ensure!(
                    data.len() == 10 && canvas.is_none(),
                    "invalid webp extended header"
                );
                let dimensions = (u24(&data[4..7]) + 1, u24(&data[7..10]) + 1);
                check_canvas(dimensions, limits)?;
                canvas = Some(dimensions);
            }
            b"ANMF" => {
                ensure!(data.len() >= 16, "truncated webp frame header");
                frames += 1;
                ensure!(frames <= limits.frames, "webp exceeds the decoding limit");
                let (width, height) = canvas.context("webp animation has no canvas")?;
                let x = u24(&data[..3]) * 2;
                let y = u24(&data[3..6]) * 2;
                let frame = (u24(&data[6..9]) + 1, u24(&data[9..12]) + 1);
                ensure!(
                    x + frame.0 <= width && y + frame.1 <= height,
                    "webp frame exceeds its canvas"
                );
                ensure!(
                    u64::from(width) * u64::from(height) * u64::from(frames) <= limits.total_pixels,
                    "webp exceeds the decoding limit"
                );
                let mut images = 0;
                let mut alpha = false;
                chunks(&data[16..], |tag, data| {
                    if tag == b"ALPH" {
                        ensure!(!alpha && images == 0, "invalid webp alpha chunk order");
                        alpha = true;
                    } else {
                        // the codec's alpha path decodes the next chunk as vp8 regardless of tag.
                        // only the two specified frame layouts may reach that decoder.
                        ensure!(
                            tag == b"VP8 " || (!alpha && tag == b"VP8L"),
                            "unsupported webp frame chunk"
                        );
                        ensure!(images == 0, "webp frame must contain one image");
                        let dimensions = image_dimensions(tag, data)?.unwrap();
                        ensure!(
                            dimensions == frame,
                            "webp frame dimensions do not match its bitstream"
                        );
                        images += 1;
                    }
                    Ok(())
                })?;
                ensure!(images == 1, "webp frame must contain one image");
            }
            _ => {
                if let Some(dimensions) = image_dimensions(tag, data)? {
                    check_canvas(dimensions, limits)?;
                    if let Some(canvas) = canvas {
                        ensure!(
                            canvas == dimensions,
                            "webp canvas dimensions do not match its bitstream"
                        );
                    } else {
                        canvas = Some(dimensions);
                    }
                }
            }
        }
        Ok(())
    })?;
    ensure!(canvas.is_some(), "webp contains no image");
    let mut decoder = WebPDecoder::new(Cursor::new(bytes)).context("invalid webp header")?;
    check_canvas(decoder.dimensions(), limits)?;
    ensure!(
        frames == decoder.num_frames(),
        "inconsistent webp animation frames"
    );
    let count = decoder.num_frames().max(1);
    let (width, height) = decoder.dimensions();
    ensure!(
        count <= limits.frames
            && u64::from(width) * u64::from(height) * u64::from(count) <= limits.total_pixels,
        "webp exceeds the decoding limit"
    );
    decoder.set_memory_limit(64 * 1024 * 1024);
    Ok(decoder)
}

fn check_canvas((width, height): (u32, u32), limits: Limits) -> Result<()> {
    ensure!(
        width > 0 && height > 0 && u64::from(width) * u64::from(height) <= limits.canvas_pixels,
        "webp canvas exceeds the pixel limit"
    );
    Ok(())
}

fn image_dimensions(tag: &[u8], data: &[u8]) -> Result<Option<(u32, u32)>> {
    match tag {
        b"VP8 " => {
            ensure!(
                data.len() >= 10 && data[0] & 1 == 0 && &data[3..6] == b"\x9d\x01\x2a",
                "invalid webp vp8 header"
            );
            Ok(Some((
                u32::from(u16::from_le_bytes(data[6..8].try_into().unwrap()) & 0x3fff),
                u32::from(u16::from_le_bytes(data[8..10].try_into().unwrap()) & 0x3fff),
            )))
        }
        b"VP8L" => {
            ensure!(
                data.len() >= 5 && data[0] == 0x2f,
                "invalid webp lossless header"
            );
            let bits = u32::from_le_bytes(data[1..5].try_into().unwrap());
            Ok(Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)))
        }
        _ => Ok(None),
    }
}

fn u24(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
}

fn chunks(mut bytes: &[u8], mut visit: impl FnMut(&[u8], &[u8]) -> Result<()>) -> Result<()> {
    while !bytes.is_empty() {
        ensure!(bytes.len() >= 8, "truncated webp chunk header");
        let length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let end = 8usize
            .checked_add(length)
            .context("invalid webp chunk length")?;
        let padded = end
            .checked_add(length & 1)
            .context("invalid webp padding")?;
        ensure!(padded <= bytes.len(), "truncated webp chunk");
        visit(&bytes[..4], &bytes[8..end])?;
        bytes = &bytes[padded..];
    }
    Ok(())
}

pub(crate) fn validate(bytes: &[u8]) -> Result<()> {
    let mut reader = decoder(bytes, COPY_LIMITS)?;
    let size = reader
        .output_buffer_size()
        .context("webp output exceeds the size limit")?;
    let mut pixels = vec![0; size];
    if reader.is_animated() {
        for _ in 0..reader.num_frames() {
            reader
                .read_frame(&mut pixels)
                .context("invalid webp frame")?;
        }
    } else {
        reader
            .read_image(&mut pixels)
            .context("invalid webp image")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_media as fixtures;

    #[test]
    fn rejects_oversized_embedded_vp8_before_codec_allocation() {
        let mut chunks = fixtures::header(1, 1);
        let mut frame = vec![0; 16];
        // the frame claims one pixel, but the bitstream would allocate a huge canvas.
        frame.extend_from_slice(&fixtures::chunk(
            b"VP8 ",
            &[0, 0, 0, 0x9d, 1, 0x2a, 255, 63, 255, 63],
        ));
        chunks.extend_from_slice(&fixtures::chunk(b"ANMF", &frame));
        let error = validate(&fixtures::riff(&chunks)).unwrap_err();
        assert!(error.to_string().contains("bitstream"));
    }

    #[test]
    fn rejects_alpha_frames_with_hidden_unchecked_bitstreams() {
        let mut frame = vec![0; 16];
        frame.extend_from_slice(&fixtures::chunk(b"ALPH", &[0, 255]));
        frame.extend_from_slice(&fixtures::chunk(
            b"JUNK",
            &[0, 0, 0, 0x9d, 1, 0x2a, 255, 63, 255, 63],
        ));
        let valid = fixtures::still(1, 1, [1, 2, 3, 255]);
        frame.extend_from_slice(&valid[12..]);
        for prefix in [Vec::new(), fixtures::frame(1, 1, [1, 2, 3, 255], 100, 2)] {
            let mut chunks = fixtures::header(1, 1);
            chunks.extend_from_slice(&prefix);
            chunks.extend_from_slice(&fixtures::chunk(b"ANMF", &frame));
            // inspect only the header: the unchecked payload must never reach pixel allocation.
            let result = decoder(&fixtures::riff(&chunks), COPY_LIMITS).map(|_| ());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported webp frame chunk")
            );
        }
    }

    #[test]
    fn rejects_corrupt_compressed_data_and_container_lengths() {
        let mut chunks = fixtures::header(2, 1);
        chunks.extend_from_slice(&fixtures::frame(2, 1, [1, 2, 3, 255], 100, 2));
        let mut second = fixtures::frame(2, 1, [4, 5, 6, 255], 100, 2);
        // keep the bitstream header intact but remove the compressed pixel data.
        second.truncate(8 + 16 + 8 + 5);
        second.push(0);
        let size = (second.len() - 8) as u32;
        second[4..8].copy_from_slice(&size.to_le_bytes());
        second[28..32].copy_from_slice(&5u32.to_le_bytes());
        chunks.extend_from_slice(&second);
        let error = validate(&fixtures::riff(&chunks)).unwrap_err();
        assert!(
            error.to_string().contains("invalid webp frame"),
            "{error:#}"
        );
        let mut still = fixtures::still(2, 1, [0, 0, 0, 255]);
        still.push(0);
        assert!(
            validate(&still)
                .unwrap_err()
                .to_string()
                .contains("container length")
        );
    }
}
