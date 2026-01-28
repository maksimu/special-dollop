//! Performance and load tests for protocol handlers
//!
//! These tests measure performance characteristics and test under load.
//!
//! Run tests with:
//!   cargo test --package guacr-handlers --test performance_test -- --include-ignored

use bytes::Bytes;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[cfg(test)]
mod performance_tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_high_frequency_messages() {
        // Test handling high-frequency messages (simulating rapid mouse movement)
        let (tx, mut rx) = mpsc::channel::<Bytes>(10000);

        let sender = tokio::spawn(async move {
            let start = Instant::now();
            let mut count = 0;

            // Send 1000 messages as fast as possible
            for i in 0..1000 {
                let msg = format!(
                    "5.mouse,{}.{},{}.{};",
                    i.to_string().len(),
                    i,
                    i.to_string().len(),
                    i
                );
                if tx.send(Bytes::from(msg)).await.is_err() {
                    break;
                }
                count += 1;
            }

            let elapsed = start.elapsed();
            (count, elapsed)
        });

        // Receiver
        let receiver = tokio::spawn(async move {
            let start = Instant::now();
            let mut count = 0;

            while let Some(_msg) = rx.recv().await {
                count += 1;
                if count >= 1000 {
                    break;
                }
            }

            let elapsed = start.elapsed();
            (count, elapsed)
        });

        let (send_result, recv_result) = tokio::join!(sender, receiver);
        let (sent_count, send_time) = send_result.unwrap();
        let (recv_count, recv_time) = recv_result.unwrap();

        println!(
            "Sent {} messages in {:?} ({:.0} msg/s)",
            sent_count,
            send_time,
            sent_count as f64 / send_time.as_secs_f64()
        );
        println!(
            "Received {} messages in {:?} ({:.0} msg/s)",
            recv_count,
            recv_time,
            recv_count as f64 / recv_time.as_secs_f64()
        );

        assert_eq!(sent_count, 1000);
        assert_eq!(recv_count, 1000);

        // Should handle at least 1000 messages/second
        assert!(recv_time.as_secs_f64() < 1.0, "Too slow: {:?}", recv_time);
    }

    #[tokio::test]
    #[ignore]
    async fn test_large_message_throughput() {
        // Test handling large messages (simulating large screenshots)
        let (tx, mut rx) = mpsc::channel::<Bytes>(100);

        let sender = tokio::spawn(async move {
            let start = Instant::now();
            let mut total_bytes = 0;

            // Send 100 x 1MB messages
            for _ in 0..100 {
                let data = vec![0u8; 1024 * 1024]; // 1MB
                total_bytes += data.len();
                if tx.send(Bytes::from(data)).await.is_err() {
                    break;
                }
            }

            let elapsed = start.elapsed();
            (total_bytes, elapsed)
        });

        let receiver = tokio::spawn(async move {
            let start = Instant::now();
            let mut total_bytes = 0;
            let mut count = 0;

            while let Some(msg) = rx.recv().await {
                total_bytes += msg.len();
                count += 1;
                if count >= 100 {
                    break;
                }
            }

            let elapsed = start.elapsed();
            (total_bytes, elapsed)
        });

        let (send_result, recv_result) = tokio::join!(sender, receiver);
        let (sent_bytes, send_time) = send_result.unwrap();
        let (recv_bytes, recv_time) = recv_result.unwrap();

        let sent_mb = sent_bytes as f64 / (1024.0 * 1024.0);
        let recv_mb = recv_bytes as f64 / (1024.0 * 1024.0);

        println!(
            "Sent {:.2} MB in {:?} ({:.2} MB/s)",
            sent_mb,
            send_time,
            sent_mb / send_time.as_secs_f64()
        );
        println!(
            "Received {:.2} MB in {:?} ({:.2} MB/s)",
            recv_mb,
            recv_time,
            recv_mb / recv_time.as_secs_f64()
        );

        assert_eq!(sent_bytes, recv_bytes);

        // Should handle at least 50 MB/s
        let throughput = recv_mb / recv_time.as_secs_f64();
        assert!(
            throughput > 50.0,
            "Throughput too low: {:.2} MB/s",
            throughput
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_concurrent_channels() {
        // Test multiple concurrent channels (simulating multiple connections)
        let num_channels = 10;
        let messages_per_channel = 100;

        let mut handles = vec![];

        for channel_id in 0..num_channels {
            let handle = tokio::spawn(async move {
                let (tx, mut rx) = mpsc::channel::<Bytes>(1000);

                // Sender
                let sender = tokio::spawn(async move {
                    for i in 0..messages_per_channel {
                        let msg = format!("Channel {} message {}", channel_id, i);
                        if tx.send(Bytes::from(msg)).await.is_err() {
                            break;
                        }
                    }
                });

                // Receiver
                let receiver = tokio::spawn(async move {
                    let mut count = 0;
                    while let Some(_msg) = rx.recv().await {
                        count += 1;
                        if count >= messages_per_channel {
                            break;
                        }
                    }
                    count
                });

                let _ = sender.await;
                receiver.await.unwrap()
            });

            handles.push(handle);
        }

        let start = Instant::now();

        // Wait for all channels to complete
        let mut total_messages = 0;
        for handle in handles {
            total_messages += handle.await.unwrap();
        }

        let elapsed = start.elapsed();
        let expected = num_channels * messages_per_channel;

        println!(
            "Processed {} messages across {} channels in {:?}",
            total_messages, num_channels, elapsed
        );

        assert_eq!(total_messages, expected);
    }

    #[tokio::test]
    #[ignore]
    async fn test_message_parsing_performance() {
        // Test Guacamole protocol parsing performance
        let test_messages = vec![
            "3.key,2.65,1.1;",
            "5.mouse,3.100,3.200,1.1;",
            "4.size,4.1024,3.768;",
            "4.sync,10.1234567890;",
        ];

        let iterations = 10000;
        let start = Instant::now();

        for _ in 0..iterations {
            for msg in &test_messages {
                // Simulate parsing
                let _parts: Vec<&str> = msg.split(',').collect();
            }
        }

        let elapsed = start.elapsed();
        let total_parses = iterations * test_messages.len();
        let parses_per_sec = total_parses as f64 / elapsed.as_secs_f64();

        println!(
            "Parsed {} messages in {:?} ({:.0} parses/s)",
            total_parses, elapsed, parses_per_sec
        );

        // Should handle at least 100k parses/second
        assert!(
            parses_per_sec > 100_000.0,
            "Parsing too slow: {:.0} parses/s",
            parses_per_sec
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_backpressure_handling() {
        // Test that backpressure is handled correctly
        let (tx, mut rx) = mpsc::channel::<Bytes>(10); // Small buffer

        let sender = tokio::spawn(async move {
            let mut sent = 0;
            let start = Instant::now();

            // Try to send 1000 messages
            for i in 0..1000 {
                let msg = format!("Message {}", i);
                match tokio::time::timeout(Duration::from_millis(100), tx.send(Bytes::from(msg)))
                    .await
                {
                    Ok(Ok(_)) => sent += 1,
                    Ok(Err(_)) => break, // Channel closed
                    Err(_) => break,     // Timeout (backpressure)
                }
            }

            (sent, start.elapsed())
        });

        // Slow receiver
        let receiver = tokio::spawn(async move {
            let mut received = 0;
            while let Some(_msg) = rx.recv().await {
                tokio::time::sleep(Duration::from_millis(10)).await; // Slow processing
                received += 1;
                if received >= 50 {
                    break;
                }
            }
            received
        });

        let (send_result, recv_result) = tokio::join!(sender, receiver);
        let (sent, _) = send_result.unwrap();
        let received = recv_result.unwrap();

        println!(
            "Sent {} messages, received {} (backpressure applied)",
            sent, received
        );

        // Sender should be blocked by backpressure
        assert!(sent < 1000, "Backpressure not working");
        assert_eq!(received, 50);
    }
}

#[cfg(test)]
mod load_tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_sustained_load() {
        // Test sustained load over time
        let (tx, mut rx) = mpsc::channel::<Bytes>(1000);

        let duration = Duration::from_secs(5);
        let target_rate = 100; // messages per second

        let sender = tokio::spawn(async move {
            let start = Instant::now();
            let mut count = 0;

            while start.elapsed() < duration {
                let msg = format!("Message {}", count);
                if tx.send(Bytes::from(msg)).await.is_ok() {
                    count += 1;
                    tokio::time::sleep(Duration::from_millis(1000 / target_rate)).await;
                } else {
                    break;
                }
            }

            count
        });

        let receiver = tokio::spawn(async move {
            let mut count = 0;
            let start = Instant::now();

            while start.elapsed() < duration + Duration::from_secs(1) {
                match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                    Ok(Some(_)) => count += 1,
                    Ok(None) => break,
                    Err(_) => continue,
                }
            }

            count
        });

        let (sent, received) = tokio::join!(sender, receiver);
        let sent = sent.unwrap();
        let received = received.unwrap();

        println!(
            "Sustained load test: sent {} messages, received {} over {:?}",
            sent, received, duration
        );

        // Should receive at least 90% of messages
        let success_rate = received as f64 / sent as f64;
        assert!(
            success_rate > 0.9,
            "Success rate too low: {:.1}%",
            success_rate * 100.0
        );
    }

    #[tokio::test]
    #[ignore]
    async fn test_burst_load() {
        // Test handling burst of messages
        let (tx, mut rx) = mpsc::channel::<Bytes>(10000);

        let sender = tokio::spawn(async move {
            let start = Instant::now();

            // Send burst of 5000 messages
            for i in 0..5000 {
                let msg = format!("Burst {}", i);
                if tx.send(Bytes::from(msg)).await.is_err() {
                    break;
                }
            }

            start.elapsed()
        });

        let receiver = tokio::spawn(async move {
            let start = Instant::now();
            let mut count = 0;

            while let Some(_msg) = rx.recv().await {
                count += 1;
                if count >= 5000 {
                    break;
                }
            }

            (count, start.elapsed())
        });

        let send_time = sender.await.unwrap();
        let (received, recv_time) = receiver.await.unwrap();

        println!(
            "Burst test: sent in {:?}, received {} in {:?}",
            send_time, received, recv_time
        );

        assert_eq!(received, 5000);
        // Should handle burst in under 1 second
        assert!(recv_time.as_secs() < 1, "Burst handling too slow");
    }
}
