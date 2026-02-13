// Client-side Guacamole protocol handshake
//
// Implements the client side of the guacd wire protocol, allowing a gateway
// to connect to a guacd server and establish a session for a given protocol
// (RDP, SSH, VNC, etc.) or join an existing session.
//
// Protocol flow:
//   1. Gateway -> guacd: select,<protocol>;  (or select,<connection-id>; for join)
//   2. guacd -> Gateway: args,<version>,<arg1>,<arg2>,...;
//   3. (New connections only):
//      a. Gateway -> guacd: size,<width>,<height>;
//      b. Gateway -> guacd: audio,<mimetype1>,...;
//      c. Gateway -> guacd: video,<mimetype1>,...;
//      d. Gateway -> guacd: image,<mimetype1>,...;
//   4. Gateway -> guacd: connect,<version>,<val1>,<val2>,...;
//   5. guacd -> Gateway: ready,<connection-id>;

use anyhow::Result;
use bytes::{Buf, BufMut, BytesMut};
use guacr_protocol::{GuacdInstruction, GuacdParser, PeekError};
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

/// Perform the client-side guacd handshake.
///
/// Connects to a guacd server and negotiates a session for the given protocol.
/// On error, returns Err -- the caller is responsible for notifying the client
/// (e.g., sending a CloseConnection frame over WebRTC).
pub async fn perform_guacd_handshake<R, W>(
    reader: &mut R,
    writer: &mut W,
    channel_id: &str,
    conn_no: u32,
    guacd_params_arc: Arc<Mutex<HashMap<String, String>>>,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + ?Sized,
    W: AsyncWriteExt + Unpin + Send + ?Sized,
{
    let mut handshake_buffer = BytesMut::with_capacity(4096);
    let mut current_handshake_buffer_len = 0;

    async fn read_expected_instruction<'a, S>(
        reader: &'a mut S,
        handshake_buffer: &'a mut BytesMut,
        current_buffer_len: &'a mut usize,
        channel_id: &'a str,
        conn_no: u32,
        expected_opcode: &'a str,
    ) -> Result<GuacdInstruction>
    where
        S: AsyncRead + Unpin + Send + ?Sized,
    {
        loop {
            // Process a peek result and extract what we need
            let process_result = {
                let peek_result =
                    GuacdParser::peek_instruction(&handshake_buffer[..*current_buffer_len]);

                match peek_result {
                    Ok(peeked_instr) => {
                        let instruction_total_len = peeked_instr.total_length_in_buffer;
                        if instruction_total_len == 0 || instruction_total_len > *current_buffer_len
                        {
                            error!(
                                "Invalid instruction length peeked ({}) vs buffer len ({}). Opcode: '{}'. Buffer (approx): {:?} (channel_id: {}, conn_no: {})",
                                instruction_total_len, *current_buffer_len, peeked_instr.opcode, &handshake_buffer[..std::cmp::min(*current_buffer_len, 100)], channel_id, conn_no
                            );
                            return Err(anyhow::anyhow!(
                                "Peeked instruction length is invalid or exceeds buffer."
                            ));
                        }
                        let content_slice = &handshake_buffer[..instruction_total_len - 1];

                        let instruction = GuacdParser::parse_instruction_content(content_slice).map_err(|e|
                            anyhow::anyhow!("Handshake: Conn {}: Failed to parse peeked Guacd instruction (opcode: '{}'): {}. Content: {:?}", conn_no, peeked_instr.opcode, e, content_slice)
                        )?;

                        let expected_opcode_check = peeked_instr.opcode == expected_opcode;

                        Some((instruction, instruction_total_len, expected_opcode_check))
                    }
                    Err(PeekError::Incomplete) => {
                        // Need more data
                        None
                    }
                    Err(err) => {
                        let err_msg = format!("Error peeking Guacd instruction while expecting '{}': {:?}. Buffer content (approx): {:?}", expected_opcode, err, &handshake_buffer[..std::cmp::min(*current_buffer_len, 100)]);
                        error!(
                            "Error during handshake (channel_id: {}, conn_no: {}, error: {})",
                            channel_id, conn_no, err_msg
                        );
                        return Err(anyhow::anyhow!(err_msg));
                    }
                }
            }; // peek_result is dropped here

            // Now we can safely mutate handshake_buffer
            if let Some((instruction, advance_len, expected_opcode_check)) = process_result {
                handshake_buffer.advance(advance_len);
                *current_buffer_len -= advance_len;

                if instruction.opcode == "error" {
                    error!("Guacd sent error during handshake (channel_id: {}, error_opcode: {}, expected_opcode: {}, error_args: {:?})", channel_id, instruction.opcode, expected_opcode, instruction.args);
                    return Err(anyhow::anyhow!(
                        "Guacd sent error '{}' ({:?}) during handshake (expected '{}')",
                        instruction.opcode,
                        instruction.args,
                        expected_opcode
                    ));
                }
                return if expected_opcode_check {
                    Ok(instruction)
                } else {
                    error!("Unexpected Guacd opcode (channel_id: {}, expected_opcode: {}, received_opcode: {}, received_args: {:?})", channel_id, expected_opcode, instruction.opcode, instruction.args);
                    Err(anyhow::anyhow!(
                        "Expected Guacd opcode '{}', got '{}' with args {:?}",
                        expected_opcode,
                        instruction.opcode,
                        instruction.args
                    ))
                };
            }

            // Handle the incomplete case - read more data
            let mut temp_read_buf = [0u8; 1024];
            match reader.read(&mut temp_read_buf).await {
                Ok(0) => {
                    // EOF received - check if there's any remaining instruction in buffer
                    // (especially an error instruction that arrived just before connection close)
                    if *current_buffer_len > 0 {
                        if let Ok(peeked) =
                            GuacdParser::peek_instruction(&handshake_buffer[..*current_buffer_len])
                        {
                            if peeked.total_length_in_buffer <= *current_buffer_len {
                                let content_slice =
                                    &handshake_buffer[..peeked.total_length_in_buffer - 1];
                                if let Ok(instruction) =
                                    GuacdParser::parse_instruction_content(content_slice)
                                {
                                    if instruction.opcode == "error" {
                                        let guacd_error_msg = instruction
                                            .args
                                            .first()
                                            .map(|s| s.as_str())
                                            .unwrap_or("Unknown guacd error");
                                        let error_code = instruction
                                            .args
                                            .get(1)
                                            .map(|s| s.as_str())
                                            .unwrap_or("");
                                        error!("Guacd sent error during handshake (channel_id: {}, error_msg: {}, error_code: {})", channel_id, guacd_error_msg, error_code);

                                        return Err(anyhow::anyhow!(
                                            "Guacd error: {} (code: {})",
                                            guacd_error_msg,
                                            error_code
                                        ));
                                    }
                                }
                            }
                        }
                    }

                    let error_msg = format!(
                        "Connection closed during handshake (expected: {}, buffer_len: {})",
                        expected_opcode, *current_buffer_len
                    );
                    error!(
                        "EOF during Guacd handshake (channel_id: {}, conn_no: {}, error: {})",
                        channel_id, conn_no, error_msg
                    );

                    return Err(anyhow::anyhow!("EOF during Guacd handshake while waiting for '{}' (incomplete data in buffer)", expected_opcode));
                }
                Ok(n_read) => {
                    if handshake_buffer.capacity() < *current_buffer_len + n_read {
                        handshake_buffer
                            .reserve(*current_buffer_len + n_read - handshake_buffer.capacity());
                    }
                    handshake_buffer.put_slice(&temp_read_buf[..n_read]);
                    *current_buffer_len += n_read;
                    debug!("Read more data for handshake, waiting for '{}' (channel_id: {}, conn_no: {}, bytes_read: {}, new_buffer_len: {})", expected_opcode, channel_id, conn_no, n_read, *current_buffer_len);
                }
                Err(e) => {
                    error!("Read error waiting for Guacd instruction (channel_id: {}, expected_opcode: {}, error: {})", channel_id, expected_opcode, e);
                    return Err(e.into());
                }
            }
        }
    }

    let mut guacd_params_locked = guacd_params_arc.lock().await;

    // --- RDP username/domain splitting logic ---
    if let Some(protocol) = guacd_params_locked.get("protocol") {
        if protocol.eq_ignore_ascii_case("rdp") {
            if let Some(username) = guacd_params_locked.get("username").cloned() {
                // Only split on backslash if it's NOT Azure AD format
                if username.starts_with("AzureAD\\") || username.starts_with(".\\AzureAD\\") {
                    debug!("Azure AD format detected - setting security to aad (channel_id: {}, conn_no: {}, username: {})", channel_id, conn_no, username);
                    guacd_params_locked.insert("security".to_string(), "aad".to_string());
                } else if let Some(pos) = username.find('\\') {
                    let domain = &username[..pos];
                    let user = &username[pos + 1..];
                    debug!("Traditional domain found - splitting (channel_id: {}, conn_no: {}, domain: {}, username: {})", channel_id, conn_no, domain, user);
                    guacd_params_locked.insert("username".to_string(), user.to_string());
                    guacd_params_locked.insert("domain".to_string(), domain.to_string());
                }
            }
        }
    }

    let protocol_name_from_params = guacd_params_locked.get("protocol").cloned().unwrap_or_else(|| {
        warn!("Guacd 'protocol' missing in guacd_params, defaulting to 'rdp' for select fallback. (channel_id: {})", channel_id);
        "rdp".to_string()
    });

    let join_connection_id_key = "connectionid";
    let join_connection_id_opt = guacd_params_locked.get(join_connection_id_key).cloned();
    debug!(
        "Checked for join connection ID in guacd_params (channel_id: {}, key_looked_up: {})",
        channel_id, join_connection_id_key
    );

    let select_arg: String;
    if let Some(id_to_join) = &join_connection_id_opt {
        debug!("Guacd Handshake: Preparing to join existing session. (channel_id: {}, session_to_join: {})", channel_id, id_to_join);
        select_arg = id_to_join.clone();
    } else {
        debug!("Guacd Handshake: Preparing for new session with protocol. (channel_id: {}, protocol: {})", channel_id, protocol_name_from_params);
        select_arg = protocol_name_from_params;
    }

    let readonly_param_key = "readonly";
    let readonly_param_value_from_map = guacd_params_locked.get(readonly_param_key).cloned();
    debug!("Initial 'readonly' value from guacd_params_locked for join attempt. (channel_id: {}, readonly_param_value_from_map: {:?})", channel_id, readonly_param_value_from_map);

    let readonly_str_for_join =
        readonly_param_value_from_map.unwrap_or_else(|| "false".to_string());
    debug!("Effective 'readonly_str_for_join' (after unwrap_or_else) for join attempt. (channel_id: {}, readonly_str_for_join: {})", channel_id, readonly_str_for_join);

    let _is_readonly = readonly_str_for_join.eq_ignore_ascii_case("true");
    debug!(
        "Final 'is_readonly' boolean for join attempt. (channel_id: {}, is_readonly_bool: {})",
        channel_id, _is_readonly
    );

    let width_for_new = guacd_params_locked
        .get("width")
        .cloned()
        .unwrap_or_else(|| "1024".to_string());
    let height_for_new = guacd_params_locked
        .get("height")
        .cloned()
        .unwrap_or_else(|| "768".to_string());
    let dpi_for_new = guacd_params_locked
        .get("dpi")
        .cloned()
        .unwrap_or_else(|| "96".to_string());
    let audio_mimetypes_str_for_new = guacd_params_locked
        .get("audio")
        .cloned()
        .unwrap_or_default();
    let video_mimetypes_str_for_new = guacd_params_locked
        .get("video")
        .cloned()
        .unwrap_or_default();
    let image_mimetypes_str_for_new = guacd_params_locked
        .get("image")
        .cloned()
        .unwrap_or_default();

    let mut connect_params_for_new_conn: HashMap<String, String> =
        if join_connection_id_opt.is_none() {
            guacd_params_locked.clone()
        } else {
            HashMap::new()
        };
    drop(guacd_params_locked);

    // Ensure width, height, and dpi are in connect params (needed for guacd connect instruction)
    // These may have been extracted with defaults if not originally in guacd_params
    if join_connection_id_opt.is_none() {
        connect_params_for_new_conn.insert("width".to_string(), width_for_new.clone());
        connect_params_for_new_conn.insert("height".to_string(), height_for_new.clone());
        connect_params_for_new_conn.insert("dpi".to_string(), dpi_for_new.clone());
    }

    let select_instruction = GuacdInstruction::new("select".to_string(), vec![select_arg.clone()]);
    debug!(
        "Guacd Handshake: Sending 'select' (channel_id: {}, instruction: {:?})",
        channel_id, select_instruction
    );
    writer
        .write_all(&GuacdParser::guacd_encode_instruction(&select_instruction))
        .await?;
    writer.flush().await?;

    debug!(
        "Guacd Handshake: Waiting for 'args' (channel_id: {})",
        channel_id
    );
    let args_instruction = read_expected_instruction(
        reader,
        &mut handshake_buffer,
        &mut current_handshake_buffer_len,
        channel_id,
        conn_no,
        "args",
    )
    .await?;
    debug!(
        "Guacd Handshake: Received 'args' from Guacd server (channel_id: {}, received_args: {:?})",
        channel_id, args_instruction.args
    );

    const EXPECTED_GUACD_VERSION: &str = "VERSION_1_5_0";
    let connect_version_arg = args_instruction.args.first().cloned().unwrap_or_else(|| {
        warn!(
            "'args' instruction missing version, defaulting to {} (channel_id: {}, conn_no: {})",
            EXPECTED_GUACD_VERSION, channel_id, conn_no
        );
        EXPECTED_GUACD_VERSION.to_string()
    });
    if connect_version_arg != EXPECTED_GUACD_VERSION {
        warn!("Guacd version mismatch. Expected: '{}', Received: '{}'. Proceeding with received version for connect. (channel_id: {}, conn_no: {})", EXPECTED_GUACD_VERSION, connect_version_arg, channel_id, conn_no);
    }

    let mut connect_args: Vec<String> = Vec::new();
    connect_args.push(connect_version_arg);

    if join_connection_id_opt.is_some() {
        info!(
            "Guacd Handshake: Preparing 'connect' for JOINING session. (channel_id: {})",
            channel_id
        );
        let is_readonly = readonly_str_for_join.eq_ignore_ascii_case("true");
        debug!("Readonly status for join. (channel_id: {}, requested_readonly_param: {}, is_readonly_for_connect: {})", channel_id, readonly_str_for_join, is_readonly);

        for (idx, arg_name_from_guacd) in args_instruction.args.iter().enumerate() {
            if idx == 0 {
                continue;
            }

            let is_readonly_arg_name_literal = "read-only";
            let is_current_arg_readonly_keyword =
                arg_name_from_guacd == is_readonly_arg_name_literal;

            debug!("Looping for connect_args (join). Comparing '{}' with '{}' (channel_id: {}, conn_no: {}, current_arg_name_from_guacd: {}, is_readonly_param_from_config: {}, is_current_arg_the_readonly_keyword: {})", arg_name_from_guacd, is_readonly_arg_name_literal, channel_id, conn_no, arg_name_from_guacd, is_readonly, is_current_arg_readonly_keyword);

            if is_current_arg_readonly_keyword {
                let value_to_push = if is_readonly {
                    "true".to_string()
                } else {
                    "".to_string()
                };
                debug!("Pushing to connect_args for 'read-only' keyword (channel_id: {}, conn_no: {}, arg_name_being_processed: {}, is_readonly_flag_for_push: {}, value_being_pushed_for_readonly_arg: {})", channel_id, conn_no, arg_name_from_guacd, is_readonly, value_to_push);
                connect_args.push(value_to_push);
            } else {
                connect_args.push("".to_string());
            }
        }
    } else {
        debug!(
            "Guacd Handshake: Preparing 'connect' for NEW session. (channel_id: {})",
            channel_id
        );

        let parse_mimetypes = |mimetype_str: &str| -> Vec<String> {
            if mimetype_str.is_empty() {
                return Vec::new();
            }
            serde_json::from_str::<Vec<String>>(mimetype_str)
                .unwrap_or_else(|e| {
                    debug!("Failed to parse mimetype string '{}' as JSON array, splitting by comma as fallback. (channel_id: {}, conn_no: {}, error: {})", mimetype_str, channel_id, conn_no, e);
                    mimetype_str.split(',').map(String::from).filter(|s| !s.is_empty()).collect()
                })
        };

        // Standard guacd size instruction: size,<width>,<height>,<dpi>;
        // DPI must be included for terminal protocols (SSH, Telnet, etc.) where
        // guacd uses it for Pango font scaling: effective_size = font_size * dpi / 96
        let size_parts: Vec<String> = width_for_new
            .split(',')
            .chain(height_for_new.split(','))
            .chain(std::iter::once(dpi_for_new.as_str()))
            .map(String::from)
            .collect();
        debug!(
            "Guacd Handshake (new): Sending 'size' (channel_id: {})",
            channel_id
        );

        let size_instruction = GuacdInstruction::new("size".to_string(), size_parts.clone());
        if size_parts.len() >= 2 {
            debug!("HANDSHAKE: Client initial size instruction (channel_id: {}, conn_no: {}, width: {}, height: {}, dpi: {}) - DPI included in size instruction", channel_id, conn_no, size_parts.first().map(|s| s.as_str()).unwrap_or("1024"), size_parts.get(1).map(|s| s.as_str()).unwrap_or("768"), dpi_for_new);
        }

        writer
            .write_all(&GuacdParser::guacd_encode_instruction(&size_instruction))
            .await?;
        writer.flush().await?;

        let audio_mimetypes = parse_mimetypes(&audio_mimetypes_str_for_new);
        debug!(
            "Guacd Handshake (new): Sending 'audio' (channel_id: {})",
            channel_id
        );
        writer
            .write_all(&GuacdParser::guacd_encode_instruction(
                &GuacdInstruction::new("audio".to_string(), audio_mimetypes),
            ))
            .await?;
        writer.flush().await?;

        let video_mimetypes = parse_mimetypes(&video_mimetypes_str_for_new);
        debug!(
            "Guacd Handshake (new): Sending 'video' (channel_id: {})",
            channel_id
        );
        writer
            .write_all(&GuacdParser::guacd_encode_instruction(
                &GuacdInstruction::new("video".to_string(), video_mimetypes),
            ))
            .await?;
        writer.flush().await?;

        let image_mimetypes = parse_mimetypes(&image_mimetypes_str_for_new);
        debug!(
            "Guacd Handshake (new): Sending 'image' (channel_id: {})",
            channel_id
        );
        writer
            .write_all(&GuacdParser::guacd_encode_instruction(
                &GuacdInstruction::new("image".to_string(), image_mimetypes),
            ))
            .await?;
        writer.flush().await?;

        // Pre-normalize config keys once for efficient lookup
        let normalized_config_map: HashMap<String, String> = connect_params_for_new_conn
            .iter()
            .map(|(key, value)| {
                let normalized_key = key.replace(&['-', '_'][..], "").to_ascii_lowercase();
                (normalized_key, value.clone())
            })
            .collect();

        for arg_name_from_guacd in args_instruction.args.iter().skip(1) {
            // Normalize the guacd parameter name by removing hyphens/underscores and converting to lowercase
            let normalized_guacd_param = arg_name_from_guacd
                .replace(&['-', '_'][..], "")
                .to_ascii_lowercase();

            // Look up the parameter value using the normalized key
            let param_value = normalized_config_map
                .get(&normalized_guacd_param)
                .cloned()
                .unwrap_or_else(String::new);

            connect_args.push(param_value);
        }
    }

    let connect_instruction = GuacdInstruction::new("connect".to_string(), connect_args.clone());
    debug!(
        "Guacd Handshake: Sending 'connect' (channel_id: {})",
        channel_id
    );
    debug!(
        "Guacd Handshake: params (channel_id: {}, instruction: {:?})",
        channel_id, connect_instruction
    );
    writer
        .write_all(&GuacdParser::guacd_encode_instruction(&connect_instruction))
        .await?;
    writer.flush().await?;

    debug!(
        "Guacd Handshake: Waiting for 'ready' (channel_id: {})",
        channel_id
    );
    let ready_instruction = read_expected_instruction(
        reader,
        &mut handshake_buffer,
        &mut current_handshake_buffer_len,
        channel_id,
        conn_no,
        "ready",
    )
    .await?;
    if let Some(client_id_from_ready) = ready_instruction.args.first() {
        debug!(
            "Guacd handshake completed. (channel_id: {}, guacd_client_id: {})",
            channel_id, client_id_from_ready
        );
    } else {
        debug!(
            "Guacd handshake completed. No client ID received with 'ready'. (channel_id: {})",
            channel_id
        );
    }
    Ok(())
}
