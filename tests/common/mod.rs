#![allow(dead_code)]

pub fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_vec();
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(data);
    if data.len() % 2 == 1 {
        bytes.push(0);
    }
    bytes
}

pub fn riff(chunks: &[u8]) -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(chunks.len() as u32 + 4).to_le_bytes());
    bytes.extend_from_slice(b"WEBP");
    bytes.extend_from_slice(chunks);
    bytes
}

pub fn u24(value: u32) -> [u8; 3] {
    value.to_le_bytes()[..3].try_into().unwrap()
}

pub fn still(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    image_webp::WebPEncoder::new(&mut bytes)
        .encode(
            &pixel.repeat((width * height) as usize),
            width,
            height,
            image_webp::ColorType::Rgba8,
        )
        .unwrap();
    bytes
}

pub fn frame(width: u32, height: u32, pixel: [u8; 4], delay: u32, flags: u8) -> Vec<u8> {
    let image = still(width, height, pixel);
    let mut frame = vec![0; 6];
    frame.extend_from_slice(&u24(width - 1));
    frame.extend_from_slice(&u24(height - 1));
    frame.extend_from_slice(&u24(delay));
    frame.push(flags);
    frame.extend_from_slice(&image[12..]);
    chunk(b"ANMF", &frame)
}

pub fn header(width: u32, height: u32) -> Vec<u8> {
    let mut extended = vec![0x12, 0, 0, 0];
    extended.extend_from_slice(&u24(width - 1));
    extended.extend_from_slice(&u24(height - 1));
    let mut chunks = chunk(b"VP8X", &extended);
    chunks.extend_from_slice(&chunk(b"ANIM", &[0; 6]));
    chunks
}

pub fn animation(width: u32, height: u32, count: usize) -> Vec<u8> {
    let mut chunks = header(width, height);
    let frames = [
        frame(width, height, [255, 0, 0, 255], 100, 2),
        frame(width, height, [0, 255, 0, 128], 200, 2),
    ];
    for index in 0..count {
        chunks.extend_from_slice(&frames[index % 2]);
    }
    riff(&chunks)
}
