use pyo3::prelude::*;

/// Python-accessible enum for CloseConnectionReason
///
/// This enum represents the various reasons why a connection might be closed.
/// It can be used when calling close_connection() or close_tube() methods.
///
/// Example:
///     reason = PyCloseConnectionReason.Normal
///     registry.close_connection("connection_id", reason.value())
#[pyclass]
#[derive(Clone, Copy)]
pub enum PyCloseConnectionReason {
    Normal = 0,
    Error = 1,
    Timeout = 2,
    ServerRefuse = 4,
    Client = 5,
    Unknown = 6,
    InvalidInstruction = 7,
    GuacdRefuse = 8,
    ConnectionLost = 9,
    ConnectionFailed = 10,
    TunnelClosed = 11,
    AdminClosed = 12,
    ErrorRecording = 13,
    GuacdError = 14,
    AIClosed = 15,
    AddressResolutionFailed = 16,
    DecryptionFailed = 17,
    ConfigurationError = 18,
    ProtocolError = 19,
    UpstreamClosed = 20,
}

#[pymethods]
impl PyCloseConnectionReason {
    /// Get the numeric value of the reason
    fn value(&self) -> u16 {
        *self as u16
    }

    /// Create from a numeric code
    #[staticmethod]
    fn from_code(code: u16) -> Self {
        match code {
            0 => PyCloseConnectionReason::Normal,
            1 => PyCloseConnectionReason::Error,
            2 => PyCloseConnectionReason::Timeout,
            4 => PyCloseConnectionReason::ServerRefuse,
            5 => PyCloseConnectionReason::Client,
            6 => PyCloseConnectionReason::Unknown,
            7 => PyCloseConnectionReason::InvalidInstruction,
            8 => PyCloseConnectionReason::GuacdRefuse,
            9 => PyCloseConnectionReason::ConnectionLost,
            10 => PyCloseConnectionReason::ConnectionFailed,
            11 => PyCloseConnectionReason::TunnelClosed,
            12 => PyCloseConnectionReason::AdminClosed,
            13 => PyCloseConnectionReason::ErrorRecording,
            14 => PyCloseConnectionReason::GuacdError,
            15 => PyCloseConnectionReason::AIClosed,
            16 => PyCloseConnectionReason::AddressResolutionFailed,
            17 => PyCloseConnectionReason::DecryptionFailed,
            18 => PyCloseConnectionReason::ConfigurationError,
            19 => PyCloseConnectionReason::ProtocolError,
            20 => PyCloseConnectionReason::UpstreamClosed,
            _ => PyCloseConnectionReason::Unknown,
        }
    }
}
