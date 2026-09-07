use super::*;

fn fixture(width: u32, height: u32, noise: bool) -> DecorationBuffer {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    let mut seed = 0x341f_29a7u32;
    for y in 0..height {
        for x in 0..width {
            if noise {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                pixels.extend_from_slice(&[seed as u8, (seed >> 8) as u8, (seed >> 16) as u8, 255]);
            } else {
                pixels.extend_from_slice(&[
                    (x % 256) as u8,
                    (y % 256) as u8,
                    ((x + y) % 256) as u8,
                    255,
                ]);
            }
        }
    }
    DecorationBuffer {
        width,
        height,
        pixels,
    }
}

fn previous_png(pixels: DecorationBuffer) -> Vec<u8> {
    let size = tiny_skia::IntSize::from_wh(pixels.width, pixels.height).unwrap();
    tiny_skia::Pixmap::from_vec(pixels.pixels, size)
        .unwrap()
        .encode_png()
        .unwrap()
}

fn decoded_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    pixels.truncate(info.buffer_size());
    (info.width, info.height, pixels)
}

fn idat_chunks(bytes: &[u8]) -> Vec<(usize, usize)> {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let mut chunks = Vec::new();
    let mut offset = 8;
    while offset < bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        if &bytes[offset + 4..offset + 8] == b"IDAT" {
            chunks.push((offset, length));
        }
        offset += length + 12;
    }
    assert_eq!(offset, bytes.len());
    chunks
}

fn idat_payload(bytes: &[u8]) -> Vec<u8> {
    idat_chunks(bytes)
        .into_iter()
        .flat_map(|(offset, length)| bytes[offset + 8..offset + 8 + length].iter().copied())
        .collect()
}

#[test]
fn joined_sub_rows_preserve_the_original_compressed_stream_across_idat_boundaries() {
    for width in [1, 2, 7, 256, 513] {
        let input = fixture(width, 83, false);
        let previous = previous_png(input.clone());
        let expected = idat_payload(&previous);
        // The old whole-image encoder can choose stored/unfiltered output
        // for very narrow images. Every original Sub stream must match byte
        // for byte; the stored fallback still supplies a decoded-pixel oracle.
        let original_sub = fdeflate::decompress_to_vec(&expected).unwrap()[0] == 1;
        assert!(original_sub || width <= 2);
        let size = tiny_skia::IntSize::from_wh(input.width, input.height).unwrap();
        let image = tiny_skia::Pixmap::from_vec(input.pixels, size).unwrap();
        for chunk_bytes in [1, 7, 64 * 1024] {
            let mut output = Vec::new();
            let mut encoder = png::Encoder::new(&mut output, image.width(), image.height());
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            write_png_rows(&image, &mut writer, chunk_bytes).unwrap();
            writer.finish().unwrap();
            let compressed = idat_payload(&output);
            if original_sub {
                assert_eq!(
                    compressed, expected,
                    "compressed Sub stream changed: width={width}, IDAT capacity={chunk_bytes}"
                );
            }
            assert!(fdeflate::decompress_to_vec(&compressed)
                .unwrap()
                .chunks_exact(width as usize * 4 + 1)
                .all(|row| row[0] == 1));
            assert_eq!(decoded_png(&output), decoded_png(&previous));
            assert!(idat_chunks(&output)
                .iter()
                .all(|(_, length)| *length <= chunk_bytes));
        }
    }
}

#[test]
fn row_adapter_latches_header_body_checksum_and_final_partial_chunk_errors() {
    #[derive(Default)]
    struct Fault {
        written: usize,
        fail_at: usize,
        failed: bool,
        retries: usize,
    }
    struct FaultWriter(std::rc::Rc<std::cell::RefCell<Fault>>);
    impl Write for FaultWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let mut fault = self.0.borrow_mut();
            if fault.failed {
                fault.retries += 1;
                return Ok(bytes.len()); // A later retry could appear successful.
            }
            if fault.written == fault.fail_at {
                fault.failed = true;
                return Err(std::io::Error::other("injected IDAT boundary failure"));
            }
            let count = bytes.len().min(fault.fail_at - fault.written);
            fault.written += count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let input = fixture(17, 3, true);
    let image = tiny_skia::Pixmap::from_vec(
        input.pixels,
        tiny_skia::IntSize::from_wh(input.width, input.height).unwrap(),
    )
    .unwrap();
    for chunk_bytes in [1, 7, 64 * 1024] {
        let mut complete = Vec::new();
        let mut encoder = png::Encoder::new(&mut complete, image.width(), image.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        write_png_rows(&image, &mut writer, chunk_bytes).unwrap();
        writer.finish().unwrap();
        let chunks = idat_chunks(&complete);
        let mut selected = vec![0, chunks.len() / 2];
        // With capacity=1 these cover fdeflate's header, ordinary writes,
        // terminal bits, and every Adler32 checksum byte. Larger capacities
        // also exercise the adapter's explicit final partial-chunk flush.
        selected.extend(chunks.len().saturating_sub(6)..chunks.len());
        selected.sort_unstable();
        selected.dedup();
        for index in selected {
            let (offset, length) = chunks[index];
            for fail_at in [
                offset,
                offset + 4,
                offset + 8,
                offset + 8 + length,
                offset + 11 + length,
            ] {
                let fault = std::rc::Rc::new(std::cell::RefCell::new(Fault {
                    fail_at,
                    ..Default::default()
                }));
                let mut encoder =
                    png::Encoder::new(FaultWriter(fault.clone()), image.width(), image.height());
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().unwrap();
                let result = write_png_rows(&image, &mut writer, chunk_bytes);
                assert!(
                    fault.borrow().failed,
                    "fault at {fail_at} was not exercised"
                );
                assert!(result
                    .unwrap_err()
                    .contains("injected IDAT boundary failure"));
                assert_eq!(
                    fault.borrow().retries,
                    0,
                    "poisoned IDAT output must stop immediately"
                );
                // Drop may attempt IEND; production's CheckedWriter also
                // poisons that path, and publication already sees an error.
            }
        }
    }
}

#[test]
fn streaming_png_preserves_every_alpha_value_and_tiny_skia_rounding() {
    let mut pixels = Vec::with_capacity(256 * 256 * 4);
    for alpha in 0..=255u8 {
        for channel in 0..=255u8 {
            pixels.extend_from_slice(&[
                channel.min(alpha),
                channel.wrapping_add(83).min(alpha),
                channel.wrapping_mul(17).min(alpha),
                alpha,
            ]);
        }
    }
    let fixture = DecorationBuffer {
        width: 256,
        height: 256,
        pixels,
    };
    let expected = decoded_png(&previous_png(fixture.clone()));
    let mut encoded = Vec::new();
    write_screenshot_png(fixture, &mut encoded).unwrap();
    assert_eq!(decoded_png(&encoded), expected);
}

#[test]
fn streaming_png_propagates_partial_write_iend_and_flush_failures() {
    struct FailingWriter {
        bytes: Vec<u8>,
        remaining: usize,
        fail_flush: bool,
    }
    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("injected write failure"));
            }
            let written = bytes.len().min(self.remaining);
            self.bytes.extend_from_slice(&bytes[..written]);
            self.remaining -= written;
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.fail_flush && self.bytes.ends_with(b"\0\0\0\0IEND\xaeB\x60\x82") {
                Err(std::io::Error::other("injected flush failure"))
            } else {
                Ok(())
            }
        }
    }
    let fixture = fixture(256, 256, true);
    let mut complete = Vec::new();
    write_screenshot_png(fixture.clone(), &mut complete).unwrap();
    assert_eq!(&complete[complete.len() - 8..complete.len() - 4], b"IEND");
    for limit in [0, 17, 80_000, complete.len() - 12, complete.len() - 1] {
        let mut failed = FailingWriter {
            bytes: Vec::new(),
            remaining: limit,
            fail_flush: false,
        };
        assert!(
            write_screenshot_png(fixture.clone(), &mut failed).is_err(),
            "write failure after {limit} bytes"
        );
    }
    let mut failed = FailingWriter {
        bytes: Vec::new(),
        remaining: usize::MAX,
        fail_flush: true,
    };
    assert!(
        write_screenshot_png(fixture, &mut failed).is_err(),
        "a final flush failure is not a publishable PNG"
    );
    assert!(
        failed.bytes.ends_with(&complete[complete.len() - 12..]),
        "failure occurred after the complete IEND"
    );
}

#[test]
fn a_single_write_failure_cannot_be_forgotten_during_png_finalization() {
    struct OneFailure {
        writes: usize,
        fail_at: usize,
        failed: bool,
    }
    impl Write for OneFailure {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            if self.writes == self.fail_at {
                self.failed = true;
                return Err(std::io::Error::other(
                    "one recoverable underlying write failure",
                ));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fixture = fixture(256, 256, true);
    let mut failures = 0;
    for fail_at in 1..32 {
        let mut destination = OneFailure {
            writes: 0,
            fail_at,
            failed: false,
        };
        let result = write_screenshot_png(fixture.clone(), &mut destination);
        if !destination.failed {
            result.unwrap();
            break;
        }
        failures += 1;
        assert!(
            result.is_err(),
            "write {fail_at} failed but finalization forgot it"
        );
    }
    assert!(
        failures >= 3,
        "fixture must exercise several underlying buffered writes"
    );
}

#[test]
fn streaming_png_has_no_full_frame_scratch_allocation() {
    let fixture = fixture(320, 2048, false);
    let frame_bytes = fixture.pixels.len();
    let (result, allocations) =
        chonk_test_support::measure(|| write_screenshot_png(fixture, &mut std::io::sink()));
    result.unwrap();
    assert!(allocations.calls > 0, "the test allocator must be active");
    assert!(
        allocations.requested_bytes < frame_bytes,
        "encoder requested {} bytes for a {frame_bytes}-byte image; scratch must stay below one full frame",
        allocations.requested_bytes
    );
}

#[test]
fn streaming_png_rejects_invalid_pixel_dimensions() {
    for fixture in [
        DecorationBuffer {
            width: 0,
            height: 1,
            pixels: Vec::new(),
        },
        DecorationBuffer {
            width: 1,
            height: 1,
            pixels: vec![0; 3],
        },
    ] {
        let mut output = Vec::new();
        assert!(write_screenshot_png(fixture, &mut output).is_err());
        assert!(output.is_empty());
    }
}

#[derive(Default)]
struct CountBytes(usize);

impl Write for CountBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += std::hint::black_box(bytes).len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode_counted(input: DecorationBuffer, streaming: bool) -> usize {
    let mut destination = CountBytes::default();
    if streaming {
        write_screenshot_png(std::hint::black_box(input), &mut destination).unwrap();
    } else {
        destination
            .write_all(&previous_png(std::hint::black_box(input)))
            .unwrap();
    }
    destination.0
}

#[test]
#[ignore = "manual PNG timing/allocation comparison; run release with --ignored --nocapture"]
fn benchmark_png_streaming() {
    for (width, height) in [(1920, 1080), (3840, 2160)] {
        for noise in [false, true] {
            let fixture = fixture(width, height, noise);
            let old = previous_png(fixture.clone());
            let mut new = Vec::new();
            write_screenshot_png(fixture.clone(), &mut new).unwrap();
            assert_eq!(
                decoded_png(&old),
                decoded_png(&new),
                "{width}x{height}, noise={noise}"
            );
            let sizes = (old.len(), new.len());
            drop((old, new));
            for streaming in [false, true] {
                // Separate from timing: requested allocator bytes are not
                // live memory or RSS and instrumentation is disabled below.
                let input = fixture.clone();
                let (bytes, allocations) =
                    chonk_test_support::measure(|| encode_counted(input, streaming));
                assert_eq!(bytes, if streaming { sizes.1 } else { sizes.0 });
                eprintln!(
                    "PNG {width}x{height} noise={noise} streaming={streaming}: {} allocations, {} requested bytes, {bytes} encoded bytes",
                    allocations.calls, allocations.requested_bytes
                );
            }
            let mut old_times = Vec::new();
            let mut new_times = Vec::new();
            for round in 0..4 {
                for streaming in [round % 2 == 0, round % 2 != 0] {
                    // Fixture construction/cloning, validation and allocation
                    // accounting are outside the measured interval.
                    let input = fixture.clone();
                    let start = Instant::now();
                    let bytes = encode_counted(input, streaming);
                    let elapsed = start.elapsed();
                    assert_eq!(bytes, if streaming { sizes.1 } else { sizes.0 });
                    if round > 0 {
                        if streaming {
                            new_times.push(elapsed);
                        } else {
                            old_times.push(elapsed);
                        }
                    }
                }
            }
            old_times.sort_unstable();
            new_times.sort_unstable();
            eprintln!(
                "PNG {width}x{height} noise={noise}: old median {:?}, stream median {:?}; pure encoding, excludes filesystem/fsync, fixture and active allocation counting",
                old_times[old_times.len()/2], new_times[new_times.len()/2]
            );
        }
    }
}
