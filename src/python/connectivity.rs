use crate::router_helpers::get_relay_access_creds;
use log::{debug, info, warn};
use serde_json::json;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Internal implementation of WebRTC connectivity test
/// Performs comprehensive diagnostics similar to turnutils but for IT personnel
pub async fn test_webrtc_connectivity_internal(
    krelay_server: &str,
    settings: HashMap<String, serde_json::Value>,
    timeout_seconds: u64,
    ksm_config: Option<&str>,
    client_version: Option<&str>,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<HashMap<String, serde_json::Value>, String> {
    let start_time = Instant::now();
    let mut results = HashMap::new();

    info!(
        "Starting WebRTC connectivity test for server: {}",
        krelay_server
    );

    // Basic test information
    results.insert("server".to_string(), json!(krelay_server));
    results.insert(
        "test_started_at".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    results.insert("timeout_seconds".to_string(), json!(timeout_seconds));
    results.insert(
        "settings".to_string(),
        serde_json::to_value(&settings).unwrap_or(json!({})),
    );

    // Step 1: DNS Resolution
    info!("Step 1: Testing DNS resolution for {}", krelay_server);
    let dns_start = Instant::now();
    match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host((krelay_server, 3478)),
    )
    .await
    {
        Ok(Ok(addresses)) => {
            let addrs: Vec<String> = addresses.map(|addr| addr.ip().to_string()).collect();
            results.insert("dns_resolution".to_string(), json!({
                "success": true,
                "duration_ms": dns_start.elapsed().as_millis(),
                "resolved_ips": addrs,
                "message": format!("Successfully resolved {} to {} IP addresses", krelay_server, addrs.len())
            }));
            addrs
        }
        Ok(Err(e)) => {
            results.insert(
                "dns_resolution".to_string(),
                json!({
                    "success": false,
                    "duration_ms": dns_start.elapsed().as_millis(),
                    "error": e.to_string(),
                    "message": format!("DNS resolution failed for {}: {}", krelay_server, e),
                    "it_diagnosis": "DNS resolution failure indicates either the hostname is incorrect, DNS servers are unreachable, or network connectivity is blocked",
                    "suggested_tests": [
                        format!("nslookup {krelay_server}"),
                        format!("dig {krelay_server}")
                    ]
                }),
            );
            return Ok(results);
        }
        Err(_) => {
            results.insert(
                "dns_resolution".to_string(),
                json!({
                    "success": false,
                    "duration_ms": dns_start.elapsed().as_millis(),
                    "error": "DNS resolution timeout",
                    "message": format!("DNS resolution timed out for {}", krelay_server),
                    "it_diagnosis": "DNS timeout suggests network connectivity issues, DNS server problems, or restrictive firewall rules blocking DNS queries",
                    "suggested_tests": [
                        "ping 8.8.8.8  # Test basic internet connectivity",
                        "nslookup google.com  # Test if DNS is working at all",
                        format!("telnet {} 53  # Test if DNS port is accessible", krelay_server),
                        "Check firewall rules for outbound UDP/53 and TCP/53",
                        "Verify corporate proxy/DNS filtering settings"
                    ]
                }),
            );
            return Ok(results);
        }
    };

    // Step 2: AWS Infrastructure connectivity test
    info!("Step 2: Testing AWS infrastructure connectivity");
    let aws_start = Instant::now();
    let mut aws_issues = Vec::new();
    let mut aws_success = true;

    // Test connectivity to common AWS endpoints that might be involved in load balancing
    let aws_endpoints = vec![("amazonaws.com", 443), ("aws.amazon.com", 443)];

    let mut aws_results = Vec::new();
    for (endpoint, port) in aws_endpoints {
        match tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect((endpoint, port)),
        )
        .await
        {
            Ok(Ok(stream)) => {
                drop(stream);
                aws_results.push(json!({
                    "endpoint": format!("{}:{}", endpoint, port),
                    "success": true,
                    "message": format!("Successfully connected to {endpoint}")
                }));
            }
            Ok(Err(e)) => {
                aws_success = false;
                aws_issues.push(format!("Failed to connect to {endpoint}: {e}"));
                aws_results.push(json!({
                    "endpoint": format!("{endpoint}:{port}"),
                    "success": false,
                    "error": e.to_string()
                }));
            }
            Err(_) => {
                aws_success = false;
                aws_issues.push(format!("Connection timeout to {endpoint}"));
                aws_results.push(json!({
                    "endpoint": format!("{endpoint}:{port}"),
                    "success": false,
                    "error": "Connection timeout"
                }));
            }
        }
    }

    // AWS connectivity is informational only - don't fail the test if it's not available
    let aws_message = if aws_success {
        "AWS infrastructure endpoints are accessible".to_string()
    } else {
        format!(
            "AWS connectivity not available (may not be required): {}",
            aws_issues.join("; ")
        )
    };

    results.insert(
        "aws_connectivity".to_string(),
        json!({
            "success": true, // Always succeed - this is informational only
            "duration_ms": aws_start.elapsed().as_millis(),
            "endpoints_tested": aws_results,
            "issues": aws_issues,
            "warning": !aws_success,
            "message": aws_message
        }),
    );

    // Step 3: Basic TCP connectivity test (port 3478)
    info!("Step 3: Testing TCP connectivity to {}:3478", krelay_server);
    let tcp_start = Instant::now();
    match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect((krelay_server, 3478)),
    )
    .await
    {
        Ok(Ok(stream)) => {
            drop(stream);
            results.insert(
                "tcp_connectivity".to_string(),
                json!({
                    "success": true,
                    "duration_ms": tcp_start.elapsed().as_millis(),
                    "port": 3478,
                    "message": format!("Successfully connected to {}:3478 via TCP", krelay_server)
                }),
            );
            true
        }
        Ok(Err(e)) => {
            results.insert(
                "tcp_connectivity".to_string(),
                json!({
                    "success": false,
                    "duration_ms": tcp_start.elapsed().as_millis(),
                    "port": 3478,
                    "error": e.to_string(),
                    "message": format!("TCP connection failed to {}:3478: {}", krelay_server, e),
                    "it_diagnosis": "TCP connection failure indicates firewall blocking, incorrect routing, or server unavailability",
                    "suggested_tests": [
                        format!("telnet {} 3478  # Test direct TCP connection", krelay_server),
                        format!("nc -v {} 3478  # Alternative TCP connection test", krelay_server),
                        "Check firewall rules for outbound TCP/3478",
                        "Verify corporate firewall/proxy settings",
                        "Test from different network if possible"
                    ]
                }),
            );
            false
        }
        Err(_) => {
            results.insert(
                "tcp_connectivity".to_string(),
                json!({
                    "success": false,
                    "duration_ms": tcp_start.elapsed().as_millis(),
                    "port": 3478,
                    "error": "Connection timeout",
                    "message": format!("TCP connection timed out to {}:3478", krelay_server),
                    "it_diagnosis": "TCP timeout typically indicates firewall blocking, network routing issues, or server overload",
                    "suggested_tests": [
                        format!("ping {}  # Test if server is reachable", krelay_server),
                        format!("traceroute {}  # Check network path", krelay_server),
                        format!("nmap -p 3478 {}  # Check if port is open", krelay_server),
                        "Check corporate firewall for TCP/3478 restrictions",
                        "Verify no local firewall software is blocking"
                    ]
                }),
            );
            false
        }
    };

    // Step 4: UDP socket binding test (to ensure we can send UDP)
    info!("Step 4: Testing UDP socket binding");
    let udp_start = Instant::now();
    match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
        Ok(socket) => {
            let local_addr = socket
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or("unknown".to_string());
            results.insert(
                "udp_binding".to_string(),
                json!({
                    "success": true,
                    "duration_ms": udp_start.elapsed().as_millis(),
                    "local_address": local_addr,
                    "message": "Successfully bound UDP socket for STUN/TURN communication"
                }),
            );
            true
        }
        Err(e) => {
            results.insert(
                "udp_binding".to_string(),
                json!({
                    "success": false,
                    "duration_ms": udp_start.elapsed().as_millis(),
                    "error": e.to_string(),
                    "message": format!("Failed to bind UDP socket: {e}"),
                    "it_diagnosis": "UDP socket binding failure indicates local network restrictions or system resource limits",
                    "suggested_tests": [
                        "netstat -ul  # Check UDP ports in use",
                        "ss -ul  # Alternative UDP port listing",
                        "Check local firewall software settings",
                        "Verify system ulimits for socket creation",
                        "Test with admin/root privileges if permitted"
                    ]
                }),
            );
            false
        }
    };

    // Step 5: WebRTC ICE configuration validation
    info!("Step 5: Testing WebRTC ICE configuration");
    let ice_start = Instant::now();

    // Extract settings for ICE server configuration
    let use_turn = settings
        .get("use_turn")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let turn_only = settings
        .get("turn_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Build ICE server configuration (similar to tube_registry.rs:330-420)
    let mut ice_urls = Vec::new();

    if !turn_only {
        ice_urls.push(format!("stun:{krelay_server}:3478"));
    }

    // Add TURN servers if configured
    if use_turn {
        ice_urls.push(format!("turn:{krelay_server}:3478"));
        ice_urls.push(format!("turns:{krelay_server}:5349"));
    }

    results.insert(
        "ice_configuration".to_string(),
        json!({
            "success": true,
            "duration_ms": ice_start.elapsed().as_millis(),
            "use_turn": use_turn,
            "turn_only": turn_only,
            "ice_servers": ice_urls,
            "server_count": ice_urls.len(),
            "message": format!("Generated ICE configuration with {} servers", ice_urls.len())
        }),
    );

    // Step 6: Simple WebRTC Peer Connection creation test
    info!("Step 6: Testing WebRTC peer connection creation");
    let webrtc_start = Instant::now();

    // Build the RTCConfiguration
    use webrtc::ice_transport::ice_gathering_state::RTCIceGatheringState;
    use webrtc::ice_transport::ice_server::RTCIceServer;
    use webrtc::peer_connection::configuration::RTCConfiguration;

    let mut webrtc_ice_servers = Vec::new();

    if !turn_only {
        webrtc_ice_servers.push(RTCIceServer {
            urls: vec![format!("stun:{}:3478", krelay_server)],
            ..Default::default()
        });
    }

    // Track whether we're using real credentials (either from ksm_config or passed parameters)
    let mut using_real_credentials = false;

    // Add TURN servers if configured
    if use_turn {
        // Priority 1: Try to get credentials from ksm_config if provided
        if let (Some(ksm_cfg), Some(client_ver)) = (ksm_config, client_version) {
            if !ksm_cfg.is_empty() && !ksm_cfg.starts_with("TEST_MODE_KSM_CONFIG") {
                debug!("Fetching TURN credentials from KSM router with 1h TTL");
                // Request 1-hour TTL for connectivity testing (matches production)
                match get_relay_access_creds(ksm_cfg, Some(3600), client_ver).await {
                    Ok(creds) => {
                        debug!("Successfully fetched TURN credentials from router");

                        // Log TTL for diagnostics
                        if let Some(ttl) = creds.get("ttl").and_then(|v| v.as_u64()) {
                            debug!(
                                "Router returned TURN TTL: {}s ({:.1}h)",
                                ttl,
                                ttl as f64 / 3600.0
                            );
                        } else {
                            debug!("No TTL in router response");
                        }

                        // Extract username and password from credentials
                        if let (Some(router_username), Some(router_password)) = (
                            creds.get("username").and_then(|v| v.as_str()),
                            creds.get("password").and_then(|v| v.as_str()),
                        ) {
                            debug!("Using router TURN credentials for test");
                            using_real_credentials = true;
                            webrtc_ice_servers.push(RTCIceServer {
                                urls: vec![format!("turn:{}:3478", krelay_server)],
                                username: router_username.to_string(),
                                credential: router_password.to_string(),
                            });
                        } else {
                            warn!("Invalid router credentials format, checking for passed credentials");
                            // Fall through to check passed credentials
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to get router credentials: {}, checking for passed credentials",
                            e
                        );
                        // Fall through to check passed credentials
                    }
                }
            }
        }

        // Priority 2: Use passed username/password if we don't have router credentials
        if !using_real_credentials {
            if let (Some(user), Some(pass)) = (username, password) {
                debug!("Using passed TURN credentials for test");
                using_real_credentials = true;
                webrtc_ice_servers.push(RTCIceServer {
                    urls: vec![format!("turn:{}:3478", krelay_server)],
                    username: user.to_string(),
                    credential: pass.to_string(),
                });
            } else {
                return Err("TURN server testing requires either ksm_config with client_version or username/password parameters".to_string());
            }
        }
    }

    let config = RTCConfiguration {
        ice_servers: webrtc_ice_servers,
        ice_transport_policy: if turn_only {
            webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::Relay
        } else {
            webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy::All
        },
        ..Default::default()
    };

    // Create isolated WebRTC API for connectivity test
    let test_api = crate::webrtc_core::IsolatedWebRTCAPI::new("connectivity-test".to_string());
    match crate::webrtc_core::create_peer_connection_isolated(&test_api, Some(config)).await {
        Ok(pc) => {
            let mut webrtc_success = true;
            let webrtc_message: String;
            let mut webrtc_details = serde_json::Map::new();

            // Test creating a data channel
            match crate::webrtc_core::create_data_channel(&pc, "test-connectivity").await {
                Ok(_dc) => {
                    webrtc_details.insert("data_channel_created".to_string(), json!(true));

                    // Create an offer to trigger ICE candidate gathering
                    match pc.create_offer(None).await {
                        Ok(offer) => {
                            match pc.set_local_description(offer).await {
                                Ok(_) => {
                                    debug!(
                                        "Set local description, starting ICE candidate gathering"
                                    );
                                    webrtc_details.insert("offer_created".to_string(), json!(true));

                                    // Wait for ICE gathering to complete or timeout
                                    let ice_timeout = Duration::from_secs(10);
                                    let ice_gathering_start = Instant::now();

                                    let gathering_result =
                                        tokio::time::timeout(ice_timeout, async {
                                            let mut last_state = pc.ice_gathering_state();
                                            debug!("Initial ICE gathering state: {:?}", last_state);
                                            while last_state != RTCIceGatheringState::Complete {
                                                tokio::time::sleep(Duration::from_millis(100))
                                                    .await;
                                                let current_state = pc.ice_gathering_state();
                                                if current_state != last_state {
                                                    debug!(
                                                        "ICE gathering state changed: {:?} -> {:?}",
                                                        last_state, current_state
                                                    );
                                                    last_state = current_state;
                                                }
                                            }
                                            debug!("ICE gathering completed");
                                        })
                                        .await;

                                    let ice_gathering_duration = ice_gathering_start.elapsed();
                                    webrtc_details.insert(
                                        "ice_gathering_duration_ms".to_string(),
                                        json!(ice_gathering_duration.as_millis()),
                                    );

                                    match gathering_result {
                                        Ok(_) => {
                                            debug!("ICE gathering completed successfully");
                                            webrtc_details.insert(
                                                "ice_gathering_completed".to_string(),
                                                json!(true),
                                            );

                                            // Analyze gathered ICE candidates
                                            if let Some(local_desc) = pc.local_description().await {
                                                let sdp_text = local_desc.sdp;
                                                let candidate_analysis = analyze_ice_candidates(
                                                    &sdp_text,
                                                    using_real_credentials,
                                                );

                                                // Add the analysis to results
                                                for (key, value) in candidate_analysis {
                                                    webrtc_details.insert(key, value);
                                                }

                                                // Determine overall success based on candidate analysis
                                                if use_turn && using_real_credentials {
                                                    // If we're testing TURN with real credentials, we should get relay candidates
                                                    let relay_candidates = webrtc_details
                                                        .get("relay_candidates_count")
                                                        .and_then(|v| v.as_u64())
                                                        .unwrap_or(0);

                                                    if relay_candidates > 0 {
                                                        webrtc_message = format!("WebRTC peer connection successful with {relay_candidates} TURN relay candidates gathered");
                                                    } else {
                                                        webrtc_success = false;
                                                        webrtc_message = "TURN server configured but no relay candidates were gathered - TURN server may not be working".to_string();
                                                    }
                                                } else {
                                                    // For STUN-only or no-TURN tests, just check if we got any candidates
                                                    let total_candidates = webrtc_details
                                                        .get("total_candidates_count")
                                                        .and_then(|v| v.as_u64())
                                                        .unwrap_or(0);

                                                    if total_candidates > 0 {
                                                        webrtc_message = format!("WebRTC peer connection successful with {total_candidates} ICE candidates gathered");
                                                    } else {
                                                        webrtc_success = false;
                                                        webrtc_message = "No ICE candidates were gathered - network connectivity issues".to_string();
                                                    }
                                                }
                                            } else {
                                                webrtc_success = false;
                                                webrtc_message = "ICE gathering completed but no local description available".to_string();
                                            }
                                        }
                                        Err(_) => {
                                            warn!(
                                                "ICE gathering timed out after {}ms",
                                                ice_timeout.as_millis()
                                            );
                                            webrtc_details.insert(
                                                "ice_gathering_completed".to_string(),
                                                json!(false),
                                            );
                                            webrtc_details.insert(
                                                "ice_gathering_timeout".to_string(),
                                                json!(true),
                                            );

                                            // Even if it timed out, analyze what we got
                                            if let Some(local_desc) = pc.local_description().await {
                                                let sdp_text = local_desc.sdp;
                                                let candidate_analysis = analyze_ice_candidates(
                                                    &sdp_text,
                                                    using_real_credentials,
                                                );

                                                for (key, value) in candidate_analysis {
                                                    webrtc_details.insert(key, value);
                                                }

                                                let total_candidates = webrtc_details
                                                    .get("total_candidates_count")
                                                    .and_then(|v| v.as_u64())
                                                    .unwrap_or(0);

                                                if total_candidates > 0 {
                                                    webrtc_message = format!("ICE gathering timed out but {total_candidates} candidates were collected");
                                                } else {
                                                    webrtc_success = false;
                                                    webrtc_message = "ICE gathering timed out with no candidates collected".to_string();
                                                }
                                            } else {
                                                webrtc_success = false;
                                                webrtc_message = "ICE gathering timed out and no local description available".to_string();
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    webrtc_success = false;
                                    webrtc_message =
                                        format!("Failed to set local description: {e}");
                                    webrtc_details.insert(
                                        "set_local_description_error".to_string(),
                                        json!(e.to_string()),
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            webrtc_success = false;
                            webrtc_message = format!("Failed to create offer: {e}");
                            webrtc_details
                                .insert("create_offer_error".to_string(), json!(e.to_string()));
                        }
                    }
                }
                Err(e) => {
                    webrtc_success = false;
                    webrtc_message =
                        format!("Peer connection created but data channel failed: {e}");
                    webrtc_details.insert("data_channel_created".to_string(), json!(false));
                    webrtc_details.insert("data_channel_error".to_string(), json!(e.to_string()));
                }
            }

            // Add final connection states
            webrtc_details.insert(
                "final_ice_gathering_state".to_string(),
                json!(format!("{:?}", pc.ice_gathering_state())),
            );
            webrtc_details.insert(
                "final_connection_state".to_string(),
                json!(format!("{:?}", pc.connection_state())),
            );

            results.insert(
                "webrtc_peer_connection".to_string(),
                json!({
                    "success": webrtc_success,
                    "duration_ms": webrtc_start.elapsed().as_millis(),
                    "message": webrtc_message,
                    "details": webrtc_details
                }),
            );

            // Close the peer connection
            let _ = pc.close().await;
        }
        Err(e) => {
            results.insert(
                "webrtc_peer_connection".to_string(),
                json!({
                    "success": false,
                    "duration_ms": webrtc_start.elapsed().as_millis(),
                    "error": e.to_string(),
                    "message": format!("Failed to create WebRTC peer connection: {}", e)
                }),
            );
        }
    }

    // Overall test summary and comprehensive results
    generate_test_summary(
        results,
        start_time,
        krelay_server,
        settings,
        use_turn,
        using_real_credentials,
        ksm_config,
        client_version,
        username,
        password,
    )
    .await
}

/// Generate comprehensive test summary and recommendations
#[allow(clippy::too_many_arguments)]
async fn generate_test_summary(
    mut results: HashMap<String, serde_json::Value>,
    start_time: Instant,
    krelay_server: &str,
    _settings: HashMap<String, serde_json::Value>,
    use_turn: bool,
    using_real_credentials: bool,
    ksm_config: Option<&str>,
    client_version: Option<&str>,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<HashMap<String, serde_json::Value>, String> {
    let total_duration = start_time.elapsed();
    let mut overall_success = true;
    let mut failed_tests = Vec::new();

    // Check each test result
    for (test_name, test_result) in &results {
        if let Some(success) = test_result.get("success").and_then(|v| v.as_bool()) {
            if !success {
                overall_success = false;
                failed_tests.push(test_name.clone());
            }
        }
    }

    results.insert("overall_result".to_string(), json!({
        "success": overall_success,
        "total_duration_ms": total_duration.as_millis(),
        "tests_run": results.len() - 1, // -1 to exclude this summary
        "failed_tests": failed_tests,
        "message": if overall_success {
            format!("All connectivity tests passed in {}ms", total_duration.as_millis())
        } else {
            format!("Connectivity test completed with {} failures in {}ms", failed_tests.len(), total_duration.as_millis())
        }
    }));

    // Add comprehensive IT-friendly recommendations and diagnostics
    generate_recommendations(
        &mut results,
        krelay_server,
        use_turn,
        using_real_credentials,
    )
    .await;

    // Add suggested command line tests
    generate_cli_tests(
        &mut results,
        krelay_server,
        use_turn,
        ksm_config,
        client_version,
        username,
        password,
    )
    .await;

    info!(
        "WebRTC connectivity test completed in {}ms - Success: {}",
        total_duration.as_millis(),
        overall_success
    );

    Ok(results)
}

/// Generate IT-friendly recommendations based on test results
async fn generate_recommendations(
    results: &mut HashMap<String, serde_json::Value>,
    krelay_server: &str,
    _use_turn: bool,
    _using_real_credentials: bool,
) {
    let mut recommendations: Vec<String> = Vec::new();
    let mut advanced_diagnostics = serde_json::Map::new();

    // DNS troubleshooting
    if !results
        .get("dns_resolution")
        .unwrap()
        .get("success")
        .unwrap()
        .as_bool()
        .unwrap()
    {
        recommendations
            .push("CRITICAL: DNS resolution failed - this blocks all connectivity".to_string());
        recommendations.push("Verify DNS servers are configured and accessible".to_string());
        recommendations
            .push("Check if corporate DNS filtering is blocking the hostname".to_string());

        advanced_diagnostics.insert(
            "dns_troubleshooting".to_string(),
            json!({
                "priority": "critical",
                "commands": [
                    format!("nslookup {krelay_server}"),
                    format!("dig {krelay_server}"),
                    format!("dig @8.8.8.8 {krelay_server}  # Try Google DNS"),
                    format!("dig @1.1.1.1 {krelay_server}  # Try Cloudflare DNS"),
                    "cat /etc/resolv.conf  # Check DNS config (Linux)",
                    "scutil --dns  # Check DNS config (macOS)",
                    "ipconfig /all  # Check DNS config (Windows)"
                ],
                "network_tests": [
                    "ping 8.8.8.8  # Test basic internet",
                    "ping 1.1.1.1  # Test alternate DNS",
                    "nslookup google.com  # Test if DNS works for known domains"
                ]
            }),
        );
    }

    // Add other troubleshooting sections...
    if recommendations.is_empty() {
        recommendations.push(
            "SUCCESS: All connectivity tests passed - WebRTC should function properly".to_string(),
        );
        recommendations
            .push("Network configuration appears optimal for real-time communication".to_string());
    }

    results.insert("recommendations".to_string(), json!(recommendations));
    results.insert(
        "advanced_diagnostics".to_string(),
        json!(advanced_diagnostics),
    );
}

/// Generate suggested CLI tests for manual verification
async fn generate_cli_tests(
    results: &mut HashMap<String, serde_json::Value>,
    krelay_server: &str,
    use_turn: bool,
    ksm_config: Option<&str>,
    client_version: Option<&str>,
    username: Option<&str>,
    password: Option<&str>,
) {
    let mut cli_tests = Vec::new();

    // Basic connectivity tests
    cli_tests.push("# Basic connectivity tests:".to_string());
    cli_tests.push(format!("ping {krelay_server}  # Test basic reachability"));
    cli_tests.push(format!(
        "telnet {krelay_server} 3478  # Test TCP connectivity"
    ));
    cli_tests.push(format!(
        "nc -u {krelay_server} 3478 < /dev/null  # Test UDP connectivity"
    ));

    // STUN testing (works without credentials)
    cli_tests.push("".to_string());
    cli_tests.push("# STUN server testing (no credentials required):".to_string());
    cli_tests.push(format!(
        "turnutils_stunclient {krelay_server}  # Test STUN server"
    ));
    cli_tests.push("turnutils_natdiscovery  # Discover NAT type and behavior".to_string());

    if use_turn {
        cli_tests.push("".to_string());
        cli_tests.push("# TURN server testing (requires credentials):".to_string());

        // Try to use actual credentials if available
        if let (Some(ksm_cfg), Some(client_ver)) = (ksm_config, client_version) {
            if !ksm_cfg.is_empty() && !ksm_cfg.starts_with("TEST_MODE_KSM_CONFIG") {
                match get_relay_access_creds(ksm_cfg, None, client_ver).await {
                    Ok(creds) => {
                        if let (Some(username), Some(password)) = (
                            creds.get("username").and_then(|v| v.as_str()),
                            creds.get("password").and_then(|v| v.as_str()),
                        ) {
                            cli_tests.push(format!(
                                "turnutils_uclient -t -u {username} -w {password} {krelay_server}  # Test TURN with router credentials"
                            ));
                        }
                    }
                    Err(_) => {
                        cli_tests.push(format!("turnutils_uclient -t -u USERNAME -w PASSWORD {krelay_server}  # Replace with actual credentials"));
                    }
                }
            }
        } else if let (Some(user), Some(pass)) = (username, password) {
            // Use passed credentials for CLI test
            cli_tests.push(format!(
                "turnutils_uclient -t -u {user} -w {pass} {krelay_server}"
            ));
        }
    }

    results.insert("suggested_cli_tests".to_string(), json!(cli_tests));
}

/// Analyze ICE candidates from SDP to determine TURN server functionality
pub fn analyze_ice_candidates(
    sdp: &str,
    using_real_credentials: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let mut analysis = serde_json::Map::new();

    let mut host_candidates = 0;
    let mut srflx_candidates = 0; // Server reflexive (STUN)
    let mut relay_candidates = 0; // TURN relay
    let mut total_candidates = 0;

    let mut candidate_details = Vec::new();

    debug!("Analyzing SDP for ICE candidates");

    // Parse SDP for a= lines containing candidates
    for line in sdp.lines() {
        if line.starts_with("a=candidate:") {
            total_candidates += 1;

            // Parse the candidate line: a=candidate:foundation component transport priority ip port typ candidate-type
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 8 {
                let candidate_type = parts[7]; // typ value
                let ip = parts[4];
                let port = parts[5];
                let transport = parts[2];

                match candidate_type {
                    "host" => {
                        host_candidates += 1;
                        candidate_details.push(json!({
                            "type": "host",
                            "ip": ip,
                            "port": port,
                            "transport": transport,
                            "description": "Local network interface"
                        }));
                    }
                    "srflx" => {
                        srflx_candidates += 1;
                        candidate_details.push(json!({
                            "type": "srflx",
                            "ip": ip,
                            "port": port,
                            "transport": transport,
                            "description": "STUN server reflexive candidate"
                        }));
                    }
                    "relay" => {
                        relay_candidates += 1;
                        candidate_details.push(json!({
                            "type": "relay",
                            "ip": ip,
                            "port": port,
                            "transport": transport,
                            "description": "TURN relay candidate"
                        }));
                    }
                    _ => {
                        // Other types like prflx (peer reflexive)
                        candidate_details.push(json!({
                            "type": candidate_type,
                            "ip": ip,
                            "port": port,
                            "transport": transport,
                            "description": format!("Other candidate type: {}", candidate_type)
                        }));
                    }
                }
            }
        }
    }

    debug!(
        "ICE candidate analysis: total={}, host={}, srflx={}, relay={}",
        total_candidates, host_candidates, srflx_candidates, relay_candidates
    );

    // Add counts to analysis
    analysis.insert(
        "total_candidates_count".to_string(),
        json!(total_candidates),
    );
    analysis.insert("host_candidates_count".to_string(), json!(host_candidates));
    analysis.insert(
        "srflx_candidates_count".to_string(),
        json!(srflx_candidates),
    );
    analysis.insert(
        "relay_candidates_count".to_string(),
        json!(relay_candidates),
    );
    analysis.insert("candidate_details".to_string(), json!(candidate_details));

    // Analysis and recommendations
    let mut candidate_analysis = Vec::new();

    if host_candidates > 0 {
        candidate_analysis.push("Local network interfaces are accessible".to_string());
    } else {
        candidate_analysis
            .push("No local network candidates - unusual network configuration".to_string());
    }

    if srflx_candidates > 0 {
        candidate_analysis.push(format!(
            "STUN server is working - {srflx_candidates} reflexive candidates gathered"
        ));
    } else {
        candidate_analysis
            .push("No STUN reflexive candidates - STUN server may not be accessible".to_string());
    }

    if using_real_credentials {
        if relay_candidates > 0 {
            candidate_analysis.push(format!(
                "TURN server is working - {relay_candidates} relay candidates gathered with real credentials"
            ));
        } else {
            candidate_analysis.push(
                "TURN server not working - no relay candidates despite valid credentials"
                    .to_string(),
            );
        }
    } else if relay_candidates > 0 {
        candidate_analysis.push(format!(
                "TURN server appears accessible - {relay_candidates} relay candidates (but credentials not tested)"
        ));
    } else {
        candidate_analysis
            .push("TURN relay functionality not tested (no credentials provided)".to_string());
    }

    analysis.insert("candidate_analysis".to_string(), json!(candidate_analysis));

    // Overall assessment
    let overall_assessment = if total_candidates == 0 {
        "Critical: No ICE candidates gathered - severe network connectivity issues".to_string()
    } else if using_real_credentials && relay_candidates == 0 {
        "TURN server not functioning despite valid credentials".to_string()
    } else if relay_candidates > 0 {
        "Network connectivity appears good with relay capabilities".to_string()
    } else if srflx_candidates > 0 {
        "Basic connectivity working via STUN (NAT traversal possible)".to_string()
    } else if host_candidates > 0 {
        "Only local candidates - may have issues with NAT traversal".to_string()
    } else {
        "Unknown connectivity state".to_string()
    };

    analysis.insert("overall_assessment".to_string(), json!(overall_assessment));

    analysis
}
