//! Reviewed shell surface output and hit geometry, at 1x, 1.5x and 2x.
use std::io::Read;
#[path = "support/shell_chrome.rs"]
mod fixture;

#[test]
fn shell_surfaces_match_goldens_and_windowmaker_matches_legacy_apis() {
    let mut oracle = flate2::read::ZlibDecoder::new(&include_bytes!("fixtures/shell-chrome/shell-chrome.bin.zlib")[..]);
    let mut count = 0;
    fixture::cases(|name, image, geometry| {
        for actual in [name.as_bytes(), geometry, image.width.to_le_bytes().as_slice(),
            image.height.to_le_bytes().as_slice(), image.pixels.as_slice()] {
            let mut len = [0; 4];
            oracle.read_exact(&mut len).unwrap();
            let len = u32::from_le_bytes(len) as usize;
            assert!(len <= 10_000_000);
            let mut expected = vec![0; len];
            oracle.read_exact(&mut expected).unwrap();
            assert_eq!(actual.len(), len, "{name}: storage/geometry changed");
            if let Some(index) = actual.iter().zip(expected).position(|(a, b)| *a != b) {
                panic!("{name}: golden byte {index} changed");
            }
        }
        count += 1;
    });
    assert_eq!(count, 120);
    assert_eq!(oracle.read(&mut [0; 1]).unwrap(), 0);
}
