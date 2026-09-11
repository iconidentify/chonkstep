//! Explicit offline oracle generation; never invoked by tests or ordinary builds.
use std::io::Write;

#[path = "../tests/support/windowmaker_fixture.rs"]
mod fixture;

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [flag, output] = args.as_slice() else {
        eprintln!("usage: decoration_goldens --write NEW_DIRECTORY");
        std::process::exit(2);
    };
    if flag != "--write" {
        eprintln!("generation requires the explicit --write flag");
        std::process::exit(2);
    }
    let output = std::path::Path::new(output);
    std::fs::create_dir(output).expect("output must be a new directory; existing fixtures are never overwritten");
    let file = std::fs::File::create(output.join("windowmaker.bin.zlib")).unwrap();
    let mut compressed = flate2::write::ZlibEncoder::new(file, flate2::Compression::best());
    compressed.write_all(b"CHONK-WM-GOLDEN-1\n").unwrap();
    let mut names = String::new();
    fixture::cases(|name, layout, surface| {
        let bytes = fixture::encoded(layout, surface);
        compressed.write_all(&(name.len() as u32).to_le_bytes()).unwrap();
        compressed.write_all(name.as_bytes()).unwrap();
        compressed.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
        compressed.write_all(&bytes).unwrap();
        names.push_str(name);
        names.push('\n');
    });
    compressed.finish().unwrap();
    std::fs::write(output.join("cases.txt"), names).unwrap();
}
