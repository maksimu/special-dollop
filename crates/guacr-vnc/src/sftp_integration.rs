// SFTP integration helper for VNC handler
// Reuses code from guacr-sftp handler

#[cfg(feature = "sftp")]
use guacr_sftp::ChannelStreamAdapter;
#[cfg(feature = "sftp")]
use log::info;
#[cfg(feature = "sftp")]
use russh::client;
#[cfg(feature = "sftp")]
use russh_sftp::client::SftpSession;
#[cfg(feature = "sftp")]
use std::sync::Arc;

/// Establish SFTP session over SSH
#[cfg(feature = "sftp")]
pub async fn establish_sftp_session(
    hostname: &str,
    port: u16,
    username: &str,
    password: Option<&str>,
    private_key: Option<&str>,
    private_key_passphrase: Option<&str>,
) -> Result<SftpSession, String> {
    use russh_keys::decode_secret_key;

    // Create SSH config
    let config = Arc::new(client::Config::default());

    // Create minimal handler
    struct SftpClientHandler;
    impl client::Handler for SftpClientHandler {
        type Error = russh::Error;
    }

    // Connect SSH
    let mut session = client::connect(config, (hostname, port), SftpClientHandler)
        .await
        .map_err(|e| format!("SSH connection failed: {}", e))?;

    // Authenticate
    let authenticated = if let Some(pwd) = password {
        session
            .authenticate_password(username, pwd)
            .await
            .map_err(|e| format!("Password auth failed: {}", e))?
    } else if let Some(key_data) = private_key {
        let passphrase = private_key_passphrase;
        let key = decode_secret_key(key_data, passphrase)
            .map_err(|e| format!("Key decode failed: {}", e))?;
        session
            .authenticate_publickey(username, Arc::new(key))
            .await
            .map_err(|e| format!("Key auth failed: {}", e))?
    } else {
        return Err("password or private_key required".to_string());
    };

    if !authenticated {
        return Err("Authentication failed".to_string());
    }

    // Open SFTP channel
    let channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("Channel open failed: {}", e))?;

    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("SFTP subsystem request failed: {}", e))?;

    // Create SFTP session
    let (channel_stream, _forwarder_handle) = ChannelStreamAdapter::new(channel);
    let sftp = SftpSession::new(channel_stream)
        .await
        .map_err(|e| format!("SFTP session creation failed: {}", e))?;

    info!(
        "SFTP session established for {}@{}:{}",
        username, hostname, port
    );
    Ok(sftp)
}

/// Handle Guacamole file instruction for SFTP
///
/// Format:
/// - Upload: file,<stream>,<mimetype>,<filename>,<base64-data>
/// - Download: file,<stream>,<mimetype>,<filename>
#[cfg(feature = "sftp")]
pub async fn handle_sftp_file_request(
    sftp: &mut SftpSession,
    args: &[String],
    to_client: &tokio::sync::mpsc::Sender<bytes::Bytes>,
) -> Result<(), String> {
    use base64::Engine;
    use bytes::Bytes;
    use guacr_protocol::{format_blob, format_end};

    if args.len() < 3 {
        return Err(
            "Invalid file instruction: need at least stream, mimetype, filename".to_string(),
        );
    }

    let stream_id: u32 = args[0]
        .parse()
        .map_err(|_| "Invalid stream ID".to_string())?;
    let _mimetype = &args[1];
    let filename = &args[2];

    // Check if this is upload (has data) or download request
    if args.len() >= 4 {
        // Upload: file,<stream>,<mimetype>,<filename>,<base64-data>
        let data = base64::engine::general_purpose::STANDARD
            .decode(&args[3])
            .map_err(|e| format!("Invalid base64 data: {}", e))?;

        // Write file via SFTP (russh-sftp 2.1 API)
        sftp.write(filename.clone(), &data)
            .await
            .map_err(|e| format!("SFTP write failed: {}", e))?;

        info!("SFTP: File uploaded: {} ({} bytes)", filename, data.len());

        // Send success response
        let response = format_end(stream_id);
        to_client
            .send(Bytes::from(response))
            .await
            .map_err(|e| format!("Failed to send response: {}", e))?;
    } else {
        // Download: file,<stream>,<mimetype>,<filename>
        // Read file via SFTP (russh-sftp 2.1 API)
        let file_data = sftp
            .read(filename.clone())
            .await
            .map_err(|e| format!("SFTP read failed: {}", e))?;

        // Send file data via blob instructions
        let base64_data = base64::engine::general_purpose::STANDARD.encode(&file_data);

        // Send blob instruction
        let blob_instr = format_blob(stream_id, &base64_data);
        to_client
            .send(Bytes::from(blob_instr))
            .await
            .map_err(|e| format!("Failed to send blob: {}", e))?;

        // Send end instruction
        let end_instr = format_end(stream_id);
        to_client
            .send(Bytes::from(end_instr))
            .await
            .map_err(|e| format!("Failed to send end: {}", e))?;

        info!(
            "SFTP: File downloaded: {} ({} bytes)",
            filename,
            file_data.len()
        );
    }

    Ok(())
}
