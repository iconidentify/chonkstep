//! Byte-for-byte geometry and sparse-pixel compatibility with main before the seam.
use std::io::Read;

#[path = "support/windowmaker_fixture.rs"]
mod fixture;

#[test]
fn windowmaker_matches_every_pre_refactor_layout_and_pixel() {
    let mut golden = flate2::read::ZlibDecoder::new(
        &include_bytes!("fixtures/windowmaker/windowmaker.bin.zlib")[..]);
    let mut magic = [0; 18];
    golden.read_exact(&mut magic).unwrap();
    assert_eq!(&magic, b"CHONK-WM-GOLDEN-1\n");
    let mut count = 0;
    fixture::cases(|name, layout, surface| {
        let mut expected_name = vec![0; read_length(&mut golden)];
        golden.read_exact(&mut expected_name).unwrap();
        assert_eq!(name.as_bytes(), expected_name, "case order/matrix changed; explain and regenerate deliberately");
        let mut expected = vec![0; read_length(&mut golden)];
        golden.read_exact(&mut expected).unwrap();
        let actual = fixture::encoded(layout, surface);
        assert_eq!(actual.len(), expected.len(), "{name}: layout/part storage changed");
        if let Some(index) = actual.iter().zip(&expected).position(|(a, b)| a != b) {
            panic!("{name}: byte {index} changed from {} to {}", expected[index], actual[index]);
        }
        count += 1;
    });
    assert_eq!(count, 720);
    assert_eq!(golden.read(&mut [0; 1]).unwrap(), 0, "unvisited golden cases");
}

fn read_length(reader: &mut impl Read) -> usize {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes).unwrap();
    let length = u32::from_le_bytes(bytes) as usize;
    assert!(length <= 1_000_000, "invalid fixture record length");
    length
}
