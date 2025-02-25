use bytes::Bytes;
use lazy_static::lazy_static;
use pyo3::prelude::*;
use pyo3::types::{PyList, PyDict, PyModule};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, Mutex as TokioMutex};
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::signaling_state::RTCSignalingState;

lazy_static! {
    static ref RUNTIME: Arc<Runtime> = Arc::new(Runtime::new().unwrap());
}

#[derive(FromPyObject)]
struct IceServer {
    #[pyo3(item("urls"))]
    urls: Vec<String>,
    #[pyo3(item("username"))]
    username: Option<String>,
    #[pyo3(item("credential"))]
    credential: Option<String>,
}

enum WebRTCEvent {
    IceCandidate(String),
    DataChannel(Arc<RTCDataChannel>),
}

#[pyclass]
struct PyRTCPeerConnection {
    pc: Arc<RTCPeerConnection>,
    _event_task_handle: Option<tokio::task::JoinHandle<()>>,
    trickle_ice: bool,
}

#[pymethods]
impl PyRTCPeerConnection {
    #[new]
    fn new(py: Python<'_>, config: Py<PyDict>, on_ice_candidate: PyObject, on_data_channel: PyObject, trickle_ice: bool) -> PyResult<Self> {
        // Enter the runtime context before doing async operations
        let _guard = RUNTIME.enter();

        let mut rtc_config = RTCConfiguration::default();

        let dict = config.bind(py);
        if let Ok(Some(servers)) = dict.get_item("iceServers") {
            let servers = servers.downcast::<PyList>()?;
            rtc_config.ice_servers = servers
                .iter()
                .filter_map(|server| {
                    server.extract::<IceServer>().ok().map(|server| RTCIceServer {
                        urls: server.urls,
                        username: server.username.unwrap_or_default(),
                        credential: server.credential.unwrap_or_default(),
                        ..Default::default()
                    })
                })
                .collect();
        }

        let api = APIBuilder::new().build();

        let pc = RUNTIME.block_on(async {
            api.new_peer_connection(rtc_config).await
        }).map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

        let pc = Arc::new(pc);

        // Create channel for event communication
        let (event_tx, mut event_rx) = mpsc::channel(32);

        // Clone necessary items for the event handling task
        let ice_callback = on_ice_candidate.clone_ref(py);
        let dc_callback = on_data_channel.clone_ref(py);

        // Spawn event handling task
        let event_task = {
            tokio::task::spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    Python::with_gil(|py| {
                        match event {
                            WebRTCEvent::IceCandidate(candidate) => {
                                if let Err(e) = ice_callback.call1(py, (candidate,)) {
                                    eprintln!("Error calling ice candidate callback: {:?}", e);
                                }
                            }
                            WebRTCEvent::DataChannel(dc) => {
                                let py_dc = PyRTCDataChannel::new(dc.clone());

                                match Py::new(py, py_dc) {
                                    Ok(py_dc_obj) => {
                                        if let Err(e) = dc_callback.call1(py, (py_dc_obj,)) {
                                            eprintln!("Error calling data channel callback: {:?}", e);
                                        }
                                    },
                                    Err(e) => eprintln!("Error creating PyRTCDataChannel: {:?}", e),
                                }
                            }
                        }
                    });
                }
            })
        };

        let instance = Self {
            pc,
            _event_task_handle: Some(event_task),
            trickle_ice,
        };

        // Set up WebRTC callbacks
        if trickle_ice {
            let tx = event_tx.clone();
            instance.pc.on_ice_candidate(Box::new(move |candidate_option| {
                let tx = tx.clone();
                Box::pin(async move {
                    if let Some(candidate) = candidate_option {
                        let candidate_str = if !candidate.related_address.is_empty() {
                            format!(
                                "candidate:{} {} {} {} {} {} typ {} raddr {} rport {}",
                                candidate.foundation,
                                candidate.component,
                                candidate.protocol.to_string().to_lowercase(),
                                candidate.priority,
                                candidate.address,
                                candidate.port,
                                candidate.typ.to_string().to_lowercase(),
                                candidate.related_address,
                                candidate.related_port
                            )
                        } else {
                            format!(
                                "candidate:{} {} {} {} {} {} typ {}",
                                candidate.foundation,
                                candidate.component,
                                candidate.protocol.to_string().to_lowercase(),
                                candidate.priority,
                                candidate.address,
                                candidate.port,
                                candidate.typ.to_string().to_lowercase()
                            )
                        };

                        let _ = tx.send(WebRTCEvent::IceCandidate(candidate_str)).await;
                    }
                })
            }));
        }

        let tx = event_tx;
        instance.pc.on_data_channel(Box::new(move |dc| {
            let tx = tx.clone();
            Box::pin(async move {
                let _ = tx.send(WebRTCEvent::DataChannel(dc)).await;
            })
        }));

        Ok(instance)
    }

    fn create_data_channel(&self, py: Python<'_>, label: &str) -> PyResult<Py<PyRTCDataChannel>> {
        // Create default data channel configuration
        let config = RTCDataChannelInit {
            ordered: Some(true),           // Guarantee message order
            max_retransmits: Some(0),      // Don't retransmit lost messages
            max_packet_life_time: None,    // No timeout for packets
            protocol: None,                // No specific protocol
            negotiated: None,              // Let WebRTC handle negotiation
        };

        let dc = RUNTIME.block_on(async {
            self.pc.create_data_channel(label, Some(config)).await
        }).map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

        Py::new(py, PyRTCDataChannel::new(dc.clone()))
    }

    fn create_offer(&self) -> PyResult<String> {
        let _guard = RUNTIME.enter();
        RUNTIME.block_on(async {
            // Create offer
            let offer = self.pc.create_offer(None).await
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

            // Store the offer by setting local description first
            self.pc.set_local_description(offer.clone()).await
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

            // For non-trickle, wait for gathering to complete
            if !self.trickle_ice {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let tx = Arc::new(TokioMutex::new(Some(tx)));

                self.pc.on_ice_gathering_state_change(Box::new(move |s| {
                    let tx = tx.clone();
                    println!("ICE gathering state changed to: {:?}", s);
                    Box::pin(async move {
                        if s == RTCIceGathererState::Complete {
                            if let Some(tx) = tx.lock().await.take() {
                                let _ = tx.send(());
                            }
                        }
                    })
                }));

                // Wait for ICE gathering to complete
                match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
                    Ok(Ok(())) => {
                        if let Some(desc) = self.pc.local_description().await {
                            return Ok(desc.sdp);
                        }
                    },
                    _ => {}
                }
            }

            // For trickle ICE or if gathering completion failed, return initial SDP
            Ok(offer.sdp)
        })
    }

    fn create_answer(&self) -> PyResult<String> {
        let _guard = RUNTIME.enter();
        RUNTIME.block_on(async {
            // Create answer
            let answer = self.pc.create_answer(None).await
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

            // Store the answer by setting local description first
            self.pc.set_local_description(answer.clone()).await
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

            // For non-trickle, wait for gathering to complete
            if !self.trickle_ice {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let tx = Arc::new(TokioMutex::new(Some(tx)));

                self.pc.on_ice_gathering_state_change(Box::new(move |s| {
                    let tx = tx.clone();
                    println!("ICE gathering state changed to: {:?}", s);
                    Box::pin(async move {
                        if s == RTCIceGathererState::Complete {
                            if let Some(tx) = tx.lock().await.take() {
                                let _ = tx.send(());
                            }
                        }
                    })
                }));

                // Wait for ICE gathering to complete
                match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
                    Ok(Ok(())) => {
                        if let Some(desc) = self.pc.local_description().await {
                            return Ok(desc.sdp);
                        }
                    },
                    _ => {}
                }
            }

            // For trickle ICE or if gathering completion failed, return initial SDP
            Ok(answer.sdp)
        })
    }

    fn set_remote_description(&self, sdp: String) -> PyResult<()> {
        let _guard = RUNTIME.enter();
        RUNTIME.block_on(async {
            // Create SessionDescription based on current state
            let desc = match self.pc.signaling_state() {
                RTCSignalingState::HaveLocalOffer => {
                    // If we have a local offer, this must be an answer
                    webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(sdp)
                },
                _ => {
                    // Otherwise, this must be an offer
                    webrtc::peer_connection::sdp::session_description::RTCSessionDescription::offer(sdp)
                }
            }.map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

            self.pc.set_remote_description(desc).await
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))
        })
    }

    fn add_ice_candidate(&self, candidate: String) -> PyResult<()> {
        let _guard = RUNTIME.enter();
        RUNTIME.block_on(async {
            let init = webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
                candidate,
                ..Default::default()
            };
            self.pc.add_ice_candidate(init)
                .await
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))
        })
    }

    #[getter]
    fn connection_state(&self) -> String {
        format!("{:?}", self.pc.connection_state())
    }
}

#[pyclass]
struct PyRTCDataChannel {
    dc: Arc<RTCDataChannel>,
    message_callback: Arc<Mutex<Option<PyObject>>>,
    error_callback: Arc<Mutex<Option<PyObject>>>,
    message_sender: crossbeam_channel::Sender<Vec<u8>>,
    _message_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl PyRTCDataChannel {
    fn new(dc: Arc<RTCDataChannel>) -> Self {
        let (message_sender, message_receiver) = crossbeam_channel::bounded(1024);

        let message_callback = Arc::new(Mutex::new(None::<PyObject>));
        let error_callback = Arc::new(Mutex::new(None::<PyObject>));
        let callback_ref = message_callback.clone();
        let error_ref = error_callback.clone();
        let receiver = message_receiver;

        let task = RUNTIME.spawn(async move {
            loop {
                match receiver.try_recv() {
                    Ok(msg) => {
                        Python::with_gil(|py| {
                            if let Some(cb) = callback_ref.lock().unwrap().as_ref() {
                                if let Err(e) = cb.call1(py, (msg,)) {
                                    if let Some(err_cb) = error_ref.lock().unwrap().as_ref() {
                                        let _ = err_cb.call1(py, (format!("Callback error: {}", e),));
                                    }
                                }
                            }
                        });
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            dc: dc.clone(),
            message_callback,
            error_callback,
            message_sender,
            _message_task: Arc::new(Mutex::new(Some(task))),
        }
    }
}

#[pymethods]
impl PyRTCDataChannel {
    #[getter]
    fn get_on_message(&self) -> Option<PyObject> {
        Python::with_gil(|py| {
            self.message_callback
                .lock()
                .unwrap()
                .as_ref()
                .map(|obj| obj.clone_ref(py))
        })
    }

    #[setter]
    fn set_on_message(&mut self, callback: Option<PyObject>) -> PyResult<()> {
        match callback {
            Some(cb) => {
                Python::with_gil(|py| {
                    *self.message_callback.lock().unwrap() = Some(cb.clone_ref(py));

                    let sender = self.message_sender.clone();
                    self.dc.on_message(Box::new(move |msg| {
                        let sender = sender.clone();
                        Box::pin(async move {
                            if let Err(e) = sender.try_send(msg.data.to_vec()) {
                                eprintln!("Failed to queue message: {}", e);
                            }
                        })
                    }));
                });
            }
            None => {
                *self.message_callback.lock().unwrap() = None;
                self.dc.on_message(Box::new(move |_| Box::pin(async {})));
            }
        }
        Ok(())
    }

    #[getter]
    fn get_on_error(&self) -> Option<PyObject> {
        Python::with_gil(|py| {
            self.error_callback
                .lock()
                .unwrap()
                .as_ref()
                .map(|obj| obj.clone_ref(py))
        })
    }

    #[setter]
    fn set_on_error(&mut self, callback: Option<PyObject>) -> PyResult<()> {
        match callback {
            Some(cb) => {
                Python::with_gil(|py| {
                    *self.error_callback.lock().unwrap() = Some(cb.clone_ref(py));
                });
            }
            None => {
                *self.error_callback.lock().unwrap() = None;
            }
        }
        Ok(())
    }

    #[pyo3(signature = (data, timeout=None))]
    fn send(&self, data: &[u8], timeout: Option<f64>) -> PyResult<()> {
        let bytes = Bytes::copy_from_slice(data);
        let timeout_duration = timeout.map(|t| Duration::from_secs_f64(t))
            .unwrap_or(Duration::from_secs(30));

        RUNTIME.block_on(async {
            tokio::time::timeout(
                timeout_duration,
                self.dc.send(&bytes)
            ).await
                .map_err(|_| PyErr::new::<pyo3::exceptions::PyTimeoutError, _>("Send operation timed out"))?
                .map(|_| ())
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))
        })
    }

    #[getter]
    fn buffered_amount(&self) -> PyResult<u64> {
        Ok(RUNTIME.block_on(async {
            self.dc.buffered_amount().await
        }) as u64)
    }

    #[pyo3(signature = (timeout=None))]
    fn wait_for_channel_open(&self, timeout: Option<f64>) -> PyResult<bool> {
        let timeout_duration = timeout.map(|t| Duration::from_secs_f64(t))
            .unwrap_or(Duration::from_secs(30));

        RUNTIME.block_on(async {
            let start = std::time::Instant::now();
            while start.elapsed() < timeout_duration {
                if self.dc.ready_state() == RTCDataChannelState::Open {
                    return Ok(true);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(false)
        })
    }

    fn close(&self) -> PyResult<()> {
        if let Ok(mut task) = self._message_task.lock() {
            if let Some(old_task) = task.take() {
                old_task.abort();
            }
        }

        RUNTIME.block_on(async {
            self.dc.close().await
        }).map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))
    }

    #[getter]
    fn ready_state(&self) -> String {
        format!("{:?}", self.dc.ready_state())
    }

    #[getter]
    fn label(&self) -> String {
        self.dc.label().to_string()
    }
}

#[pymodule]
fn pam_rustwebrtc(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRTCPeerConnection>()?;
    m.add_class::<PyRTCDataChannel>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::{Mutex as TokioMutex};
    use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
    use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;

    // Helper function to create a peer connection
    async fn create_peer_connection(config: RTCConfiguration) -> webrtc::error::Result<RTCPeerConnection> {
        let api = APIBuilder::new().build();
        api.new_peer_connection(config).await
    }

    #[test]
    fn test_webrtc_connection_creation() {
        let result = RUNTIME.block_on(async {

            let config = RTCConfiguration::default();
            create_peer_connection(config).await
        });

        assert!(result.is_ok());
        let pc = result.unwrap();
        assert_eq!(pc.connection_state(), RTCPeerConnectionState::New);
    }

    #[test]
    fn test_data_channel_creation() {
        let result = RUNTIME.block_on(async {
            let config = RTCConfiguration::default();
            let pc = create_peer_connection(config).await?;
            pc.create_data_channel("test", None).await
        });

        assert!(result.is_ok());
        let dc = result.unwrap();
        assert_eq!(dc.label(), "test");
    }

    // Helper function to exchange ICE candidates
    async fn exchange_ice_candidates(
        peer1: Arc<RTCPeerConnection>,
        peer2: Arc<RTCPeerConnection>,
    ) {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        let _ready_tx = Arc::new(Mutex::new(Some(ready_tx)));
        println!("Starting exchange_ice_candidates");
        let (ice_tx1, mut ice_rx1) = mpsc::channel::<String>(32);
        let (ice_tx2, mut ice_rx2) = mpsc::channel::<String>(32);

        // Set up ICE candidate handlers
        let peer1_clone = peer1.clone();
        peer1_clone.on_ice_candidate(Box::new(move |c| {
            let ice_tx = ice_tx1.clone();
            Box::pin(async move {
                if let Some(candidate) = c {
                    let candidate_str = format_ice_candidate(&candidate);
                    println!("Found ICE candidate for peer1: {:?}", candidate_str);
                    if let Err(e) = ice_tx.send(candidate_str).await {
                        eprintln!("Failed to send ICE candidate for peer1: {:?}", e);
                    }
                }
            })
        }));

        let peer2_clone = peer2.clone();
        peer2_clone.on_ice_candidate(Box::new(move |c| {
            let ice_tx = ice_tx2.clone();
            Box::pin(async move {
                if let Some(candidate) = c {
                    let candidate_str = format_ice_candidate(&candidate);
                    println!("Found ICE candidate for peer2: {:?}", candidate_str);
                    if let Err(e) = ice_tx.send(candidate_str).await {
                        eprintln!("Failed to send ICE candidate for peer2: {:?}", e);
                    }
                }
            })
        }));

        // Handle candidate exchange
        let peer2_clone = peer2.clone();
        let handle1 = tokio::spawn(async move {
            while let Some(candidate) = ice_rx1.recv().await {
                let init = RTCIceCandidateInit {
                    candidate,
                    ..Default::default()
                };
                if let Err(err) = peer2_clone.add_ice_candidate(init).await {
                    eprintln!("Error adding ICE candidate to peer2: {:?}", err);
                }
            }
        });

        let peer1_clone = peer1.clone();
        let handle2 = tokio::spawn(async move {
            while let Some(candidate) = ice_rx2.recv().await {
                let init = RTCIceCandidateInit {
                    candidate,
                    ..Default::default()
                };
                if let Err(err) = peer1_clone.add_ice_candidate(init).await {
                    eprintln!("Error adding ICE candidate to peer1: {:?}", err);
                }
            }
        });

        // Set a timeout for ICE gathering
        let timeout = tokio::time::sleep(tokio::time::Duration::from_secs(5));
        tokio::pin!(timeout);

        tokio::select! {
            _ = timeout => {
                println!("ICE gathering timed out");
            }
            _ = ready_rx => {
                println!("ICE gathering completed successfully");
            }
        }

        // Clean up handlers
        let _ = handle1.abort();
        let _ = handle2.abort();
    }

    fn format_ice_candidate(candidate: &RTCIceCandidate) -> String {
        if !candidate.related_address.is_empty() {
            format!(
                "candidate:{} {} {} {} {} {} typ {} raddr {} rport {}",
                candidate.foundation,
                candidate.component,
                candidate.protocol.to_string().to_lowercase(),
                candidate.priority,
                candidate.address,
                candidate.port,
                candidate.typ.to_string().to_lowercase(),
                candidate.related_address,
                candidate.related_port
            )
        } else {
            format!(
                "candidate:{} {} {} {} {} {} typ {}",
                candidate.foundation,
                candidate.component,
                candidate.protocol.to_string().to_lowercase(),
                candidate.priority,
                candidate.address,
                candidate.port,
                candidate.typ.to_string().to_lowercase()
            )
        }
    }

    #[test]
    fn test_p2p_connection() {
        println!("Starting P2P connection test");
        RUNTIME.block_on(async {
            let config = RTCConfiguration::default();
            let peer1 = Arc::new(create_peer_connection(config.clone()).await.unwrap());
            let peer2 = Arc::new(create_peer_connection(config).await.unwrap());

            let done_signal = Arc::new(TokioMutex::new(false));
            let done_signal_clone = Arc::clone(&done_signal);

            let dc_received = Arc::new(TokioMutex::new(None));
            let dc_received_clone = Arc::clone(&dc_received);

            // Set up connection state callback
            peer2.on_peer_connection_state_change(Box::new(move |s| {
                let done = Arc::clone(&done_signal_clone);
                Box::pin(async move {
                    if s == RTCPeerConnectionState::Connected {
                        let mut done = done.lock().await;
                        *done = true;
                    }
                })
            }));

            // Set up data channel callback
            peer2.on_data_channel(Box::new(move |dc| {
                let dc_received = Arc::clone(&dc_received_clone);
                Box::pin(async move {
                    let mut dc_guard = dc_received.lock().await;
                    *dc_guard = Some(dc.clone());
                })
            }));

            println!("Creating data channel");
            let dc1 = peer1.create_data_channel("test-channel", None).await.unwrap();
            println!("Data channel created successfully");

            // Monitor ICE gathering state
            peer1.on_ice_gathering_state_change(Box::new(|s| {
                println!("Peer1 ICE gathering state changed to: {:?}", s);
                Box::pin(async {})
            }));

            peer2.on_ice_gathering_state_change(Box::new(|s| {
                println!("Peer2 ICE gathering state changed to: {:?}", s);
                Box::pin(async {})
            }));

            // Exchange ICE candidates
            let ice_exchange = tokio::spawn(exchange_ice_candidates(peer1.clone(), peer2.clone()));

            println!("Creating and setting offer");
            let offer = peer1.create_offer(None).await.unwrap();
            println!("Created offer: {:?}", offer);

            peer1.set_local_description(offer.clone()).await.unwrap();
            println!("Set local description on peer1");

            peer2.set_remote_description(offer).await.unwrap();
            println!("Set remote description on peer2");

            println!("Creating and setting answer");
            let answer = peer2.create_answer(None).await.unwrap();
            println!("Created answer: {:?}", answer);

            peer2.set_local_description(answer.clone()).await.unwrap();
            println!("Set local description on peer2");

            peer1.set_remote_description(answer).await.unwrap();
            println!("Set remote description on peer1");

            println!("Waiting for connection to establish");
            let mut retries = 50;
            while retries > 0 && !*done_signal.lock().await {
                println!("Connection state - peer1: {:?}, peer2: {:?}",
                         peer1.connection_state(),
                         peer2.connection_state()
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                retries -= 1;
            }

            // Ensure ICE candidate exchange completed
            ice_exchange.await.expect("ICE exchange task failed");

            assert_eq!(peer1.connection_state(), RTCPeerConnectionState::Connected);
            assert_eq!(peer2.connection_state(), RTCPeerConnectionState::Connected);

            // Test data channel
            let dc2 = dc_received.lock().await;
            assert!(dc2.is_some(), "Data channel should have been received");
            assert_eq!(dc2.as_ref().unwrap().label(), "test-channel");

            // Test message sending
            let message = Bytes::from("Hello from peer1!");
            dc1.send(&message).await.unwrap();

            if let Some(dc2) = &*dc2 {
                let (msg_tx, mut msg_rx) = mpsc::channel(1);

                dc2.on_message(Box::new(move |msg| {
                    let tx = msg_tx.clone();
                    Box::pin(async move {
                        let _ = tx.send(msg.data).await;
                    })
                }));

                let received = tokio::time::timeout(
                    tokio::time::Duration::from_secs(5),
                    msg_rx.recv()
                ).await.unwrap();

                assert_eq!(received.unwrap(), message);
            }
        });
    }

    #[test]
    fn test_p2p_connection_non_trickle() {
        println!("Starting non-trickle P2P connection test");
        RUNTIME.block_on(async {
            // Create config with STUN servers first
            let config = RTCConfiguration {
                ice_servers: vec![
                    RTCIceServer {
                        urls: vec!["stun:stun.l.google.com:19302".to_string()],
                        ..Default::default()
                    }
                ],
                ..Default::default()
            };

            let api = APIBuilder::new().build();
            let peer1 = Arc::new(api.new_peer_connection(config.clone()).await.unwrap());
            let peer2 = Arc::new(api.new_peer_connection(config).await.unwrap());

            let done_signal = Arc::new(TokioMutex::new(false));
            let done_signal_clone = Arc::clone(&done_signal);

            // Set up connection state callback
            peer2.on_peer_connection_state_change(Box::new(move |s| {
                let done = Arc::clone(&done_signal_clone);
                Box::pin(async move {
                    println!("Peer2 connection state changed to: {:?}", s);
                    if s == RTCPeerConnectionState::Connected {
                        let mut done = done.lock().await;
                        *done = true;
                    }
                })
            }));

            println!("Creating data channel");
            let dc1 = peer1.create_data_channel("test-channel", None).await.unwrap();
            println!("Data channel created successfully");

            // Create channels for ICE gathering completion
            let (gather_tx1, gather_rx1) = tokio::sync::oneshot::channel();
            let gather_tx1 = Arc::new(TokioMutex::new(Some(gather_tx1)));
            let (gather_tx2, gather_rx2) = tokio::sync::oneshot::channel();
            let gather_tx2 = Arc::new(TokioMutex::new(Some(gather_tx2)));

            // Set up ICE gathering state monitoring for peer1
            peer1.on_ice_gathering_state_change(Box::new(move |s| {
                let tx = gather_tx1.clone();
                println!("Peer1 ICE gathering state: {:?}", s);
                Box::pin(async move {
                    if s == RTCIceGathererState::Complete {
                        if let Some(tx) = tx.lock().await.take() {
                            let _ = tx.send(());
                        }
                    }
                })
            }));

            // Set up ICE gathering state monitoring for peer2
            peer2.on_ice_gathering_state_change(Box::new(move |s| {
                let tx = gather_tx2.clone();
                println!("Peer2 ICE gathering state: {:?}", s);
                Box::pin(async move {
                    if s == RTCIceGathererState::Complete {
                        if let Some(tx) = tx.lock().await.take() {
                            let _ = tx.send(());
                        }
                    }
                })
            }));

            // Create and set local description for peer1 (offer)
            println!("Creating offer and waiting for ICE gathering");
            let offer = peer1.create_offer(None).await.unwrap();
            peer1.set_local_description(offer).await.unwrap();

            // Wait for peer1's ICE gathering to complete
            match tokio::time::timeout(std::time::Duration::from_secs(30), gather_rx1).await {
                Ok(Ok(())) => {
                    if let Some(complete_offer) = peer1.local_description().await {
                        println!("Complete offer with ICE candidates:\n{}", complete_offer.sdp);
                        assert!(complete_offer.sdp.contains("a=candidate:"),
                                "Offer should contain ICE candidates");

                        // Set the complete offer on peer2
                        peer2.set_remote_description(complete_offer).await.unwrap();

                        // Create and set local description for peer2 (answer)
                        println!("Creating answer and waiting for ICE gathering");
                        let answer = peer2.create_answer(None).await.unwrap();
                        peer2.set_local_description(answer).await.unwrap();

                        // Wait for peer2's ICE gathering to complete
                        match tokio::time::timeout(std::time::Duration::from_secs(30), gather_rx2).await {
                            Ok(Ok(())) => {
                                if let Some(complete_answer) = peer2.local_description().await {
                                    println!("Complete answer with ICE candidates:\n{}", complete_answer.sdp);
                                    assert!(complete_answer.sdp.contains("a=candidate:"),
                                            "Answer should contain ICE candidates");

                                    // Set the complete answer on peer1
                                    peer1.set_remote_description(complete_answer).await.unwrap();
                                }
                            },
                            _ => panic!("Timeout waiting for peer2 ICE gathering"),
                        }
                    }
                },
                _ => panic!("Timeout waiting for peer1 ICE gathering"),
            }

            // Wait for connection with increased timeout
            println!("Waiting for connection to establish");
            let mut retries = 150; // 30 seconds total
            while retries > 0 && !*done_signal.lock().await {
                println!("Connection state - peer1: {:?}, peer2: {:?}",
                         peer1.connection_state(),
                         peer2.connection_state()
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                retries -= 1;
            }

            assert_eq!(peer1.connection_state(), RTCPeerConnectionState::Connected);
            assert_eq!(peer2.connection_state(), RTCPeerConnectionState::Connected);

            // Test data channel functionality
            let message = Bytes::from("Hello WebRTC!");
            dc1.send(&message).await.unwrap();

            // Wait for message echo
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        });
    }
}