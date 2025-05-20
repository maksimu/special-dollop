use base64::{engine::general_purpose::STANDARD as BASE64, engine::general_purpose::URL_SAFE as URL_SAFE_BASE64, Engine as _};

use reqwest::{self};
use serde::{Deserialize, Serialize};
use std::env;
use p256::ecdsa::{SigningKey, Signature, signature::Signer};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::error::Error;
use std::time::Duration;
use anyhow::anyhow;

// Custom error type to replace KRouterException
#[derive(Debug)]
struct KRouterError(String);

impl std::fmt::Display for KRouterError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "KRouter Error: {}", self.0)
    }
}

impl Error for KRouterError {}

// Struct for KSM config
#[derive(Deserialize, Serialize)]
struct KsmConfig {
    #[serde(rename = "clientId")]
    client_id: String,
    hostname: String,
    // Add other fields as needed
}

// Define a struct for the body of post_connection_state
#[derive(Serialize)]
struct ConnectionStateBody {
    #[serde(rename = "type")]
    connection_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminated: Option<bool>,
}

// Constants
const VERIFY_SSL: bool = true;

// Uses the name and version from Cargo.toml at compile time
const KEEPER_CLIENT: &str = concat!(env!("CARGO_PKG_NAME"), ":", env!("CARGO_PKG_VERSION")); 

const KEY_PRIVATE_KEY: &str = "privateKey";
const KEY_CLIENT_ID: &str = "clientId";

// Challenge response module - implements caching logic
mod challenge_response {
    use super::*;
    use lazy_static::lazy_static;
    use log::{debug, error};
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use p256::pkcs8::DecodePrivateKey;

    const CHALLENGE_RESPONSE_TIMEOUT_SEC: u64 = 300; // 5 minutes
    const WEBSOCKET_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

    lazy_static! {
        static ref CHALLENGE_DATA: Mutex<ChallengeData> = Mutex::new(ChallengeData {
            challenge_seconds: 0.0,
            challenge: String::new(),
            signature: String::new(),
        });
    }

    struct ChallengeData {
        challenge_seconds: f64,
        challenge: String,
        signature: String,
        
    }

    pub struct ChallengeResponse;

    impl ChallengeResponse {
        pub async fn fetch(ksm_config: &str) -> Result<(String, String), Box<dyn Error>> {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64();

            // Check if we can use the cached challenge
            {
                let data = CHALLENGE_DATA.lock().unwrap();
                let latest_challenge_seconds = now - data.challenge_seconds;

                if latest_challenge_seconds < CHALLENGE_RESPONSE_TIMEOUT_SEC as f64
                    && !data.challenge.is_empty()
                    && !data.signature.is_empty() {
                    debug!("Using Keeper API challenge already received {} seconds ago.",
                          latest_challenge_seconds as u64);
                    return Ok((data.challenge.clone(), data.signature.clone()));
                }
            }

            // Need to fetch a new challenge
            let router_http_host = http_router_url_from_ksm_config(ksm_config)?;
            let url = format!("{}/api/device/get_challenge", router_http_host);

            let client = reqwest::Client::builder()
                .timeout(WEBSOCKET_CONNECTION_TIMEOUT)
                .danger_accept_invalid_certs(!VERIFY_SSL)
                .build()?;

            let response = match client.get(&url).send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        error!(
                            "HTTP error response code ({}) received fetching challenge string from Keeper",
                            status
                        );
                        return Err(Box::new(KRouterError(format!(
                            "HTTP error response code ({})",
                            status
                        ))));
                    }
                    resp
                }
                Err(e) => {
                    error!("HTTP error received fetching challenge string from Keeper: {}", e);
                    return Err(Box::new(e));
                }
            };

            let challenge = response.text().await?;
            let signature = sign_client_id(ksm_config, &challenge)?;

            debug!("Fetched new Keeper API challenge and generated response.");

            // Update the cache
            {
                let mut data = CHALLENGE_DATA.lock().unwrap();
                data.challenge_seconds = now;
                data.challenge = challenge.clone();
                data.signature = signature.clone();
            }

            Ok((challenge, signature))
        }
    }

    // Function to sign client_id with the challenge
    fn sign_client_id(ksm_config: &str, challenge: &str) -> Result<String, Box<dyn Error>> {
        let ksm_config_dict: serde_json::Value = serde_json::from_str(ksm_config)?;

        let private_key_der_str = match ksm_config_dict.get(KEY_PRIVATE_KEY) {
            Some(val) => val.as_str().ok_or("Private key not found or not a string")?,
            None => return Err(Box::new(KRouterError("Private key not found in config".into()))),
        };

        let client_id_str = match ksm_config_dict.get(KEY_CLIENT_ID) {
            Some(val) => val.as_str().ok_or("Client ID not found or not a string")?,
            None => return Err(Box::new(KRouterError("Client ID not found in config".into()))),
        };

        debug!("Decoding client_id and private key");
        
        let private_key_der_bytes = match url_safe_str_to_bytes(private_key_der_str) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to decode private key: {}", e);
                return Err(e);
            }
        };
        
        let mut client_id_bytes = match url_safe_str_to_bytes(client_id_str) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to decode client_id: {}", e);
                return Err(e);
            }
        };

        debug!("Adding challenge to the signature before connecting to the router");
        
        let challenge_bytes = match url_safe_str_to_bytes(challenge) {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to decode challenge: {}", e);
                return Err(e);
            }
        };
        
        client_id_bytes.extend_from_slice(&challenge_bytes);

        let signature = sign_data(&private_key_der_bytes, &client_id_bytes)?;

        Ok(signature)
    }

    // Convert URL safe string to bytes
    fn url_safe_str_to_bytes(s: &str) -> Result<Vec<u8>, Box<dyn Error>> {
        // Add padding if needed
        let padded = if s.len() % 4 == 0 {
            s.to_string()
        } else {
            let padding = "=".repeat(4 - s.len() % 4);
            format!("{}{}", s, padding)
        };
        
        // Try URL safe first, then fall back to the standard
        let bytes = match URL_SAFE_BASE64.decode(padded.as_bytes()) {
            Ok(result) => result,
            Err(_) => {
                // If URL safe fails, try standard Base64
                match BASE64.decode(padded.as_bytes()) {
                    Ok(result) => result,
                    Err(e) => return Err(Box::new(e)),
                }
            }
        };
        
        Ok(bytes)
    }

    // Function to sign data with the private key
    fn sign_data(private_key_der: &[u8], data: &[u8]) -> Result<String, Box<dyn Error>> {
        // Parse the private key from DER
        let signing_key = SigningKey::from_pkcs8_der(private_key_der)?;

        // Sign the data
        let signature: Signature = signing_key.sign(data);

        // Convert to bytes and encode with URL-safe base64 (no padding)
        let sig_bytes = signature.to_der();
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig_bytes);

        Ok(sig_b64)
    }
}

// Function to get router URL from KSM config
pub(crate) fn router_url_from_ksm_config(ksm_config_str: &str) -> anyhow::Result<String> {
    // Special handling for test mode
    if ksm_config_str == "TEST_MODE_KSM_CONFIG" {
        return Ok("test-relay.example.com".to_string());
    }

    // Check environment variable first
    if let Ok(router_hostname_env) = env::var("KPAM_ROUTER_HOST") {
        return Ok(router_hostname_env);
    }

    // Check if config is base64 encoded
    let ksm_config_str = if is_base64(ksm_config_str) {
        // Decode base64 string
        let decoded = BASE64.decode(ksm_config_str)
            .map_err(|e| anyhow!("Failed to decode base64: {}", e))?;
        String::from_utf8(decoded)
            .map_err(|e| anyhow!("Failed to convert to UTF-8: {}", e))?
    } else {
        ksm_config_str.to_string()
    };

    // Parse JSON
    let ksm_config: KsmConfig = serde_json::from_str(&ksm_config_str)
        .map_err(|e| anyhow!("Failed to parse JSON: {}", e))?;
    let mut ka_hostname = ksm_config.hostname;

    // Handle the gov cloud domain
    if ka_hostname.contains("govcloud.") {
        ka_hostname = ka_hostname.replace("govcloud.", "");
    }

    let router_hostname = format!("connect.{}", ka_hostname);
    Ok(router_hostname)
}

// Helper function to check if a string is base64 encoded
fn is_base64(s: &str) -> bool {
    // Check if the string could be base64 encoded (standard or URL-safe)
    // Standard base64 uses A-Z, a-z, 0-9, +, /, and = for padding
    // URL-safe base64 uses A-Z, a-z, 0-9, -, _, and = for padding
    
    // Only check if the length is valid for base64 (multiple of 4 if padding is used)
    if s.len() % 4 != 0 && !s.ends_with('=') {
        return false;
    }
    
    // Check if characters are valid for either standard or URL-safe base64
    s.chars().all(|c| {
        c.is_alphanumeric() || c == '+' || c == '/' || c == '-' || c == '_' || c == '='
    })
}

// Function to get WebSocket router URL
fn ws_router_url_from_ksm_config(ksm_config_str: &str) -> Result<String, Box<dyn Error>> {
    let router_host = router_url_from_ksm_config(ksm_config_str)?;

    if router_host.starts_with("ws") {
        return Ok(router_host);
    }

    let disable_ssl = env::var("KPAM_ROUTER_DISABLE_SSL").is_ok();

    if disable_ssl {
        Ok(format!("ws://{}", router_host))
    } else {
        Ok(format!("wss://{}", router_host))
    }
}

// Function to get HTTP router URL
fn http_router_url_from_ksm_config(ksm_config_str: &str) -> Result<String, Box<dyn Error>> {
    let router_http_host = ws_router_url_from_ksm_config(ksm_config_str)?;
    Ok(router_http_host.replace("ws", "http"))
}

// Main router request function
async fn router_request(
    ksm_config: &str,
    http_method: &str,
    url_path: &str,
    query_params: Option<std::collections::HashMap<String, String>>,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, Box<dyn Error>> {
    // Debug log the request details
    log::debug!("Router request: {} {}, ksm_config={:?}", http_method, url_path, ksm_config);
    
    let router_http_host = http_router_url_from_ksm_config(ksm_config)?;
    let ksm_config_parsed: KsmConfig = serde_json::from_str(ksm_config)?;
    let client_id = &ksm_config_parsed.client_id;

    let url = format!("{}/{}", router_http_host, url_path);

    let (challenge_str, signature) = challenge_response::ChallengeResponse::fetch(ksm_config).await?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .danger_accept_invalid_certs(!VERIFY_SSL)
        .build()?;

    // Create request builder
    let mut request_builder = match http_method {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        _ => return Err(Box::new(KRouterError(format!("Unsupported HTTP method: {}", http_method)))),
    };

    // Add headers
    request_builder = request_builder
        .header("Challenge", challenge_str)
        .header("Signature", signature)
        .header("Authorization", format!("KeeperDevice {}", client_id))
        .header("ClientVersion", KEEPER_CLIENT);

    // Add query parameters if provided
    if let Some(params) = query_params {
        request_builder = request_builder.query(&params);
    }

    // Add body if provided
    if let Some(json_body) = body {
        request_builder = request_builder.json(&json_body);
    }

    // Send the request
    let response = request_builder.send().await?;

    // Check status and handle errors
    if !response.status().is_success() {
        return Err(Box::new(KRouterError(format!(
            "Request failed with status: {}",
            response.status()
        ))));
    }

    // Parse response text
    if response.content_length().unwrap_or(0) > 0 {
        let text = response.text().await?;
        if !text.is_empty() {
            return Ok(serde_json::from_str(&text)?);
        }
    }

    // Return an empty JSON object if no content
    Ok(serde_json::json!({}))
}

// Function to get relay access credentials
pub async fn get_relay_access_creds(
    ksm_config: &str,
    expire_sec: Option<u64>,
) -> Result<serde_json::Value, Box<dyn Error>> {
    // Special handling for test mode
    if ksm_config == "TEST_MODE_KSM_CONFIG" {
        // Return mock credentials for tests
        return Ok(serde_json::json!({
            "username": "test_username",
            "password": "test_password",
            "ttl": 86400
        }));
    }

    let mut query_params = std::collections::HashMap::new();

    if let Some(sec) = expire_sec {
        query_params.insert("expire-sec".to_string(), sec.to_string());
    }

    router_request(
        ksm_config,
        "GET",
        "api/device/relay_access_creds",
        Some(query_params),
        None,
    ).await
}

// Function to post connection state
pub async fn post_connection_state(
    ksm_config: &str,
    connection_state: &str,
    token: &serde_json::Value,
    is_terminated: Option<bool>,
) -> Result<(), Box<dyn Error>> {
    // Special handling for test mode
    if ksm_config == "TEST_MODE_KSM_CONFIG" {
        // Just return OK for tests without making an actual request
        log::debug!("TEST MODE: Skipping post_connection_state for {}", connection_state);
        return Ok(());
    }

    let body = match token {
        serde_json::Value::String(token_str) => {
            ConnectionStateBody {
                connection_type: connection_state.to_string(),
                token: Some(token_str.clone()),
                tokens: None,
                terminated: is_terminated,
            }
        },
        serde_json::Value::Array(token_list) => {
            // Convert the array of values to strings
            let tokens = token_list
                .iter()
                .filter_map(|v| {
                    if let serde_json::Value::String(s) = v {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<String>>();

            ConnectionStateBody {
                connection_type: connection_state.to_string(),
                token: None,
                tokens: Some(tokens),
                terminated: is_terminated,
            }
        },
        _ => {
            return Err(Box::new(KRouterError(format!(
                "Invalid token type: {:?}",
                token
            ))));
        }
    };

    router_request(
        ksm_config,
        "POST",
        "api/device/connect_state",
        None,
        Some(serde_json::to_value(body)?),
    ).await?;

    Ok(())
}
