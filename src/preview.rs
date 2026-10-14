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
        bytes.len() <= 2 * 1024 * 1024,
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

    fn animation(frames: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder =
                gif::Encoder::new(&mut bytes, 2, 1, &[0, 0, 0, 255, 255, 255]).unwrap();
            for index in 0..frames {
                let frame = gif::Frame {
                    width: 2,
                    height: 1,
                    delay: 10,
                    buffer: vec![(index % 2) as u8, 1].into(),
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
