//! Compile the exact private dependency helper, not a model or copied version.
//! Smithay stays excluded from the workspace (its complete upstream dev feature
//! graph is not a compositor prerequisite). These tests need no display or X11.

use chonk_test_support::{measure, AllocationCounter, AllocationStats};

#[global_allocator]
static ALLOCATOR: AllocationCounter = AllocationCounter;
#[path = "../../../vendor/smithay-0.7.0/src/xwayland/xwm/incoming_buffer.rs"]
mod incoming_buffer;

use incoming_buffer::IncomingBuffer;
use std::io::{self, ErrorKind};

#[test]
fn partial_writes_preserve_the_exact_binary_stream() {
    let original: Vec<u8> = (0..65536).map(|index| (index % 251) as u8).collect();
    let mut buffer = IncomingBuffer::default();
    buffer.extend(original.clone());
    let mut actual = Vec::new();
    for size in [1, 17, 4096, 0, 8192, 7, 65536] {
        if size == 0 {
            continue;
        }
        let before = buffer.len();
        let drained = buffer
            .write_with(|bytes| {
                let count = size.min(bytes.len());
                actual.extend_from_slice(&bytes[..count]);
                Ok(count)
            })
            .unwrap();
        assert_eq!(buffer.len(), before.saturating_sub(size));
        assert_eq!(drained, buffer.is_empty());
    }
    assert!(buffer.is_empty());
    assert_eq!(actual, original);
}

#[test]
fn transient_write_errors_preserve_bytes_and_yield_without_spinning() {
    for kind in [ErrorKind::WouldBlock, ErrorKind::Interrupted] {
        let mut buffer = IncomingBuffer::default();
        buffer.extend(b"binary\0payload".to_vec());
        let mut calls = 0;
        let result = buffer.write_with(|_| {
            calls += 1;
            Err(kind.into())
        });
        assert_eq!(calls, 1, "one callback cannot spin on readiness");
        assert!(!result.unwrap(), "transient errors wait for readiness");
        assert_eq!(buffer.len(), 14);
        assert!(buffer
            .write_with(|bytes| {
                assert_eq!(bytes, b"binary\0payload");
                Ok(bytes.len())
            })
            .unwrap());
    }
}

#[test]
fn zero_writes_fail_instead_of_busy_looping() {
    let mut buffer = IncomingBuffer::default();
    buffer.extend(vec![42]);
    assert_eq!(
        buffer.write_with(|_| Ok(0)).unwrap_err().kind(),
        ErrorKind::WriteZero
    );
    assert_eq!(buffer.len(), 1);
}

#[test]
fn fatal_errors_are_not_retried_and_empty_buffers_do_not_write() {
    let mut buffer = IncomingBuffer::default();
    assert!(buffer
        .write_with(|_| panic!("empty buffers never write"))
        .unwrap());
    buffer.extend(vec![3, 2, 1]);
    assert_eq!(
        buffer
            .write_with(|_| Err(io::Error::from(ErrorKind::BrokenPipe)))
            .unwrap_err()
            .kind(),
        ErrorKind::BrokenPipe
    );
    assert_eq!(buffer.len(), 3);
}

#[test]
fn appending_while_a_chunk_is_partly_written_keeps_order() {
    let mut buffer = IncomingBuffer::default();
    buffer.extend(vec![1, 2, 3]);
    assert!(!buffer
        .write_with(|bytes| {
            assert_eq!(bytes, [1, 2, 3]);
            Ok(1)
        })
        .unwrap());
    buffer.extend(vec![4, 5]);
    assert!(buffer
        .write_with(|bytes| {
            assert_eq!(bytes, [2, 3, 4, 5]);
            Ok(4)
        })
        .unwrap());
}

#[test]
fn moving_and_draining_owned_chunks_needs_no_additional_allocations() {
    // x11rb has already allocated each GetProperty reply. Measure only the
    // bridge's additional buffer work: 8 MiB through 1 KiB partial writes.
    let chunks: Vec<Vec<u8>> = (0..128).map(|_| vec![0xa5; 65536]).collect();
    let mut buffer = IncomingBuffer::default();
    let (written, stats) = measure(|| {
        let mut total = 0;
        for chunk in chunks {
            buffer.extend(chunk);
            while !buffer
                .write_with(|pending| {
                    let count = pending.len().min(1024);
                    total += count;
                    Ok(count)
                })
                .unwrap()
            {}
        }
        total
    });
    eprintln!("incoming buffer: payload={written}, additional allocation requests={stats:?}");
    assert_eq!(written, 8 * 1024 * 1024);
    assert_eq!(stats, AllocationStats::default());
}
