//! Write new reviewed shell goldens and PNGs, never rewrite an existing oracle.
use std::io::Write;
#[path = "../tests/support/shell_chrome.rs"]
mod fixture;

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [flag, directory] = args.as_slice() else { panic!("usage: shell_chrome_goldens --write NEW_DIRECTORY") };
    assert_eq!(flag, "--write");
    let directory = std::path::Path::new(directory);
    std::fs::create_dir(directory).expect("output must be a new directory");
    let file = std::fs::File::create(directory.join("shell-chrome.bin.zlib")).unwrap();
    let mut compressed = flate2::write::ZlibEncoder::new(file, flate2::Compression::best());
    fixture::cases(|name, image, geometry| {
        for bytes in [name.as_bytes(), geometry, image.width.to_le_bytes().as_slice(),
            image.height.to_le_bytes().as_slice(), image.pixels.as_slice()] {
            compressed.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
            compressed.write_all(bytes).unwrap();
        }
        let pixmap = tiny_skia::PixmapRef::from_bytes(&image.pixels, image.width, image.height).unwrap();
        pixmap.save_png(directory.join(format!("{name}.png"))).unwrap();
    });
    compressed.finish().unwrap();
}
