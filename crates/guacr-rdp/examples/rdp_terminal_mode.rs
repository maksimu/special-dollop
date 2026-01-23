// Example: RDP with Sixel graphics rendering
//
// This example shows how to connect to an RDP server and render
// the output using Sixel graphics in the terminal.
//
// Usage:
//   cargo run --example rdp_terminal_mode -- --host 192.168.1.100

use bytes::Bytes;
use guacr_handlers::ProtocolHandler;
use guacr_rdp::{RdpConfig, RdpHandler};
use guacr_terminal::GraphicsMode;
use std::collections::HashMap;
use std::env;
use std::io::Read;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize simple logging
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }

    // Parse command line arguments
    let args: Vec<String> = env::args().collect();

    let mut host = "localhost".to_string();
    let mut port = "3389".to_string();
    let mut username = "Administrator".to_string();
    let mut password = "".to_string();
    let mode = GraphicsMode::Sixel;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--host" | "-h" => {
                if i + 1 < args.len() {
                    host = args[i + 1].clone();
                    i += 1;
                }
            }
            "--port" | "-p" => {
                if i + 1 < args.len() {
                    port = args[i + 1].clone();
                    i += 1;
                }
            }
            "--username" | "-u" => {
                if i + 1 < args.len() {
                    username = args[i + 1].clone();
                    i += 1;
                }
            }
            "--password" | "-P" => {
                if i + 1 < args.len() {
                    password = args[i + 1].clone();
                    i += 1;
                }
            }
            "--help" => {
                print_usage();
                return Ok(());
            }
            _ => {}
        }
        i += 1;
    }

    println!("RDP Terminal Mode Example");
    println!("=========================");
    println!("Host: {}:{}", host, port);
    println!("Username: {}", username);
    println!("Graphics mode: {:?}", mode);
    println!();

    // Create RDP config with terminal mode enabled
    let config = RdpConfig {
        terminal_mode: Some(mode),
        terminal_cols: 240,
        terminal_rows: 60,
        ..Default::default()
    };

    let handler = RdpHandler::new(config);

    // Setup channels
    let (to_client, mut from_handler) = mpsc::channel(128);
    let (to_handler, from_client) = mpsc::channel(128);

    // Connection parameters
    let mut params = HashMap::new();
    params.insert("hostname".to_string(), host);
    params.insert("port".to_string(), port);
    params.insert("username".to_string(), username);
    if !password.is_empty() {
        params.insert("password".to_string(), password);
    }
    params.insert("ignore-certificate".to_string(), "true".to_string());

    // Spawn handler task
    let handler_task = tokio::spawn(async move {
        if let Err(e) = handler.connect(params, to_client, from_client).await {
            eprintln!("RDP connection error: {:?}", e);
        }
    });

    // Receive and print terminal output
    let output_task = tokio::spawn(async move {
        while let Some(data) = from_handler.recv().await {
            // Print terminal graphics output (Sixel escape sequences)
            print!("{}", String::from_utf8_lossy(&data));
            std::io::Write::flush(&mut std::io::stdout()).ok();
        }
    });

    // Handle keyboard input from terminal
    let input_task = tokio::spawn(async move {
        // SIMPLIFIED INPUT DEMO
        //
        // This example reads raw stdin and sends ASCII characters to RDP.
        // It works for basic typing but has limitations:
        //
        // Limitations:
        // - Line-buffered (need to press Enter)
        // - No raw mode (terminal processes Ctrl+C, etc.)
        // - No special keys (arrows, F1-F12, etc.)
        // - No mouse input from terminal
        //
        // The RDP handler DOES support all of these via Guacamole protocol:
        // - handler.rs processes "key" instructions with X11 keysyms
        // - handler.rs processes "mouse" instructions with buttons/position
        // - Works with any client that sends proper Guacamole protocol
        //
        // To make this example fully interactive, you would need:
        // 1. Raw mode: crossterm::terminal::enable_raw_mode()
        // 2. Event parsing: crossterm::event::read() for keys/mouse
        // 3. X11 keysym mapping for special keys
        //
        // For a complete terminal client, see guacr-ssh which uses crossterm.

        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1024];

        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    // Send each ASCII byte as a key event
                    // Note: ASCII values happen to match X11 keysyms for printable chars
                    for &byte in &buf[..n] {
                        let keysym = byte as u32;

                        // Send key press
                        let key_down = format!("3.key,{},1.1;", keysym);
                        if to_handler.send(Bytes::from(key_down)).await.is_err() {
                            return;
                        }

                        // Send key release
                        let key_up = format!("3.key,{},1.0;", keysym);
                        if to_handler.send(Bytes::from(key_up)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Wait for tasks
    tokio::select! {
        _ = handler_task => {
            println!("Handler task completed");
        }
        _ = output_task => {
            println!("Output task completed");
        }
        _ = input_task => {
            println!("Input task completed");
        }
        _ = tokio::signal::ctrl_c() => {
            println!("\nReceived Ctrl+C, exiting...");
        }
    }

    Ok(())
}

fn print_usage() {
    println!("Usage: rdp_terminal_mode [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --host, -h HOST        RDP server hostname (default: localhost)");
    println!("  --port, -p PORT        RDP server port (default: 3389)");
    println!("  --username, -u USER    Username (default: Administrator)");
    println!("  --password, -P PASS    Password (default: empty)");
    println!("  --mode, -m MODE        Graphics mode:");
    println!("                           sixel       - Sixel graphics (only supported mode)");
    println!("  --help                 Show this help message");
    println!();
    println!("Examples:");
    println!("  cargo run --example rdp_terminal_mode -- --host 192.168.1.100");
    println!("  cargo run --example rdp_terminal_mode -- --host server.local --username user --password pass");
}
