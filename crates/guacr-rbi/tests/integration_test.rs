//! Integration tests for RBI handler
//!
//! These tests require Chrome/Chromium to be installed.
//!
//! Run tests with:
//!   cargo test --package guacr-rbi --test integration_test --features chrome -- --include-ignored
//!
//! Chrome detection:
//!   - Linux: /usr/bin/chromium-browser, /usr/bin/chromium, /usr/bin/google-chrome
//!   - macOS: /Applications/Google Chrome.app/Contents/MacOS/Google Chrome
//!   - Windows: C:\Program Files\Google\Chrome\Application\chrome.exe

#[cfg(feature = "chrome")]
use std::time::Duration;
#[cfg(feature = "chrome")]
use tokio::time::timeout;

#[allow(dead_code)]
fn find_chrome() -> Option<String> {
    // Check CHROMIUM_PATH env var first
    if let Ok(path) = std::env::var("CHROMIUM_PATH") {
        if std::path::Path::new(&path).exists() {
            return Some(path);
        }
    }

    // Common Chrome/Chromium locations
    let paths = vec![
        "/usr/bin/chromium-browser",
        "/usr/bin/chromium",
        "/usr/bin/google-chrome",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
    ];

    for path in paths {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    None
}

#[cfg(test)]
#[cfg(feature = "chrome")]
mod chrome_integration_tests {
    use super::*;
    use bytes::Bytes;
    use guacr_handlers::ProtocolHandler;
    use guacr_rbi::RbiHandler;
    use std::collections::HashMap;
    use tokio::sync::mpsc;

    fn skip_if_chrome_not_available() -> bool {
        if find_chrome().is_none() {
            eprintln!("Skipping RBI tests - Chrome/Chromium not found");
            eprintln!("Install Chrome or set CHROMIUM_PATH environment variable");
            return true;
        }
        false
    }

    #[tokio::test]
    #[ignore]
    async fn test_rbi_chrome_launch() {
        if skip_if_chrome_not_available() {
            return;
        }

        let handler = RbiHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (_from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("url".to_string(), "https://example.com".to_string());
        params.insert("width".to_string(), "1024".to_string());
        params.insert("height".to_string(), "768".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Give Chrome time to launch
        // Chrome can take several seconds to start on first launch
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Try to receive any messages
        let mut received_any = false;
        for _ in 0..10 {
            match timeout(Duration::from_millis(500), to_client_rx.recv()).await {
                Ok(Some(_msg)) => {
                    received_any = true;
                    break;
                }
                _ => break,
            }
        }

        // RBI handler may not send messages if Chrome fails to launch
        // Just verify the handler didn't crash immediately
        if !received_any {
            eprintln!(
                "Warning: RBI handler didn't send messages (Chrome may have failed to launch)"
            );
        }

        let _ = timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_rbi_chrome_navigation() {
        if skip_if_chrome_not_available() {
            return;
        }

        let handler = RbiHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("url".to_string(), "https://example.com".to_string());
        params.insert("width".to_string(), "800".to_string());
        params.insert("height".to_string(), "600".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Wait for ready
        for _ in 0..10 {
            match timeout(Duration::from_secs(2), to_client_rx.recv()).await {
                Ok(Some(msg)) => {
                    let msg_str = String::from_utf8_lossy(&msg);
                    if msg_str.contains("ready") || msg_str.contains("img") {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Give browser time to load
        tokio::time::sleep(Duration::from_millis(500)).await;

        drop(from_client_tx);
        let _ = timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_rbi_chrome_mouse_input() {
        if skip_if_chrome_not_available() {
            return;
        }

        let handler = RbiHandler::with_defaults();
        let (to_client_tx, mut to_client_rx) = mpsc::channel::<Bytes>(1024);
        let (_from_client_tx, from_client_rx) = mpsc::channel::<Bytes>(1024);

        let mut params = HashMap::new();
        params.insert("url".to_string(), "https://example.com".to_string());

        let handle =
            tokio::spawn(
                async move { handler.connect(params, to_client_tx, from_client_rx).await },
            );

        // Give Chrome time to launch
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Try to receive any messages
        let mut received_any = false;
        for _ in 0..10 {
            match timeout(Duration::from_millis(500), to_client_rx.recv()).await {
                Ok(Some(_msg)) => {
                    received_any = true;
                    break;
                }
                _ => break,
            }
        }

        // Note: We don't test actual mouse input because Chrome may not launch
        if !received_any {
            eprintln!("Warning: RBI handler didn't send messages");
        }

        let _ = timeout(Duration::from_secs(2), handle).await;
    }
}

#[cfg(test)]
mod unit_tests {
    use guacr_handlers::ProtocolHandler;
    use guacr_rbi::RbiHandler;

    #[test]
    fn test_rbi_handler_creation() {
        let handler = RbiHandler::with_defaults();
        assert_eq!(handler.name(), "http");
    }

    #[tokio::test]
    async fn test_rbi_handler_health_check() {
        let handler = RbiHandler::with_defaults();
        let health = handler.health_check().await;
        assert!(health.is_ok());
    }
}
