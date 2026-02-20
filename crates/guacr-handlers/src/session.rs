// Session lifecycle helpers shared across all protocol handlers.
//
// Provides consistent ready, name, disconnect, and bell instructions
// so every handler uses the same wire format for session startup/shutdown.

use bytes::Bytes;
use guacr_protocol::{format_bell_audio, format_instruction};
use log::{debug, warn};
use tokio::sync::mpsc;

use crate::{HandlerError, MultiFormatRecorder, RecordingDirection, Result};

/// Send a `ready` instruction to the client.
///
/// This signals that the backend is connected and ready to process instructions.
/// Propagates errors because if the client is gone at startup, the session should abort.
pub async fn send_ready(to_client: &mpsc::Sender<Bytes>, connection_id: &str) -> Result<()> {
    let instr = format_instruction("ready", &[connection_id]);
    debug!("Session: Sending ready instruction: {}", instr);
    to_client
        .send(Bytes::from(instr))
        .await
        .map_err(|e| HandlerError::ChannelError(e.to_string()))
}

/// Send a `name` instruction to the client.
///
/// Tells the client the display name for this connection (e.g., "SSH", "RDP", "VNC").
/// Propagates errors.
pub async fn send_name(to_client: &mpsc::Sender<Bytes>, protocol_name: &str) -> Result<()> {
    let instr = format_instruction("name", &[protocol_name]);
    to_client
        .send(Bytes::from(instr))
        .await
        .map_err(|e| HandlerError::ChannelError(e.to_string()))
}

/// Send a `disconnect` instruction to the client.
///
/// Best-effort: swallows send errors since the client may already be gone
/// when we're shutting down.
pub async fn send_disconnect(to_client: &mpsc::Sender<Bytes>) {
    let instr = format_instruction("disconnect", &[]);
    if to_client.send(Bytes::from(instr)).await.is_err() {
        debug!("Session: Failed to send disconnect (client may have closed)");
    }
}

/// Send a bell/beep audio notification to the client.
///
/// Sends the standard Guacamole audio sequence: audio + blob + end instructions.
/// Propagates errors.
pub async fn send_bell(to_client: &mpsc::Sender<Bytes>, stream_id: u32) -> Result<()> {
    let bell_instrs = format_bell_audio(stream_id);
    for instr in bell_instrs {
        to_client
            .send(Bytes::from(instr))
            .await
            .map_err(|e| HandlerError::ChannelError(e.to_string()))?;
    }
    Ok(())
}

/// Send a Guacamole instruction to the client and record it (if recording is enabled).
///
/// This is the shared implementation used by all protocol handlers (SSH, Telnet,
/// Serial, Database, RDP, VNC). Records the instruction as server-to-client
/// direction before sending.
pub async fn send_and_record(
    to_client: &mpsc::Sender<Bytes>,
    recorder: &mut Option<MultiFormatRecorder>,
    instruction: Bytes,
) -> std::result::Result<(), String> {
    if let Some(ref mut rec) = recorder {
        if let Err(e) = rec.record_instruction(RecordingDirection::ServerToClient, &instruction) {
            warn!("Failed to record server instruction: {}", e);
        }
    }
    to_client
        .send(instruction)
        .await
        .map_err(|e| format!("Send failed: {}", e))
}

/// Record a client-to-server instruction (if recording is enabled).
///
/// Shared implementation used by all protocol handlers. Only records; does not send.
pub fn record_client_input(recorder: &mut Option<MultiFormatRecorder>, instruction: &Bytes) {
    if let Some(ref mut rec) = recorder {
        if let Err(e) = rec.record_instruction(RecordingDirection::ClientToServer, instruction) {
            warn!("Failed to record client input: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_send_ready() {
        let (tx, mut rx) = mpsc::channel(16);
        send_ready(&tx, "ssh-ready").await.unwrap();
        let msg = rx.recv().await.unwrap();
        let s = String::from_utf8(msg.to_vec()).unwrap();
        assert_eq!(s, "5.ready,9.ssh-ready;");
    }

    #[tokio::test]
    async fn test_send_name() {
        let (tx, mut rx) = mpsc::channel(16);
        send_name(&tx, "SSH").await.unwrap();
        let msg = rx.recv().await.unwrap();
        let s = String::from_utf8(msg.to_vec()).unwrap();
        assert_eq!(s, "4.name,3.SSH;");
    }

    #[tokio::test]
    async fn test_send_disconnect() {
        let (tx, mut rx) = mpsc::channel(16);
        send_disconnect(&tx).await;
        let msg = rx.recv().await.unwrap();
        let s = String::from_utf8(msg.to_vec()).unwrap();
        assert_eq!(s, "10.disconnect;");
    }

    #[tokio::test]
    async fn test_send_bell() {
        let (tx, mut rx) = mpsc::channel(16);
        send_bell(&tx, 100).await.unwrap();
        // Should produce 3 instructions: audio, blob, end
        let audio = rx.recv().await.unwrap();
        let blob = rx.recv().await.unwrap();
        let end = rx.recv().await.unwrap();
        let audio_s = String::from_utf8(audio.to_vec()).unwrap();
        let end_s = String::from_utf8(end.to_vec()).unwrap();
        assert!(audio_s.contains("audio"));
        assert!(audio_s.contains("audio/wav"));
        let blob_s = String::from_utf8(blob.to_vec()).unwrap();
        assert!(blob_s.contains("blob"));
        assert!(end_s.contains("end"));
    }

    #[tokio::test]
    async fn test_send_ready_closed_channel() {
        let (tx, rx) = mpsc::channel(16);
        drop(rx);
        let result = send_ready(&tx, "test").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_send_disconnect_closed_channel() {
        let (tx, rx) = mpsc::channel(16);
        drop(rx);
        // Should not panic, just log debug
        send_disconnect(&tx).await;
    }

    #[tokio::test]
    async fn test_send_and_record_without_recorder() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut recorder: Option<MultiFormatRecorder> = None;
        let instr = Bytes::from("5.ready,4.test;");
        send_and_record(&tx, &mut recorder, instr).await.unwrap();
        let msg = rx.recv().await.unwrap();
        assert_eq!(&msg[..], b"5.ready,4.test;");
    }

    #[tokio::test]
    async fn test_send_and_record_closed_channel() {
        let (tx, rx) = mpsc::channel(16);
        drop(rx);
        let mut recorder: Option<MultiFormatRecorder> = None;
        let result = send_and_record(&tx, &mut recorder, Bytes::from("test")).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_record_client_input_without_recorder() {
        let mut recorder: Option<MultiFormatRecorder> = None;
        // Should not panic when recorder is None
        record_client_input(&mut recorder, &Bytes::from("3.key,1.1,5.65536;"));
    }
}
