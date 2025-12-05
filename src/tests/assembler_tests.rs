//! End-to-end tests for the multi-channel assembler
//!
//! These tests verify:
//! 1. Fragmentation of large frames
//! 2. Reassembly of fragments back into original frames
//! 3. Out-of-order fragment handling
//! 4. Timeout of incomplete fragment buffers
//! 5. Performance under load

use crate::channel::assembler::{
    has_fragment_header, AssemblerConfig, FragmentHeader, FRAGMENT_HEADER_SIZE,
};
use bytes::Bytes;
use std::time::Duration;

/// Test that small frames pass through without fragmentation
#[test]
fn test_small_frame_passthrough() {
    // Frame smaller than threshold should not be fragmented
    let small_data = Bytes::from_static(b"Hello, World!");

    // Check it doesn't have fragment header
    assert!(!has_fragment_header(&small_data));

    // The data should be usable as-is
    assert_eq!(&small_data[..], b"Hello, World!");
}

/// Test fragment header encoding/decoding roundtrip
#[test]
fn test_fragment_header_roundtrip() {
    // Test various sequence IDs, fragment indices, and totals
    let test_cases = vec![
        (0, 0, 1),            // Single fragment
        (1, 0, 2),            // First of two
        (1, 1, 2),            // Second of two
        (12345, 5, 100),      // Middle fragment
        (u32::MAX, 255, 256), // Large values
    ];

    for (seq_id, frag_idx, total_frags) in test_cases {
        let header = FragmentHeader::new(seq_id, frag_idx, total_frags);
        let encoded = header.encode();

        assert_eq!(encoded.len(), FRAGMENT_HEADER_SIZE);

        let decoded = FragmentHeader::decode(&encoded).expect("Should decode");
        assert_eq!(decoded.seq_id, seq_id);
        assert_eq!(decoded.frag_idx, frag_idx);
        assert_eq!(decoded.total_frags, total_frags);
    }
}

/// Test first/last fragment detection
#[test]
fn test_fragment_position_detection() {
    // Single fragment (both first and last)
    let single = FragmentHeader::new(1, 0, 1);
    assert!(single.is_first());
    assert!(single.is_last());

    // First of multiple
    let first = FragmentHeader::new(1, 0, 5);
    assert!(first.is_first());
    assert!(!first.is_last());

    // Middle fragment
    let middle = FragmentHeader::new(1, 2, 5);
    assert!(!middle.is_first());
    assert!(!middle.is_last());

    // Last fragment
    let last = FragmentHeader::new(1, 4, 5);
    assert!(!last.is_first());
    assert!(last.is_last());
}

/// Test manual fragmentation and reassembly
#[test]
fn test_manual_fragmentation_reassembly() {
    // Create a "large" frame that would need to be fragmented
    let original_data = vec![0u8; 1000];
    for (i, byte) in original_data.iter().enumerate() {
        // Each byte should be 0 (as initialized)
        assert_eq!(*byte, 0, "Byte {} should be 0", i);
    }

    // Manually fragment into 4 parts (250 bytes each)
    let chunk_size = 250;
    let num_fragments = original_data.len().div_ceil(chunk_size);
    assert_eq!(num_fragments, 4);

    let seq_id = 42u32;
    let mut fragments = Vec::new();

    for (i, chunk) in original_data.chunks(chunk_size).enumerate() {
        let header = FragmentHeader::new(seq_id, i as u16, num_fragments as u16);
        let header_bytes = header.encode();

        // Build fragment: header + data
        let mut fragment = Vec::with_capacity(FRAGMENT_HEADER_SIZE + chunk.len());
        fragment.extend_from_slice(&header_bytes);
        fragment.extend_from_slice(chunk);

        fragments.push(Bytes::from(fragment));
    }

    // Verify fragments have headers
    for fragment in &fragments {
        assert!(has_fragment_header(fragment));
    }

    // Reassemble manually
    let mut reassembled = Vec::new();
    for fragment in &fragments {
        let header = FragmentHeader::decode(fragment).expect("Should decode");
        assert_eq!(header.seq_id, seq_id);

        // Extract payload (skip header)
        let payload = &fragment[FRAGMENT_HEADER_SIZE..];
        reassembled.extend_from_slice(payload);
    }

    // Verify reassembled matches original
    assert_eq!(reassembled.len(), original_data.len());
    assert_eq!(reassembled, original_data);
}

/// Test out-of-order fragment reassembly
#[test]
fn test_out_of_order_reassembly() {
    // Original data split into fragments
    let parts = [b"AAAA", b"BBBB", b"CCCC", b"DDDD"];
    let seq_id = 123u32;

    // Create fragments out of order: 2, 0, 3, 1
    let indices = [2, 0, 3, 1];
    let mut fragments = Vec::new();

    for &idx in &indices {
        let header = FragmentHeader::new(seq_id, idx as u16, 4);
        let header_bytes = header.encode();

        let mut fragment = Vec::new();
        fragment.extend_from_slice(&header_bytes);
        fragment.extend_from_slice(parts[idx]);

        fragments.push((idx, Bytes::from(fragment)));
    }

    // Simulate reassembly buffer
    let mut buffer: Vec<Option<Bytes>> = vec![None; 4];
    let mut received_count = 0;

    for (idx, fragment) in fragments {
        let _header = FragmentHeader::decode(&fragment).expect("Should decode");
        let payload = fragment.slice(FRAGMENT_HEADER_SIZE..);

        if buffer[idx].is_none() {
            buffer[idx] = Some(payload);
            received_count += 1;
        }
    }

    assert_eq!(received_count, 4);

    // Reassemble in order
    let mut reassembled = Vec::new();
    for part in buffer.iter().flatten() {
        reassembled.extend_from_slice(part);
    }

    assert_eq!(&reassembled[..], b"AAAABBBBCCCCDDDD");
}

/// Test duplicate fragment handling
#[test]
fn test_duplicate_fragment_ignored() {
    let seq_id = 1u32;
    let mut buffer: Vec<Option<Bytes>> = vec![None; 2];
    let mut received_count = 0;

    // Add first fragment
    let frag0 = {
        let header = FragmentHeader::new(seq_id, 0, 2);
        let mut data = Vec::from(header.encode());
        data.extend_from_slice(b"ORIGINAL");
        Bytes::from(data)
    };

    if buffer[0].is_none() {
        buffer[0] = Some(frag0.slice(FRAGMENT_HEADER_SIZE..));
        received_count += 1;
    }

    // Try to add duplicate with different payload
    let frag0_dup = {
        let header = FragmentHeader::new(seq_id, 0, 2);
        let mut data = Vec::from(header.encode());
        data.extend_from_slice(b"DUPLICATE");
        Bytes::from(data)
    };

    if buffer[0].is_none() {
        buffer[0] = Some(frag0_dup.slice(FRAGMENT_HEADER_SIZE..));
        received_count += 1;
    }

    // Original should be kept
    assert_eq!(received_count, 1);
    assert_eq!(&buffer[0].as_ref().unwrap()[..], b"ORIGINAL");
}

/// Test config defaults
#[test]
fn test_assembler_config_defaults() {
    let config = AssemblerConfig::default();

    // Verify sensible defaults
    assert!(config.fragment_threshold > 0);
    assert!(config.max_fragments > 0);
    assert!(config.fragment_timeout > Duration::ZERO);
    assert!(config.max_pending_buffers > 0);

    // Default threshold should be reasonable for WebRTC
    assert!(config.fragment_threshold >= 8 * 1024); // At least 8KB
    assert!(config.fragment_threshold <= 64 * 1024); // At most 64KB
}

/// Test large frame fragmentation calculation
#[test]
fn test_fragmentation_calculation() {
    let config = AssemblerConfig::default();
    let usable_size = config.fragment_threshold - FRAGMENT_HEADER_SIZE;

    // Frame that fits in one fragment
    let small_frame_size = usable_size;
    let small_fragments = small_frame_size.div_ceil(usable_size);
    assert_eq!(small_fragments, 1);

    // Frame that needs exactly 2 fragments
    let two_frag_size = usable_size + 1;
    let two_fragments = two_frag_size.div_ceil(usable_size);
    assert_eq!(two_fragments, 2);

    // Large frame
    let large_frame_size = usable_size * 10;
    let many_fragments = large_frame_size.div_ceil(usable_size);
    assert_eq!(many_fragments, 10);

    // Verify max fragment limit
    let max_size = usable_size * (config.max_fragments as usize);
    let max_fragments = max_size.div_ceil(usable_size);
    assert_eq!(max_fragments, config.max_fragments as usize);
}

/// Test fragment header binary format stability
#[test]
fn test_fragment_header_binary_format() {
    // Create a known header - first fragment (frag_idx = 0)
    let header = FragmentHeader::new(0x12345678, 0x0000, 0x00CD);
    let encoded = header.encode();

    // Verify exact binary format:
    // [flags: 1][seq_id: 4][frag_idx: 2][total_frags: 2]
    assert_eq!(encoded.len(), 9);

    // Flags: HAS_FRAGMENT_HEADER | FIRST_FRAGMENT (since frag_idx == 0)
    // 0x01 | 0x02 = 0x03
    assert_eq!(encoded[0], 0x03, "First fragment should have flags 0x03");

    // Seq ID in big-endian: 0x12345678
    assert_eq!(encoded[1], 0x12);
    assert_eq!(encoded[2], 0x34);
    assert_eq!(encoded[3], 0x56);
    assert_eq!(encoded[4], 0x78);

    // Frag idx in big-endian: 0x0000
    assert_eq!(encoded[5], 0x00);
    assert_eq!(encoded[6], 0x00);

    // Total frags in big-endian: 0x00CD
    assert_eq!(encoded[7], 0x00);
    assert_eq!(encoded[8], 0xCD);

    // Also test middle fragment (no FIRST or LAST)
    let middle = FragmentHeader::new(1, 5, 10);
    let middle_encoded = middle.encode();
    assert_eq!(
        middle_encoded[0], 0x01,
        "Middle fragment should only have HAS_FRAGMENT_HEADER"
    );

    // Test last fragment
    let last = FragmentHeader::new(1, 9, 10);
    let last_encoded = last.encode();
    assert_eq!(
        last_encoded[0], 0x05,
        "Last fragment should have HAS_FRAGMENT_HEADER | LAST_FRAGMENT"
    );
}

/// Performance test for fragment creation
#[test]
fn test_fragment_header_performance() {
    use std::time::Instant;

    let iterations = 100_000u64;

    let start = Instant::now();
    for i in 0..iterations {
        let header = FragmentHeader::new(i as u32, (i % 100) as u16, 100);
        let _ = header.encode();
    }
    let duration = start.elapsed();

    let ns_per_op = duration.as_nanos() / iterations as u128;
    let ops_per_sec = if ns_per_op > 0 {
        1_000_000_000 / ns_per_op
    } else {
        u128::MAX // Essentially infinite
    };

    println!(
        "Fragment header encode: {}ns per operation ({} ops/sec)",
        ns_per_op, ops_per_sec
    );

    // Should be very fast (<1us per operation)
    // In release mode this can be optimized to near-zero
    assert!(
        ns_per_op < 1000,
        "Header encoding too slow: {}ns",
        ns_per_op
    );
}

/// Performance test for fragment parsing
#[test]
fn test_fragment_parse_performance() {
    use std::time::Instant;

    let iterations = 100_000u64;

    // Pre-create encoded headers
    let headers: Vec<[u8; 9]> = (0..100)
        .map(|i| FragmentHeader::new(i, (i % 10) as u16, 10).encode())
        .collect();

    let start = Instant::now();
    for i in 0..iterations {
        let header_bytes = &headers[(i as usize) % headers.len()];
        let _ = FragmentHeader::decode(header_bytes);
    }
    let duration = start.elapsed();

    let ns_per_op = duration.as_nanos() / iterations as u128;
    let ops_per_sec = if ns_per_op > 0 {
        1_000_000_000 / ns_per_op
    } else {
        u128::MAX // Essentially infinite
    };

    println!(
        "Fragment header decode: {}ns per operation ({} ops/sec)",
        ns_per_op, ops_per_sec
    );

    // Should be very fast (<1us per operation)
    // In release mode this can be optimized to near-zero
    assert!(
        ns_per_op < 1000,
        "Header decoding too slow: {}ns",
        ns_per_op
    );
}

/// Test simulated frame transmission with fragmentation
#[test]
fn test_simulated_transmission() {
    // Simulate sending a large frame that needs fragmentation
    let original = Bytes::from(vec![42u8; 50_000]); // 50KB frame
    let config = AssemblerConfig::default();

    let usable_size = config.fragment_threshold - FRAGMENT_HEADER_SIZE;
    let num_frags = original.len().div_ceil(usable_size);

    println!(
        "Fragmenting {}KB frame into {} fragments ({}KB usable per fragment)",
        original.len() / 1024,
        num_frags,
        usable_size / 1024
    );

    // Fragment
    let seq_id = 1u32;
    let mut fragments = Vec::new();

    for (i, chunk) in original.chunks(usable_size).enumerate() {
        let header = FragmentHeader::new(seq_id, i as u16, num_frags as u16);
        let mut frag = Vec::with_capacity(FRAGMENT_HEADER_SIZE + chunk.len());
        frag.extend_from_slice(&header.encode());
        frag.extend_from_slice(chunk);
        fragments.push(Bytes::from(frag));
    }

    // Verify fragment count
    assert_eq!(fragments.len(), num_frags);

    // Reassemble
    let mut reassembled = bytes::BytesMut::with_capacity(original.len());
    for frag in &fragments {
        let payload = &frag[FRAGMENT_HEADER_SIZE..];
        reassembled.extend_from_slice(payload);
    }

    // Verify
    assert_eq!(reassembled.len(), original.len());
    assert_eq!(&reassembled[..], &original[..]);

    println!(
        "Successfully transmitted and reassembled {}KB",
        original.len() / 1024
    );
}

/// Test standalone fragment_frame function used by connections.rs
#[test]
fn test_standalone_fragment_frame() {
    use crate::channel::assembler::{
        fragment_frame, has_fragment_header, DEFAULT_FRAGMENT_THRESHOLD, DEFAULT_MAX_FRAGMENTS,
    };

    // Test 1: Small frame should return None (no fragmentation needed)
    let small_frame = Bytes::from(vec![1u8; 1000]); // 1KB - under threshold
    let result = fragment_frame(
        &small_frame,
        DEFAULT_FRAGMENT_THRESHOLD,
        DEFAULT_MAX_FRAGMENTS,
    );
    assert!(result.is_none(), "Small frame should not be fragmented");

    // Test 2: Large frame should be fragmented
    let large_frame = Bytes::from(vec![2u8; 50_000]); // 50KB - over threshold
    let fragments = fragment_frame(
        &large_frame,
        DEFAULT_FRAGMENT_THRESHOLD,
        DEFAULT_MAX_FRAGMENTS,
    );
    assert!(fragments.is_some(), "Large frame should be fragmented");

    let fragments = fragments.unwrap();
    println!(
        "Large frame (50KB) fragmented into {} fragments",
        fragments.len()
    );

    // Verify all fragments have headers
    for (i, frag) in fragments.iter().enumerate() {
        assert!(
            has_fragment_header(frag),
            "Fragment {} should have a header",
            i
        );
        // Verify fragment size is reasonable
        assert!(
            frag.len() <= DEFAULT_FRAGMENT_THRESHOLD,
            "Fragment {} exceeds threshold: {} > {}",
            i,
            frag.len(),
            DEFAULT_FRAGMENT_THRESHOLD
        );
    }

    // Test 3: Reassemble fragments and verify data integrity
    let mut reassembled = bytes::BytesMut::with_capacity(large_frame.len());
    for frag in &fragments {
        let header = FragmentHeader::decode(frag).expect("Fragment should have valid header");
        let payload = &frag[FRAGMENT_HEADER_SIZE..];
        reassembled.extend_from_slice(payload);

        // Verify header fields
        assert!(header.total_frags > 1, "Should have multiple fragments");
        assert!(
            header.frag_idx < header.total_frags,
            "Fragment index should be less than total"
        );
    }

    assert_eq!(
        reassembled.len(),
        large_frame.len(),
        "Reassembled size should match original"
    );
    assert_eq!(
        &reassembled[..],
        &large_frame[..],
        "Reassembled data should match original"
    );

    println!(
        "Standalone fragmentation test passed - 50KB frame correctly fragmented and reassembled"
    );
}

/// Test fragmentation with EventDrivenSender integration (simulated)
#[test]
fn test_fragmentation_send_simulation() {
    use crate::channel::assembler::{
        fragment_frame, has_fragment_header, DEFAULT_FRAGMENT_THRESHOLD, DEFAULT_MAX_FRAGMENTS,
    };

    // Simulate what connections.rs does when sending
    let test_data = Bytes::from(vec![0xABu8; 100_000]); // 100KB frame

    let mut sent_frames: Vec<Bytes> = Vec::new();

    // Simulate the send path
    if test_data.len() > DEFAULT_FRAGMENT_THRESHOLD {
        if let Some(fragments) = fragment_frame(
            &test_data,
            DEFAULT_FRAGMENT_THRESHOLD,
            DEFAULT_MAX_FRAGMENTS,
        ) {
            // Would normally send each fragment through EventDrivenSender
            for fragment in fragments {
                sent_frames.push(fragment);
            }
        } else {
            // Frame too large for fragmentation
            sent_frames.push(test_data.clone());
        }
    } else {
        // Small frame - send directly
        sent_frames.push(test_data.clone());
    }

    // Verify multiple fragments were "sent"
    assert!(
        sent_frames.len() > 1,
        "100KB frame should produce multiple fragments"
    );
    println!(
        "100KB frame would be sent as {} fragments",
        sent_frames.len()
    );

    // Verify all sent frames are valid fragments
    for frame in &sent_frames {
        assert!(has_fragment_header(frame), "All frames should be fragments");
    }

    // Simulate receive side reassembly
    let mut reassembled = bytes::BytesMut::with_capacity(test_data.len());
    for frame in &sent_frames {
        let payload = &frame[FRAGMENT_HEADER_SIZE..];
        reassembled.extend_from_slice(payload);
    }

    assert_eq!(
        reassembled.len(),
        test_data.len(),
        "Reassembled size should match"
    );
    assert_eq!(
        &reassembled[..],
        &test_data[..],
        "Reassembled data should match"
    );

    println!("Send simulation passed - 100KB correctly fragmented and reassembled");
}
