// Integration tests for size instruction detection and processing
// Tests the complete pipeline from GuacdParser detection to background processing

use crate::buffer_pool::{BufferPool, BufferPoolConfig};
use crate::channel::guacd_parser::{GuacdParser, OpcodeAction, SpecialOpcode};
use bytes::BytesMut;
use log::{debug, info, warn};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

// Mock message structure for testing the size instruction processing
#[derive(Clone)]
struct MockSizeInstructionMessage {
    buffer: BytesMut,
    buffer_pool: BufferPool,
}

// Mock background processor function for testing
async fn mock_process_size_instruction(
    channel_id: &str,
    instruction_buffer: &BytesMut,
) -> anyhow::Result<(String, i32, String)> {
    debug!(
        "Processing size instruction in test (channel_id: {}, buffer_len: {})",
        channel_id,
        instruction_buffer.len()
    );

    // Parse the instruction from the buffer to extract layer and raw instruction
    if let Ok(peeked) = GuacdParser::peek_instruction(instruction_buffer) {
        if peeked.opcode == "size" && peeked.args.len() >= 2 {
            let layer = peeked.args[0].parse::<i32>().unwrap_or(-1);
            let width = peeked.args.get(1).unwrap_or(&"0");
            let height = peeked.args.get(2).unwrap_or(&"0");
            let raw_instruction = std::str::from_utf8(instruction_buffer)
                .unwrap_or("")
                .to_string();

            info!(   "Successfully processed size instruction (channel_id: {}, layer: {}, width: {}, height: {})",
                  channel_id, layer, width, height);

            return Ok((channel_id.to_string(), layer, raw_instruction));
        }
    }

    warn!(
        "Failed to parse as valid size instruction (channel_id: {})",
        channel_id
    );
    Err(anyhow::anyhow!("Not a valid size instruction"))
}

#[tokio::test]
async fn test_expandable_size_instruction_detection() {
    // Test the new expandable validation function detects size instructions correctly

    let size_instructions = vec![
        b"4.size,1.0,4.1920,4.1080;" as &[u8], // 1920x1080 on layer 0
        b"4.size,1.1,3.800,3.600;" as &[u8],   // 800x600 on layer 1
        b"4.size,2.10,4.1024,3.768;" as &[u8], // 1024x768 on layer 10
    ];

    let non_size_instructions = vec![
        b"4.sync,4.1000;" as &[u8],
        b"5.error,11.Auth failed;" as &[u8],
        b"4.copy,2.-1,1.0,1.0,3.100,3.100,1.0,1.0,2.50,2.50;" as &[u8],
    ];

    info!("Testing expandable size instruction detection");

    // Test size instruction detection using new expandable system
    for (i, instruction) in size_instructions.iter().enumerate() {
        debug!(
            "Testing size instruction detection (instruction_num: {}, instruction: {})",
            i,
            std::str::from_utf8(instruction).unwrap_or("invalid_utf8")
        );

        match GuacdParser::validate_and_detect_special(instruction) {
            Ok((instruction_len, action)) => {
                assert_eq!(instruction_len, instruction.len());
                assert_eq!(
                    action,
                    OpcodeAction::ProcessSpecial(SpecialOpcode::Size),
                    "Size instruction {} should be detected as special size action",
                    i
                );

                info!(
                    "Size instruction correctly detected (instruction_num: {}, action: {:?})",
                    i, action
                );
            }
            Err(e) => panic!("Size instruction {} detection failed: {:?}", i, e),
        }
    }

    // Test non-size instruction detection
    for (i, instruction) in non_size_instructions.iter().enumerate() {
        debug!(
            "Testing non-size instruction (instruction_num: {}, instruction: {})",
            i,
            std::str::from_utf8(instruction).unwrap_or("invalid_utf8")
        );

        match GuacdParser::validate_and_detect_special(instruction) {
            Ok((instruction_len, action)) => {
                assert_eq!(instruction_len, instruction.len());
                assert_ne!(
                    action,
                    OpcodeAction::ProcessSpecial(SpecialOpcode::Size),
                    "Non-size instruction {} should not be detected as size",
                    i
                );

                info!(
                    "Non-size instruction correctly classified (instruction_num: {}, action: {:?})",
                    i, action
                );
            }
            Err(e) => panic!("Non-size instruction {} validation failed: {:?}", i, e),
        }
    }
}

#[tokio::test]
async fn test_size_instruction_background_processing_pipeline() {
    // Test the complete pipeline from detection to background processing

    info!("Starting background processing pipeline test");

    let config = BufferPoolConfig::default();
    let buffer_pool = BufferPool::new(config);
    let channel_id = "test_channel_123";

    // Create a bounded channel for size instruction processing
    let (size_tx, mut size_rx) = mpsc::channel::<MockSizeInstructionMessage>(10);

    debug!( "Created bounded channel for size instruction processing (channel_id: {}, channel_capacity: {})",
           channel_id, 10);

    // Spawn background processor task
    let channel_id_clone = channel_id.to_string();
    let processor_handle = tokio::spawn(async move {
        let mut processed_instructions = Vec::new();

        info!(
            "Background processor started (channel_id: {})",
            channel_id_clone
        );

        while let Some(size_msg) = size_rx.recv().await {
            let MockSizeInstructionMessage {
                buffer,
                buffer_pool,
            } = size_msg;

            debug!(    "Received size instruction in background processor (channel_id: {}, buffer_len: {})",
                   channel_id_clone, buffer.len());

            // Process the size instruction
            match mock_process_size_instruction(&channel_id_clone, &buffer).await {
                Ok((channel_id, layer, raw_instruction)) => {
                    processed_instructions.push((channel_id, layer, raw_instruction));
                }
                Err(e) => {
                    warn!(
                        "Failed to process size instruction (channel_id: {}, error: {})",
                        channel_id_clone, e
                    );
                }
            }

            // Return buffer to pool (simulating the real implementation)
            buffer_pool.release(buffer);
        }

        info!(
            "Background processor completed (channel_id: {}, processed_count: {})",
            channel_id_clone,
            processed_instructions.len()
        );

        processed_instructions
    });

    // Simulate the hot path detection and sending
    let test_size_instructions = vec![
        (b"4.size,1.0,4.1920,4.1080;" as &[u8], 0i32), // layer 0
        (b"4.size,1.5,4.1440,3.900;" as &[u8], 5i32),  // layer 5
        (b"4.size,2.10,4.1024,3.768;" as &[u8], 10i32), // layer 10
    ];

    for (instruction_bytes, expected_layer) in test_size_instructions.iter() {
        debug!(
            "Processing test size instruction in hot path (instruction: {}, expected_layer: {})",
            std::str::from_utf8(instruction_bytes).unwrap_or("invalid_utf8"),
            expected_layer
        );

        // Simulate hot path: detect size instruction using new expandable system
        match GuacdParser::validate_and_detect_special(instruction_bytes) {
            Ok((instruction_len, action)) => {
                assert_eq!(
                    action,
                    OpcodeAction::ProcessSpecial(SpecialOpcode::Size),
                    "Should detect as size instruction action"
                );

                info!(
                    "Hot path detected size instruction (instruction_len: {}, action: {:?})",
                    instruction_len, action
                );

                // Simulate buffer pool acquisition and copying (hot path)
                let mut size_buffer = buffer_pool.acquire();
                size_buffer.clear();
                size_buffer.extend_from_slice(&instruction_bytes[..instruction_len]);

                debug!(
                    "Acquired buffer and copied instruction data (buffer_len: {})",
                    size_buffer.len()
                );

                // Send to background processor
                let size_msg = MockSizeInstructionMessage {
                    buffer: size_buffer,
                    buffer_pool: buffer_pool.clone(),
                };

                size_tx
                    .send(size_msg)
                    .await
                    .expect("Failed to send size instruction to background processor");

                debug!("Sent instruction to background processor");
            }
            Err(e) => panic!("Size instruction detection failed: {:?}", e),
        }
    }

    // Close the sender to signal completion
    drop(size_tx);
    info!("Closed sender, waiting for background processing to complete");

    // Wait for background processing to complete
    let processed_instructions = timeout(Duration::from_secs(5), processor_handle)
        .await
        .expect("Background processor timed out")
        .expect("Background processor task failed");

    // Verify all instructions were processed correctly
    assert_eq!(
        processed_instructions.len(),
        3,
        "Should have processed 3 size instructions"
    );

    for (i, (processed_channel_id, layer, raw_instruction)) in
        processed_instructions.iter().enumerate()
    {
        assert_eq!(
            processed_channel_id, channel_id,
            "Channel ID should match for instruction {}",
            i
        );
        assert_eq!(
            *layer, test_size_instructions[i].1,
            "Layer should match for instruction {}",
            i
        );
        assert!(
            raw_instruction.contains("size"),
            "Raw instruction should contain 'size' for instruction {}",
            i
        );
        assert!(
            raw_instruction.ends_with(";"),
            "Raw instruction should end with ';' for instruction {}",
            i
        );

        info!(
            "Verified processed instruction (instruction_num: {}, channel_id: {}, layer: {})",
            i, processed_channel_id, layer
        );
    }

    info!("Background processing pipeline test completed successfully");
}

#[tokio::test]
async fn test_size_instruction_channel_backpressure() {
    // Test that the size instruction channel handles backpressure correctly

    info!("Testing channel backpressure handling");

    let config = BufferPoolConfig::default();
    let buffer_pool = BufferPool::new(config);

    // Create a very small channel to test backpressure
    let (size_tx, mut size_rx) = mpsc::channel::<MockSizeInstructionMessage>(1);

    debug!(
        "Created small capacity channel for backpressure testing (channel_capacity: {})",
        1
    );

    let size_instruction = b"4.size,1.0,4.1920,4.1080;";

    // Fill the channel to capacity
    let mut first_buffer = buffer_pool.acquire();
    first_buffer.clear();
    first_buffer.extend_from_slice(size_instruction);

    let first_msg = MockSizeInstructionMessage {
        buffer: first_buffer,
        buffer_pool: buffer_pool.clone(),
    };

    // This should succeed (channel capacity = 1)
    size_tx
        .send(first_msg)
        .await
        .expect("First send should succeed");
    info!("First message sent successfully");

    // Try to send another (should succeed with try_send or timeout with send)
    let mut second_buffer = buffer_pool.acquire();
    second_buffer.clear();
    second_buffer.extend_from_slice(size_instruction);

    let second_msg = MockSizeInstructionMessage {
        buffer: second_buffer,
        buffer_pool: buffer_pool.clone(),
    };

    // This should fail with try_send (simulating the real hot path behavior)
    let try_send_result = size_tx.try_send(second_msg);
    assert!(
        try_send_result.is_err(),
        "try_send should fail when channel is full"
    );

    warn!("try_send correctly failed due to channel backpressure");

    // The buffer should be automatically released when the message is dropped
    // (This is tested implicitly - if there's a leak, other tests will show it)

    // Drain the channel to clean up
    let _first_received = size_rx.recv().await.expect("Should receive first message");
    debug!("Drained channel for cleanup");

    info!("Channel backpressure test completed successfully");
}

#[tokio::test]
async fn test_expandable_system_performance() {
    // Test that the new expandable system maintains performance characteristics
    use std::time::Instant;

    info!("Testing expandable system performance");

    let size_instruction = b"4.size,1.0,4.1920,4.1080;";
    let sync_instruction = b"4.sync,4.1000;"; // Now classified as ServerSync for keepalive auto-response
    let error_instruction = b"5.error,11.Auth failed;";

    let iterations = 10000; // Higher iteration count for performance testing

    debug!(
        "Starting performance test with expandable opcode system (iterations: {})",
        iterations
    );

    let start = Instant::now();
    for i in 0..iterations {
        // Test size instruction
        match GuacdParser::validate_and_detect_special(size_instruction) {
            Ok((_, OpcodeAction::ProcessSpecial(SpecialOpcode::Size))) => {} // Expected
            other => panic!(
                "Size instruction validation failed at iteration {}: {:?}",
                i, other
            ),
        }

        // Test sync instruction (now ServerSync for keepalive auto-response)
        match GuacdParser::validate_and_detect_special(sync_instruction) {
            Ok((_, OpcodeAction::ServerSync)) => {} // Expected
            other => panic!(
                "Sync instruction validation failed at iteration {}: {:?}",
                i, other
            ),
        }

        // Test error instruction
        match GuacdParser::validate_and_detect_special(error_instruction) {
            Ok((_, OpcodeAction::CloseConnection)) => {} // Expected
            other => panic!(
                "Error instruction validation failed at iteration {}: {:?}",
                i, other
            ),
        }
    }
    let duration = start.elapsed();

    let per_instruction = duration / (iterations * 3);

    info!("Expandable validation performance results (iterations: {}, total_duration_micros: {}, per_instruction_nanos: {})",
          iterations * 3, duration.as_micros(), per_instruction.as_nanos());

    // Performance should be sub-microsecond per instruction
    // This is a soft requirement - the test passes regardless, but prints timing info
    if per_instruction > Duration::from_micros(10) {
        warn!("WARNING: Expandable opcode detection may be slower than expected (per_instruction_micros: {})",
              per_instruction.as_micros());
    } else {
        info!("Performance within expected bounds");
    }
}

#[tokio::test]
async fn test_realistic_guacamole_scenarios_with_expandable_system() {
    // Test with realistic Guacamole protocol scenarios using the new expandable system

    info!("Testing realistic Guacamole scenarios with expandable system");

    // Realistic size instructions that would come from an actual Guacamole session
    let realistic_scenarios = vec![
        // Initial screen size setting
        (
            b"4.size,1.0,4.1920,4.1080;" as &[u8],
            "Initial 1080p screen",
        ),
        // Window resize
        (b"4.size,1.0,4.1440,3.900;" as &[u8], "Resize to 1440x900"),
        // Multiple layer support
        (
            b"4.size,1.1,3.800,3.600;" as &[u8],
            "Secondary layer 800x600",
        ),
        (
            b"4.size,2.10,4.1024,3.768;" as &[u8],
            "Layer 10 at 1024x768",
        ),
        // Very large screens
        (
            b"4.size,1.0,4.3840,4.2160;" as &[u8],
            "4K display 3840x2160",
        ),
        // Small screens (mobile/tablet)
        (
            b"4.size,1.0,3.768,4.1024;" as &[u8],
            "Tablet portrait 768x1024",
        ),
    ];

    for (instruction_bytes, description) in realistic_scenarios {
        debug!(
            "Testing realistic scenario (scenario: {}, instruction: {})",
            description,
            std::str::from_utf8(instruction_bytes).unwrap_or("invalid_utf8")
        );

        match GuacdParser::validate_and_detect_special(instruction_bytes) {
            Ok((instruction_len, action)) => {
                assert_eq!(
                    instruction_len,
                    instruction_bytes.len(),
                    "Wrong instruction length for: {}",
                    description
                );
                assert_eq!(
                    action,
                    OpcodeAction::ProcessSpecial(SpecialOpcode::Size),
                    "Should be detected as size action for: {}",
                    description
                );

                // Also verify we can parse the full instruction
                if let Ok(peeked) = GuacdParser::peek_instruction(instruction_bytes) {
                    assert_eq!(peeked.opcode, "size");
                    assert!(
                        peeked.args.len() >= 3,
                        "Size instruction should have at least 3 args (layer, width, height)"
                    );

                    // Verify layer can be parsed as integer
                    let layer: Result<i32, _> = peeked.args[0].parse();
                    assert!(
                        layer.is_ok(),
                        "Layer should be parseable as integer for: {}",
                        description
                    );

                    // Verify width and height can be parsed as integers
                    let width: Result<u32, _> = peeked.args[1].parse();
                    let height: Result<u32, _> = peeked.args[2].parse();
                    assert!(
                        width.is_ok(),
                        "Width should be parseable as integer for: {}",
                        description
                    );
                    assert!(
                        height.is_ok(),
                        "Height should be parseable as integer for: {}",
                        description
                    );

                    // Verify reasonable screen dimensions
                    let width = width.unwrap();
                    let height = height.unwrap();
                    assert!(
                        width > 0 && width <= 10000,
                        "Width should be reasonable for: {}",
                        description
                    );
                    assert!(
                        height > 0 && height <= 10000,
                        "Height should be reasonable for: {}",
                        description
                    );

                    info!(    "Realistic scenario validated successfully (scenario: {}, layer: {}, width: {}, height: {})",
                          description, layer.unwrap(), width, height);
                } else {
                    panic!("Failed to peek instruction for: {}", description);
                }
            }
            Err(e) => panic!("Validation failed for {}: {:?}", description, e),
        }
    }

    info!("All realistic Guacamole scenarios passed with expandable system");
}

#[tokio::test]
async fn test_expandable_system_extensibility_demonstration() {
    // Demonstrate how easy it is to extend the system for new opcodes

    info!("Demonstrating expandable system extensibility");

    // Current opcodes we support
    let test_cases = vec![
        (
            b"5.error,10.Test error;" as &[u8],
            OpcodeAction::CloseConnection,
            "Error opcode",
        ),
        (
            b"4.size,1.0,4.1920,4.1080;" as &[u8],
            OpcodeAction::ProcessSpecial(SpecialOpcode::Size),
            "Size opcode",
        ),
        (
            b"4.sync,4.1000;" as &[u8],
            OpcodeAction::ServerSync,
            "Sync opcode (keepalive auto-response)",
        ),
        (
            b"4.copy,2.10,1.0,1.0,3.100,3.100;" as &[u8],
            OpcodeAction::Normal,
            "Copy opcode",
        ),
    ];

    for (instruction, expected_action, description) in test_cases {
        debug!(
            "Testing opcode classification (test_case: {}, instruction: {})",
            description,
            std::str::from_utf8(instruction).unwrap_or("invalid_utf8")
        );

        match GuacdParser::validate_and_detect_special(instruction) {
            Ok((_, action)) => {
                assert_eq!(
                    action, expected_action,
                    "Action mismatch for: {}",
                    description
                );
                info!(
                    "Opcode correctly classified (test_case: {}, action: {:?})",
                    description, action
                );
            }
            Err(e) => panic!("Failed to validate {}: {:?}", description, e),
        }
    }

    info!("Extensibility demonstration completed - system correctly classifies all opcode types");

    // Future opcodes can be added by:
    // 1. Adding to SpecialOpcode enum
    // 2. Adding byte comparison in matches_bytes()
    // 3. Adding to detection logic in validate_and_detect_special()
    // 4. Adding handler dispatch in connections.rs
    // 5. Implementing the handler function
}
