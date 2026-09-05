//! Compare the already-linked PNG decoders on identical embedded artwork.
//! Run with `cargo run --release -p chonk-shell --example wallpaper_decode`.

use std::hint::black_box;
use std::time::Instant;
use tiny_skia::{IntSize, Pixmap};

fn image_decode(bytes: &[u8]) -> Pixmap {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .unwrap().into_rgba8();
    let size = IntSize::from_wh(image.width(), image.height()).unwrap();
    let mut pixels = image.into_raw();
    for [r, g, b, a] in pixels.as_chunks_mut::<4>().0 {
        if *a != 255 {
            for channel in [r, g, b] {
                *channel = ((*channel as u16 * *a as u16 + 127) / 255) as u8;
            }
        }
    }
    Pixmap::from_vec(pixels, size).unwrap()
}

fn measure(label: &str, bytes: &[u8], decode: impl Fn(&[u8]) -> Pixmap) {
    let image = decode(bytes);
    let checksum = image.data().iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    drop(image);
    let started = Instant::now();
    for _ in 0..25 {
        black_box(decode(black_box(bytes)));
    }
    println!("{{\"decoder\":\"{label}\",\"iterations\":25,\"ns_per_decode\":{},\"checksum\":\"{checksum:016x}\"}}",
        started.elapsed().as_nanos() / 25);
}

fn main() {
    let bytes = include_bytes!("../assets/wallpapers/lavender-grid.png");
    assert_eq!(Pixmap::decode_png(bytes).unwrap(), image_decode(bytes));
    for round in 0_usize..5 {
        if round.is_multiple_of(2) {
            measure("tiny-skia-png", bytes, |bytes| Pixmap::decode_png(bytes).unwrap());
            measure("image-png", bytes, image_decode);
        } else {
            measure("image-png", bytes, image_decode);
            measure("tiny-skia-png", bytes, |bytes| Pixmap::decode_png(bytes).unwrap());
        }
    }
}
