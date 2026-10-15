use std::time::Duration;

use anyhow::{Context, Result, ensure};

pub struct Frame {
    pub rgba: Vec<u8>,
    pub delay: Duration,
}

pub struct Animation {
    pub width: usize,
    pub height: usize,
    pub frames: Vec<Frame>,
}

fn decoder(bytes: &[u8]) -> Result<gif::Decoder<&[u8]>> {
    ensure!(
        bytes.len() <= 25 * 1024 * 1024,
        "preview exceeds the file limit"
    );
    ensure!(
        bytes.last() == Some(&0x3b),
        "preview is missing its trailer"
    );
    let mut options = gif::DecodeOptions::new();
    options.set_memory_limit(gif::MemoryLimit::Bytes(1_000_000.try_into().unwrap()));
    options.check_frame_consistency(true);
    options.read_info(bytes).context("invalid preview header")
}

pub fn decode(bytes: &[u8]) -> Result<Animation> {
    ensure!(
        bytes.len() <= 25 * 1024 * 1024,
        "preview exceeds the file limit"
    );
    match crate::media::Format::detect(bytes)? {
        crate::media::Format::Gif => decode_gif(bytes),
        crate::media::Format::WebP => decode_webp(bytes),
    }
}

fn decode_webp(bytes: &[u8]) -> Result<Animation> {
    let mut reader = crate::webp::decoder(
        bytes,
        crate::webp::Limits {
            canvas_pixels: 1_000_000,
            total_pixels: 40_000_000,
            frames: 500,
        },
    )?;
    if reader.is_animated() {
        // use a transparent canvas, including each frame's dispose-to-background operation.
        reader.set_background_color([0; 4])?;
    }
    let (width, height) = reader.dimensions();
    let count = reader.num_frames().max(1) as usize;
    let step = count.div_ceil(48);
    let scale = (160.0 / f64::from(width))
        .min(120.0 / f64::from(height))
        .min(1.0);
    let target_width = ((f64::from(width) * scale) as usize).max(1);
    let target_height = ((f64::from(height) * scale) as usize).max(1);
    let channels = if reader.has_alpha() { 4 } else { 3 };
    let mut pixels = vec![
        0;
        reader
            .output_buffer_size()
            .context("webp preview exceeds the output limit")?
    ];
    let mut frames: Vec<Frame> = Vec::new();
    for index in 0..count {
        let milliseconds = if reader.is_animated() {
            reader
                .read_frame(&mut pixels)
                .context("invalid webp preview frame")?
        } else {
            reader
                .read_image(&mut pixels)
                .context("invalid webp preview")?;
            100
        };
        let delay = Duration::from_millis(u64::from(milliseconds).max(20));
        if index % step == 0 {
            let mut rgba = Vec::with_capacity(target_width * target_height * 4);
            for y in 0..target_height {
                for x in 0..target_width {
                    let offset = ((y * height as usize / target_height) * width as usize
                        + x * width as usize / target_width)
                        * channels;
                    rgba.extend_from_slice(&pixels[offset..offset + 3]);
                    rgba.push(if channels == 4 {
                        pixels[offset + 3]
                    } else {
                        255
                    });
                }
            }
            frames.push(Frame { rgba, delay });
        } else if let Some(previous) = frames.last_mut() {
            previous.delay += delay;
        }
    }
    Ok(Animation {
        width: target_width,
        height: target_height,
        frames,
    })
}

fn decode_gif(bytes: &[u8]) -> Result<Animation> {
    let mut reader = decoder(bytes)?;
    let width = usize::from(reader.width());
    let height = usize::from(reader.height());
    let canvas = width * height;
    ensure!(
        canvas > 0 && canvas <= 1_000_000,
        "preview canvas exceeds the pixel limit"
    );
    let mut count = 0usize;
    while reader
        .read_next_frame()
        .context("invalid preview frame")?
        .is_some()
    {
        count += 1;
        ensure!(
            count <= 500 && count * canvas <= 40_000_000,
            "preview exceeds the decoding limit"
        );
    }
    ensure!(count > 0, "preview has no frames");
    // retain at most 48 small composited frames, preserving skipped-frame time.
    let step = count.div_ceil(48);
    let scale = (160.0 / width as f64).min(120.0 / height as f64).min(1.0);
    let target_width = ((width as f64 * scale) as usize).max(1);
    let target_height = ((height as f64 * scale) as usize).max(1);
    let mut reader = decoder(bytes)?;
    let mut screen = gif_dispose::Screen::new_decoder(&reader);
    let mut frames: Vec<Frame> = Vec::new();
    let mut index = 0;
    while let Some(frame) = reader.read_next_frame().context("invalid preview frame")? {
        let delay = Duration::from_millis(u64::from(frame.delay).max(2) * 10);
        screen
            .blit_frame(frame)
            .context("could not composite preview")?;
        if index % step == 0 {
            let pixels = screen.pixels_rgba();
            let source = pixels.buf();
            let mut rgba = Vec::with_capacity(target_width * target_height * 4);
            for y in 0..target_height {
                for x in 0..target_width {
                    let pixel =
                        source[(y * height / target_height) * width + x * width / target_width];
                    rgba.extend_from_slice(&[pixel.r, pixel.g, pixel.b, pixel.a]);
                }
            }
            frames.push(Frame { rgba, delay });
        } else if let Some(previous) = frames.last_mut() {
            previous.delay += delay;
        }
        index += 1;
    }
    Ok(Animation {
        width: target_width,
        height: target_height,
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_media as webp;

    #[test]
    fn webp_motion_timing_alpha_and_sampling_are_preserved() {
        let animation = decode(&webp::animation(2, 1, 2)).unwrap();
        assert_eq!((animation.width, animation.height), (2, 1));
        assert_eq!(animation.frames.len(), 2);
        assert_eq!(animation.frames[0].rgba, [255, 0, 0, 255].repeat(2));
        assert_eq!(animation.frames[1].rgba, [0, 255, 0, 128].repeat(2));
        assert_eq!(animation.frames[0].delay, Duration::from_millis(100));
        assert_eq!(animation.frames[1].delay, Duration::from_millis(200));
        let animation = decode(&webp::animation(2, 1, 100)).unwrap();
        assert!(animation.frames.len() <= 48);
        assert_eq!(
            animation
                .frames
                .iter()
                .map(|frame| frame.delay)
                .sum::<Duration>(),
            Duration::from_secs(15)
        );
        let still = decode(&webp::still(320, 240, [20, 40, 60, 255])).unwrap();
        assert_eq!((still.width, still.height), (160, 120));
        assert_eq!(still.frames[0].rgba, [20, 40, 60, 255].repeat(160 * 120));
    }

    #[test]
    fn webp_subframes_blend_and_dispose_on_the_canvas() {
        let mut chunks = webp::header(2, 1);
        chunks.extend_from_slice(&webp::frame(2, 1, [255, 0, 0, 255], 100, 2));
        // blend a half-transparent pixel over red, then clear that subrectangle.
        chunks.extend_from_slice(&webp::frame(1, 1, [0, 255, 0, 128], 100, 1));
        chunks.extend_from_slice(&webp::frame(1, 1, [0, 0, 255, 128], 100, 0));
        let animation = decode(&webp::riff(&chunks)).unwrap();
        assert_eq!(&animation.frames[1].rgba[4..], &[255, 0, 0, 255]);
        assert_eq!(animation.frames[1].rgba[3], 255);
        assert!(animation.frames[1].rgba[0] > 100 && animation.frames[1].rgba[1] > 100);
        assert_eq!(&animation.frames[2].rgba[..4], &[0, 0, 255, 128]);
        assert_eq!(&animation.frames[2].rgba[4..], &[255, 0, 0, 255]);
    }

    #[test]
    fn webp_previews_reject_oversized_or_truncated_animations() {
        for bytes in [
            webp::animation(1, 1, 501),
            webp::animation(1024, 1024, 1),
            webp::animation(1000, 1000, 41),
        ] {
            assert!(decode(&bytes).is_err());
        }
        let mut bytes = webp::animation(2, 1, 2);
        bytes.pop();
        assert!(decode(&bytes).is_err());
    }

    fn animation(frames: usize) -> Vec<u8> {
        sized_animation(2, 1, frames)
    }

    fn sized_animation(width: u16, height: u16, frames: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder =
                gif::Encoder::new(&mut bytes, width, height, &[0, 0, 0, 255, 255, 255]).unwrap();
            for index in 0..frames {
                let frame = gif::Frame {
                    width,
                    height,
                    delay: 10,
                    buffer: vec![(index % 2) as u8; usize::from(width) * usize::from(height)]
                        .into(),
                    ..Default::default()
                };
                encoder.write_frame(&frame).unwrap();
            }
        }
        bytes
    }

    #[test]
    fn decodes_motion_and_bounds_retained_frames_without_losing_duration() {
        let decoded = decode(&animation(100)).unwrap();
        assert!(decoded.frames.len() <= 48);
        assert_eq!(
            decoded
                .frames
                .iter()
                .map(|frame| frame.delay)
                .sum::<Duration>(),
            Duration::from_secs(10)
        );
        let decoded = decode(&animation(2)).unwrap();
        assert_ne!(decoded.frames[0].rgba, decoded.frames[1].rgba);
        assert_eq!(decoded.frames[0].rgba.len(), 8);
    }

    #[test]
    fn downsampling_and_decoding_limits_apply_before_retaining_frames() {
        let animation = decode(&sized_animation(320, 240, 2)).unwrap();
        assert_eq!((animation.width, animation.height), (160, 120));
        assert_eq!(animation.frames[0].rgba.len(), 160 * 120 * 4);
        assert!(
            decode(&sized_animation(1, 1, 501))
                .err()
                .unwrap()
                .to_string()
                .contains("decoding limit")
        );
        assert!(
            decode(&sized_animation(1000, 1000, 41))
                .err()
                .unwrap()
                .to_string()
                .contains("decoding limit")
        );
        assert!(
            decode(&vec![0; 25 * 1024 * 1024 + 1])
                .err()
                .unwrap()
                .to_string()
                .contains("file limit")
        );
    }

    #[test]
    fn rejects_invalid_or_oversized_canvases() {
        let mut bytes = animation(2);
        bytes[6..10].fill(255);
        assert!(decode(&bytes).is_err());
        assert!(decode(b"not a gif").is_err());
        let mut bytes = animation(2);
        bytes.pop();
        assert!(decode(&bytes).is_err());
    }
}
