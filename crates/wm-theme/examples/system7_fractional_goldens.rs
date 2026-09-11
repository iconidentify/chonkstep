//! Explicit implementation-golden writer for fractional scales. This never
//! writes or replaces the separate, OS-captured 1x acceptance oracles.
use std::io::Write;
#[path = "../tests/support/system7_fractional.rs"]
mod fixture;

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [flag, directory] = args.as_slice() else { panic!("usage: system7_fractional_goldens --write NEW_DIRECTORY") };
    assert_eq!(flag, "--write");
    let directory = std::path::Path::new(directory);
    std::fs::create_dir(directory).expect("output must be a new directory");
    let file = std::fs::File::create(directory.join("fractional.bin.zlib")).unwrap();
    let mut compressed = flate2::write::ZlibEncoder::new(file, flate2::Compression::best());
    fixture::cases(|name, pixels| {
        for bytes in [name.as_bytes(), pixels.as_slice()] {
            compressed.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
            compressed.write_all(bytes).unwrap();
        }
    });
    compressed.finish().unwrap();
}
