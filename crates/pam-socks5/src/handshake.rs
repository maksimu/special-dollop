//! SOCKS5 protocol handshake logic.
//!
//! Handles the SOCKS5 greeting, authentication method negotiation,
//! and connection request parsing over any async stream.

use anyhow::{anyhow, Result};
use log::error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// SOCKS5 protocol constants
pub const SOCKS5_VERSION: u8 = 0x05;
pub const SOCKS5_AUTH_METHOD_NONE: u8 = 0x00;
pub const SOCKS5_AUTH_FAILED: u8 = 0xFF;
pub const SOCKS5_CMD_CONNECT: u8 = 0x01;
pub const SOCKS5_ADDR_TYPE_IPV4: u8 = 0x01;
pub const SOCKS5_ATYP_DOMAIN: u8 = 0x03;
pub const SOCKS5_ATYP_IPV6: u8 = 0x04;
pub const SOCKS5_FAIL: u8 = 0x01;

/// Compile-time SOCKS5 success response constant for zero-allocation response.
/// Format: [VER=0x05, REP=0x00, RSV=0x00, ATYP=0x01, BND.ADDR=0x00000000, BND.PORT=0x0000]
pub const SOCKS5_SUCCESS_RESPONSE: [u8; 10] =
    [0x05, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

/// Parsed SOCKS5 destination address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Socks5Address {
    IPv4(String),
    Domain(String),
    IPv6(String),
}

impl Socks5Address {
    /// Returns the address as a string (hostname or IP).
    pub fn as_str(&self) -> &str {
        match self {
            Socks5Address::IPv4(s) => s,
            Socks5Address::Domain(s) => s,
            Socks5Address::IPv6(s) => s,
        }
    }
}

/// Parsed SOCKS5 command type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Socks5Command {
    Connect,
}

/// SOCKS5 handshake handler, generic over any async stream.
///
/// Follows the same pattern as `GuacdHandshake<S>` from `guacr-guacd`.
pub struct Socks5Handshake<S> {
    stream: S,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Socks5Handshake<S> {
    pub fn new(stream: S) -> Self {
        Self { stream }
    }

    /// Consume self and return the underlying stream.
    pub fn into_inner(self) -> S {
        self.stream
    }

    /// Step 1: Negotiate authentication method.
    /// Reads the client greeting and responds with the selected auth method.
    pub async fn negotiate_auth(&mut self) -> Result<()> {
        let mut buf = [0u8; 2];
        self.stream.read_exact(&mut buf).await?;

        let socks_version = buf[0];
        let num_methods = buf[1];

        if socks_version != SOCKS5_VERSION {
            error!("Invalid SOCKS version: {}", socks_version);
            self.stream
                .write_all(&[SOCKS5_VERSION, SOCKS5_AUTH_FAILED])
                .await?;
            return Err(anyhow!("Invalid SOCKS version: {}", socks_version));
        }

        // Read authentication methods
        let mut methods = vec![0u8; num_methods as usize];
        self.stream.read_exact(&mut methods).await?;

        let selected_method = if methods.contains(&SOCKS5_AUTH_METHOD_NONE) {
            SOCKS5_AUTH_METHOD_NONE
        } else {
            SOCKS5_AUTH_FAILED
        };

        self.stream
            .write_all(&[SOCKS5_VERSION, selected_method])
            .await?;

        if selected_method == SOCKS5_AUTH_FAILED {
            return Err(anyhow!("No supported authentication method"));
        }

        Ok(())
    }

    /// Step 2: Read and parse the connection request.
    /// Returns the command, address, and port.
    pub async fn read_request(&mut self) -> Result<(Socks5Command, Socks5Address, u16)> {
        let mut buf = [0u8; 4];
        self.stream.read_exact(&mut buf).await?;

        let version = buf[0];
        let cmd = buf[1];
        let _reserved = buf[2];
        let addr_type = buf[3];

        if version != SOCKS5_VERSION {
            error!("Invalid SOCKS version in request: {}", version);
            self.send_error(SOCKS5_FAIL).await?;
            return Err(anyhow!("Invalid SOCKS version in request: {}", version));
        }

        let command = match cmd {
            SOCKS5_CMD_CONNECT => Socks5Command::Connect,
            _ => {
                error!("Unsupported SOCKS command: {}", cmd);
                self.send_error(0x07).await?; // Command not supported
                return Err(anyhow!("Unsupported SOCKS command: {}", cmd));
            }
        };

        // Parse destination address
        let address = match addr_type {
            SOCKS5_ADDR_TYPE_IPV4 => {
                let mut addr = [0u8; 4];
                self.stream.read_exact(&mut addr).await?;
                Socks5Address::IPv4(format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]))
            }
            SOCKS5_ATYP_DOMAIN => {
                let mut len = [0u8; 1];
                self.stream.read_exact(&mut len).await?;
                let domain_len = len[0] as usize;
                let mut domain = vec![0u8; domain_len];
                self.stream.read_exact(&mut domain).await?;
                Socks5Address::Domain(String::from_utf8(domain)?)
            }
            SOCKS5_ATYP_IPV6 => {
                let mut addr = [0u8; 16];
                self.stream.read_exact(&mut addr).await?;
                Socks5Address::IPv6(format!(
                    "{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
                    ((addr[0] as u16) << 8) | (addr[1] as u16),
                    ((addr[2] as u16) << 8) | (addr[3] as u16),
                    ((addr[4] as u16) << 8) | (addr[5] as u16),
                    ((addr[6] as u16) << 8) | (addr[7] as u16),
                    ((addr[8] as u16) << 8) | (addr[9] as u16),
                    ((addr[10] as u16) << 8) | (addr[11] as u16),
                    ((addr[12] as u16) << 8) | (addr[13] as u16),
                    ((addr[14] as u16) << 8) | (addr[15] as u16)
                ))
            }
            _ => {
                error!("Unsupported address type: {}", addr_type);
                self.send_error(0x08).await?; // Address type not supported
                return Err(anyhow!("Unsupported address type: {}", addr_type));
            }
        };

        // Read port
        let mut port_buf = [0u8; 2];
        self.stream.read_exact(&mut port_buf).await?;
        let port = u16::from_be_bytes(port_buf);

        Ok((command, address, port))
    }

    /// Send a SOCKS5 success response to the client.
    pub async fn send_success(&mut self) -> Result<()> {
        self.stream.write_all(&SOCKS5_SUCCESS_RESPONSE).await?;
        Ok(())
    }

    /// Send a SOCKS5 error response to the client.
    pub async fn send_error(&mut self, code: u8) -> Result<()> {
        let response = [
            SOCKS5_VERSION,
            code,
            0x00,                  // Reserved
            SOCKS5_ADDR_TYPE_IPV4, // Address type
            0,
            0,
            0,
            0, // BND.ADDR
            0,
            0, // BND.PORT
        ];
        self.stream.write_all(&response).await?;
        Ok(())
    }

    /// Convenience method: perform full handshake (auth + request parsing).
    /// Returns the parsed target (host + port).
    pub async fn handshake(&mut self) -> Result<(Socks5Address, u16)> {
        self.negotiate_auth().await?;
        let (_command, address, port) = self.read_request().await?;
        Ok((address, port))
    }

    /// Send an auth failure response for non-localhost rejection.
    pub async fn reject_non_localhost(&mut self) -> Result<()> {
        // Read the initial greeting to determine the SOCKS version
        let mut buf = [0u8; 2];
        if self.stream.read_exact(&mut buf).await.is_ok() {
            let version = buf[0];
            if version == SOCKS5_VERSION {
                self.stream
                    .write_all(&[SOCKS5_VERSION, SOCKS5_AUTH_FAILED])
                    .await?;
            } else {
                self.send_error(SOCKS5_FAIL).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socks5_constants() {
        assert_eq!(SOCKS5_VERSION, 0x05);
        assert_eq!(SOCKS5_AUTH_METHOD_NONE, 0x00);
        assert_eq!(SOCKS5_AUTH_FAILED, 0xFF);
        assert_eq!(SOCKS5_CMD_CONNECT, 0x01);
        assert_eq!(SOCKS5_ADDR_TYPE_IPV4, 0x01);
        assert_eq!(SOCKS5_ATYP_DOMAIN, 0x03);
        assert_eq!(SOCKS5_ATYP_IPV6, 0x04);
    }

    #[test]
    fn test_socks5_success_response() {
        assert_eq!(SOCKS5_SUCCESS_RESPONSE.len(), 10);
        assert_eq!(SOCKS5_SUCCESS_RESPONSE[0], 0x05); // SOCKS version 5
        assert_eq!(SOCKS5_SUCCESS_RESPONSE[1], 0x00); // Success
        assert_eq!(SOCKS5_SUCCESS_RESPONSE[2], 0x00); // Reserved
        assert_eq!(SOCKS5_SUCCESS_RESPONSE[3], 0x01); // IPv4
        assert_eq!(&SOCKS5_SUCCESS_RESPONSE[4..8], &[0x00, 0x00, 0x00, 0x00]);
        assert_eq!(&SOCKS5_SUCCESS_RESPONSE[8..10], &[0x00, 0x00]);
    }

    #[test]
    fn test_socks5_address_as_str() {
        let ipv4 = Socks5Address::IPv4("127.0.0.1".to_string());
        assert_eq!(ipv4.as_str(), "127.0.0.1");

        let domain = Socks5Address::Domain("example.com".to_string());
        assert_eq!(domain.as_str(), "example.com");

        let ipv6 = Socks5Address::IPv6("::1".to_string());
        assert_eq!(ipv6.as_str(), "::1");
    }

    #[tokio::test]
    async fn test_handshake_auth_negotiation() {
        // Simulate a SOCKS5 client: send greeting, receive auth response
        let (client, server) = tokio::io::duplex(1024);

        let server_task = tokio::spawn(async move {
            let mut hs = Socks5Handshake::new(server);
            hs.negotiate_auth().await
        });

        let client_task = tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(client);
            // Send greeting: version=5, 1 method, method=0 (no auth)
            writer.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
            // Read response
            let mut buf = [0u8; 2];
            tokio::io::AsyncReadExt::read_exact(&mut reader, &mut buf)
                .await
                .unwrap();
            buf
        });

        let response = client_task.await.unwrap();
        assert_eq!(response, [0x05, 0x00]); // version 5, no auth
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_handshake_invalid_version() {
        let (client, server) = tokio::io::duplex(1024);

        let server_task = tokio::spawn(async move {
            let mut hs = Socks5Handshake::new(server);
            hs.negotiate_auth().await
        });

        let client_task = tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(client);
            // Send invalid version
            writer.write_all(&[0x04, 0x01, 0x00]).await.unwrap();
            // Read response (auth failed)
            let mut buf = [0u8; 2];
            tokio::io::AsyncReadExt::read_exact(&mut reader, &mut buf)
                .await
                .unwrap();
            buf
        });

        let response = client_task.await.unwrap();
        assert_eq!(response, [0x05, 0xFF]); // version 5, auth failed
        assert!(server_task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn test_full_handshake_connect_domain() {
        let (client, server) = tokio::io::duplex(1024);

        let server_task = tokio::spawn(async move {
            let mut hs = Socks5Handshake::new(server);
            hs.handshake().await
        });

        let client_task = tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(client);
            // Greeting: version=5, 1 method, no auth
            writer.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
            // Read auth response
            let mut buf = [0u8; 2];
            tokio::io::AsyncReadExt::read_exact(&mut reader, &mut buf)
                .await
                .unwrap();
            assert_eq!(buf, [0x05, 0x00]);

            // Request: CONNECT to example.com:80
            let domain = b"example.com";
            let mut request = vec![0x05, 0x01, 0x00, 0x03]; // ver, cmd=connect, rsv, atyp=domain
            request.push(domain.len() as u8);
            request.extend_from_slice(domain);
            request.extend_from_slice(&80u16.to_be_bytes());
            writer.write_all(&request).await.unwrap();
        });

        client_task.await.unwrap();
        let (address, port) = server_task.await.unwrap().unwrap();
        assert_eq!(address, Socks5Address::Domain("example.com".to_string()));
        assert_eq!(port, 80);
    }

    #[tokio::test]
    async fn test_full_handshake_connect_ipv4() {
        let (client, server) = tokio::io::duplex(1024);

        let server_task = tokio::spawn(async move {
            let mut hs = Socks5Handshake::new(server);
            hs.handshake().await
        });

        let client_task = tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(client);
            // Greeting
            writer.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
            let mut buf = [0u8; 2];
            tokio::io::AsyncReadExt::read_exact(&mut reader, &mut buf)
                .await
                .unwrap();

            // Request: CONNECT to 192.168.1.1:443
            let request = [
                0x05, 0x01, 0x00, 0x01, // ver, cmd=connect, rsv, atyp=ipv4
                192, 168, 1, 1, // address
                0x01, 0xBB, // port 443
            ];
            writer.write_all(&request).await.unwrap();
        });

        client_task.await.unwrap();
        let (address, port) = server_task.await.unwrap().unwrap();
        assert_eq!(address, Socks5Address::IPv4("192.168.1.1".to_string()));
        assert_eq!(port, 443);
    }
}
