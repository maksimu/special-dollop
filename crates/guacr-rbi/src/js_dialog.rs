// JavaScript Dialog Handling for RBI
//
// Handles JavaScript alerts, confirms, prompts, and beforeunload dialogs
// in the isolated browser session.
// Based on KCM's KCM-504-javascript-alerts branch.
//
// These dialogs would block the browser event loop if not handled,
// causing the session to become unresponsive.

use bytes::Bytes;
use log::{debug, info, warn};
use std::time::{Duration, Instant};

/// Maximum time to wait for user response to a dialog
pub const DIALOG_TIMEOUT_SECS: u64 = 60;

/// Maximum length of dialog message to display
pub const MAX_MESSAGE_LENGTH: usize = 2048;

/// Types of JavaScript dialogs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsDialogType {
    /// window.alert() - informational, user acknowledges
    Alert,
    /// window.confirm() - yes/no question
    Confirm,
    /// window.prompt() - text input request
    Prompt,
    /// beforeunload event - leave page warning
    BeforeUnload,
}

impl JsDialogType {
    pub fn as_str(&self) -> &'static str {
        match self {
            JsDialogType::Alert => "alert",
            JsDialogType::Confirm => "confirm",
            JsDialogType::Prompt => "prompt",
            JsDialogType::BeforeUnload => "beforeunload",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "alert" => Some(JsDialogType::Alert),
            "confirm" => Some(JsDialogType::Confirm),
            "prompt" => Some(JsDialogType::Prompt),
            "beforeunload" | "onbeforeunload" => Some(JsDialogType::BeforeUnload),
            _ => None,
        }
    }
}

/// JavaScript dialog configuration
#[derive(Debug, Clone)]
pub struct JsDialogConfig {
    /// Whether to show dialogs to user (vs auto-dismiss)
    pub show_dialogs: bool,
    /// Auto-dismiss alerts after this duration
    pub auto_dismiss_alert_ms: Option<u64>,
    /// Auto-confirm (true/false) for confirm dialogs
    pub auto_confirm: Option<bool>,
    /// Auto-response for prompt dialogs
    pub auto_prompt_response: Option<String>,
    /// Allow beforeunload dialogs (can be abused by malicious sites)
    pub allow_beforeunload: bool,
    /// Maximum dialog timeout in seconds
    pub timeout_secs: u64,
}

impl Default for JsDialogConfig {
    fn default() -> Self {
        Self {
            show_dialogs: true,
            auto_dismiss_alert_ms: Some(5000), // Auto-dismiss alerts after 5s
            auto_confirm: None,                // Show confirm dialogs to user
            auto_prompt_response: None,        // Show prompt dialogs to user
            allow_beforeunload: false,         // Block beforeunload by default
            timeout_secs: DIALOG_TIMEOUT_SECS,
        }
    }
}

/// A JavaScript dialog request from the browser
#[derive(Debug, Clone)]
pub struct JsDialogRequest {
    /// Unique identifier for this dialog
    pub id: String,
    /// Type of dialog
    pub dialog_type: JsDialogType,
    /// Dialog message text
    pub message: String,
    /// Default value (for prompts)
    pub default_value: Option<String>,
    /// URL of page showing the dialog
    pub origin_url: String,
    /// When the dialog was shown
    pub shown_at: Instant,
}

impl JsDialogRequest {
    /// Check if dialog has timed out
    pub fn is_timed_out(&self, timeout_secs: u64) -> bool {
        self.shown_at.elapsed() > Duration::from_secs(timeout_secs)
    }

    /// Get truncated message for display
    pub fn display_message(&self) -> &str {
        if self.message.len() > MAX_MESSAGE_LENGTH {
            &self.message[..MAX_MESSAGE_LENGTH]
        } else {
            &self.message
        }
    }
}

/// Response to a JavaScript dialog
#[derive(Debug, Clone)]
pub struct JsDialogResponse {
    /// Dialog ID this response is for
    pub id: String,
    /// Whether user confirmed (OK) or cancelled
    pub confirmed: bool,
    /// User's text input (for prompts)
    pub input: Option<String>,
}

/// JavaScript dialog manager
pub struct JsDialogManager {
    config: JsDialogConfig,
    /// Currently pending dialog (only one at a time per browser)
    pending_dialog: Option<JsDialogRequest>,
    /// Counter for generating dialog IDs
    next_id: u64,
}

impl JsDialogManager {
    pub fn new(config: JsDialogConfig) -> Self {
        Self {
            config,
            pending_dialog: None,
            next_id: 1,
        }
    }

    /// Handle a new JavaScript dialog from the browser
    /// Returns the dialog to show to user, or None if auto-handled
    pub fn handle_dialog(
        &mut self,
        dialog_type: JsDialogType,
        message: String,
        default_value: Option<String>,
        origin_url: String,
    ) -> Option<(JsDialogRequest, Option<JsDialogResponse>)> {
        // beforeunload can be used maliciously, optionally block
        if dialog_type == JsDialogType::BeforeUnload && !self.config.allow_beforeunload {
            info!("RBI: Blocked beforeunload dialog from {}", origin_url);
            return Some((
                JsDialogRequest {
                    id: "blocked".to_string(),
                    dialog_type,
                    message,
                    default_value,
                    origin_url,
                    shown_at: Instant::now(),
                },
                Some(JsDialogResponse {
                    id: "blocked".to_string(),
                    confirmed: true, // Allow navigation
                    input: None,
                }),
            ));
        }

        // Check for auto-handling
        if !self.config.show_dialogs {
            return self.auto_handle(dialog_type, message, default_value, origin_url);
        }

        // Create dialog request
        let dialog = JsDialogRequest {
            id: format!("jsdialog-{}", self.next_id),
            dialog_type,
            message,
            default_value,
            origin_url,
            shown_at: Instant::now(),
        };
        self.next_id += 1;

        info!(
            "RBI: JavaScript {} dialog - id={}, origin={}",
            dialog.dialog_type.as_str(),
            dialog.id,
            dialog.origin_url
        );

        // Check for type-specific auto-handling
        let auto_response = match dialog_type {
            JsDialogType::Alert if self.config.auto_dismiss_alert_ms.is_some() => {
                // Will auto-dismiss after timeout
                None
            }
            JsDialogType::Confirm if self.config.auto_confirm.is_some() => Some(JsDialogResponse {
                id: dialog.id.clone(),
                confirmed: self.config.auto_confirm.unwrap(),
                input: None,
            }),
            JsDialogType::Prompt if self.config.auto_prompt_response.is_some() => {
                Some(JsDialogResponse {
                    id: dialog.id.clone(),
                    confirmed: true,
                    input: self.config.auto_prompt_response.clone(),
                })
            }
            _ => None,
        };

        self.pending_dialog = Some(dialog.clone());
        Some((dialog, auto_response))
    }

    /// Auto-handle dialog without user interaction
    fn auto_handle(
        &mut self,
        dialog_type: JsDialogType,
        message: String,
        default_value: Option<String>,
        origin_url: String,
    ) -> Option<(JsDialogRequest, Option<JsDialogResponse>)> {
        let id = format!("jsdialog-{}", self.next_id);
        self.next_id += 1;

        let dialog = JsDialogRequest {
            id: id.clone(),
            dialog_type,
            message,
            default_value: default_value.clone(),
            origin_url,
            shown_at: Instant::now(),
        };

        let response = match dialog_type {
            JsDialogType::Alert => {
                debug!("RBI: Auto-dismissing alert");
                JsDialogResponse {
                    id,
                    confirmed: true,
                    input: None,
                }
            }
            JsDialogType::Confirm => {
                let confirmed = self.config.auto_confirm.unwrap_or(false);
                debug!("RBI: Auto-responding to confirm: {}", confirmed);
                JsDialogResponse {
                    id,
                    confirmed,
                    input: None,
                }
            }
            JsDialogType::Prompt => {
                let input = self.config.auto_prompt_response.clone().or(default_value);
                debug!("RBI: Auto-responding to prompt");
                JsDialogResponse {
                    id,
                    confirmed: input.is_some(),
                    input,
                }
            }
            JsDialogType::BeforeUnload => {
                debug!("RBI: Auto-confirming beforeunload");
                JsDialogResponse {
                    id,
                    confirmed: true,
                    input: None,
                }
            }
        };

        Some((dialog, Some(response)))
    }

    /// Handle user response to a dialog
    pub fn handle_response(&mut self, response: JsDialogResponse) -> Result<(), String> {
        match &self.pending_dialog {
            Some(dialog) if dialog.id == response.id => {
                info!(
                    "RBI: Dialog response - id={}, confirmed={}",
                    response.id, response.confirmed
                );
                self.pending_dialog = None;
                Ok(())
            }
            Some(_) => Err("Response ID doesn't match pending dialog".to_string()),
            None => Err("No pending dialog".to_string()),
        }
    }

    /// Check for timed-out dialogs
    pub fn check_timeout(&mut self) -> Option<JsDialogResponse> {
        if let Some(ref dialog) = self.pending_dialog {
            // Auto-dismiss alerts after configured time
            if dialog.dialog_type == JsDialogType::Alert {
                if let Some(dismiss_ms) = self.config.auto_dismiss_alert_ms {
                    if dialog.shown_at.elapsed() > Duration::from_millis(dismiss_ms) {
                        let response = JsDialogResponse {
                            id: dialog.id.clone(),
                            confirmed: true,
                            input: None,
                        };
                        self.pending_dialog = None;
                        info!("RBI: Auto-dismissed alert after {}ms", dismiss_ms);
                        return Some(response);
                    }
                }
            }

            // General timeout
            if dialog.is_timed_out(self.config.timeout_secs) {
                let response = JsDialogResponse {
                    id: dialog.id.clone(),
                    confirmed: false,
                    input: None,
                };
                self.pending_dialog = None;
                warn!("RBI: Dialog timed out - id={}", response.id);
                return Some(response);
            }
        }
        None
    }

    /// Get currently pending dialog
    pub fn get_pending(&self) -> Option<&JsDialogRequest> {
        self.pending_dialog.as_ref()
    }

    /// Check if a dialog is pending
    pub fn has_pending(&self) -> bool {
        self.pending_dialog.is_some()
    }
}

/// Generate Guacamole instruction to show dialog to client
pub fn format_dialog_instruction(dialog: &JsDialogRequest) -> Bytes {
    // Use Guacamole's pipe instruction for JS dialog
    // The client-side JavaScript will render this as a dialog
    let dialog_json = serde_json::json!({
        "id": dialog.id,
        "type": dialog.dialog_type.as_str(),
        "message": dialog.display_message(),
        "defaultValue": dialog.default_value,
        "origin": dialog.origin_url,
    });

    let json_str = dialog_json.to_string();
    let instr = format!(
        "4.pipe,0.{},16.application/json,9.js-dialog,{}.{};",
        dialog.id.len(),
        json_str.len(),
        json_str
    );
    Bytes::from(instr)
}

/// JavaScript to inject for intercepting dialogs via CDP
pub const DIALOG_INTERCEPTOR_JS: &str = r#"
(function() {
    // Store original functions
    const originalAlert = window.alert;
    const originalConfirm = window.confirm;
    const originalPrompt = window.prompt;
    
    // Track intercepted calls for CDP event handler
    window.__guac_dialog_pending = null;
    
    // These will be handled by CDP Page.javascriptDialogOpening event
    // This script just ensures we can track dialog state
    
    console.log('Guacamole: Dialog interceptor installed');
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dialog_type_parsing() {
        assert_eq!(JsDialogType::parse("alert"), Some(JsDialogType::Alert));
        assert_eq!(JsDialogType::parse("CONFIRM"), Some(JsDialogType::Confirm));
        assert_eq!(JsDialogType::parse("prompt"), Some(JsDialogType::Prompt));
        assert_eq!(
            JsDialogType::parse("beforeunload"),
            Some(JsDialogType::BeforeUnload)
        );
        assert_eq!(
            JsDialogType::parse("onbeforeunload"),
            Some(JsDialogType::BeforeUnload)
        );
        assert_eq!(JsDialogType::parse("unknown"), None);
        assert_eq!(JsDialogType::parse(""), None);
    }

    #[test]
    fn test_dialog_type_as_str() {
        assert_eq!(JsDialogType::Alert.as_str(), "alert");
        assert_eq!(JsDialogType::Confirm.as_str(), "confirm");
        assert_eq!(JsDialogType::Prompt.as_str(), "prompt");
        assert_eq!(JsDialogType::BeforeUnload.as_str(), "beforeunload");
    }

    #[test]
    fn test_auto_dismiss_alert() {
        let config = JsDialogConfig {
            show_dialogs: false,
            ..Default::default()
        };
        let mut manager = JsDialogManager::new(config);

        let result = manager.handle_dialog(
            JsDialogType::Alert,
            "Test alert".to_string(),
            None,
            "https://example.com".to_string(),
        );

        assert!(result.is_some());
        let (_, response) = result.unwrap();
        assert!(response.is_some());
        assert!(response.unwrap().confirmed);
    }

    #[test]
    fn test_auto_confirm_dialog() {
        let config = JsDialogConfig {
            show_dialogs: false,
            auto_confirm: Some(true),
            ..Default::default()
        };
        let mut manager = JsDialogManager::new(config);

        let result = manager.handle_dialog(
            JsDialogType::Confirm,
            "Delete all files?".to_string(),
            None,
            "https://example.com".to_string(),
        );

        assert!(result.is_some());
        let (_, response) = result.unwrap();
        assert!(response.is_some());
        assert!(response.unwrap().confirmed);
    }

    #[test]
    fn test_auto_deny_confirm_dialog() {
        let config = JsDialogConfig {
            show_dialogs: false,
            auto_confirm: Some(false),
            ..Default::default()
        };
        let mut manager = JsDialogManager::new(config);

        let result = manager.handle_dialog(
            JsDialogType::Confirm,
            "Delete all files?".to_string(),
            None,
            "https://example.com".to_string(),
        );

        assert!(result.is_some());
        let (_, response) = result.unwrap();
        assert!(response.is_some());
        assert!(!response.unwrap().confirmed);
    }

    #[test]
    fn test_auto_prompt_response() {
        let config = JsDialogConfig {
            show_dialogs: false,
            auto_prompt_response: Some("auto-value".to_string()),
            ..Default::default()
        };
        let mut manager = JsDialogManager::new(config);

        let result = manager.handle_dialog(
            JsDialogType::Prompt,
            "Enter your name:".to_string(),
            Some("default-name".to_string()),
            "https://example.com".to_string(),
        );

        assert!(result.is_some());
        let (_, response) = result.unwrap();
        assert!(response.is_some());
        let resp = response.unwrap();
        assert!(resp.confirmed);
        assert_eq!(resp.input.as_deref(), Some("auto-value"));
    }

    #[test]
    fn test_prompt_with_default_value() {
        let config = JsDialogConfig {
            show_dialogs: false,
            auto_prompt_response: None, // Will use default value
            ..Default::default()
        };
        let mut manager = JsDialogManager::new(config);

        let result = manager.handle_dialog(
            JsDialogType::Prompt,
            "Enter value:".to_string(),
            Some("default-123".to_string()),
            "https://example.com".to_string(),
        );

        assert!(result.is_some());
        let (_, response) = result.unwrap();
        assert!(response.is_some());
        let resp = response.unwrap();
        assert!(resp.confirmed);
        assert_eq!(resp.input.as_deref(), Some("default-123"));
    }

    #[test]
    fn test_block_beforeunload() {
        let config = JsDialogConfig {
            allow_beforeunload: false,
            ..Default::default()
        };
        let mut manager = JsDialogManager::new(config);

        let result = manager.handle_dialog(
            JsDialogType::BeforeUnload,
            "Are you sure?".to_string(),
            None,
            "https://example.com".to_string(),
        );

        assert!(result.is_some());
        let (_, response) = result.unwrap();
        assert!(response.is_some());
        // Should auto-confirm to allow navigation
        assert!(response.unwrap().confirmed);
    }

    #[test]
    fn test_allow_beforeunload() {
        let config = JsDialogConfig {
            allow_beforeunload: true,
            show_dialogs: true,
            ..Default::default()
        };
        let mut manager = JsDialogManager::new(config);

        let result = manager.handle_dialog(
            JsDialogType::BeforeUnload,
            "Are you sure?".to_string(),
            None,
            "https://example.com".to_string(),
        );

        assert!(result.is_some());
        let (dialog, response) = result.unwrap();
        // Should show dialog to user (no auto-response)
        assert!(response.is_none());
        assert!(manager.has_pending());
        assert_eq!(dialog.dialog_type, JsDialogType::BeforeUnload);
    }

    #[test]
    fn test_message_truncation() {
        let long_message = "x".repeat(MAX_MESSAGE_LENGTH + 100);
        let dialog = JsDialogRequest {
            id: "test".to_string(),
            dialog_type: JsDialogType::Alert,
            message: long_message.clone(),
            default_value: None,
            origin_url: "https://example.com".to_string(),
            shown_at: Instant::now(),
        };

        assert_eq!(dialog.display_message().len(), MAX_MESSAGE_LENGTH);
        assert_eq!(dialog.message.len(), MAX_MESSAGE_LENGTH + 100);
    }

    #[test]
    fn test_short_message_not_truncated() {
        let message = "Short message";
        let dialog = JsDialogRequest {
            id: "test".to_string(),
            dialog_type: JsDialogType::Alert,
            message: message.to_string(),
            default_value: None,
            origin_url: "https://example.com".to_string(),
            shown_at: Instant::now(),
        };

        assert_eq!(dialog.display_message(), message);
    }

    #[test]
    fn test_dialog_response_handling() {
        let config = JsDialogConfig {
            show_dialogs: true,
            ..Default::default()
        };
        let mut manager = JsDialogManager::new(config);

        // Create a dialog
        let result = manager.handle_dialog(
            JsDialogType::Confirm,
            "Proceed?".to_string(),
            None,
            "https://example.com".to_string(),
        );

        let (dialog, _) = result.unwrap();
        assert!(manager.has_pending());

        // Send response
        let response = JsDialogResponse {
            id: dialog.id.clone(),
            confirmed: true,
            input: None,
        };

        assert!(manager.handle_response(response).is_ok());
        assert!(!manager.has_pending());
    }

    #[test]
    fn test_wrong_response_id() {
        let config = JsDialogConfig {
            show_dialogs: true,
            ..Default::default()
        };
        let mut manager = JsDialogManager::new(config);

        // Create a dialog
        manager.handle_dialog(
            JsDialogType::Alert,
            "Test".to_string(),
            None,
            "https://example.com".to_string(),
        );

        // Send response with wrong ID
        let response = JsDialogResponse {
            id: "wrong-id".to_string(),
            confirmed: true,
            input: None,
        };

        assert!(manager.handle_response(response).is_err());
    }

    #[test]
    fn test_no_pending_dialog() {
        let config = JsDialogConfig::default();
        let mut manager = JsDialogManager::new(config);

        assert!(!manager.has_pending());
        assert!(manager.get_pending().is_none());

        let response = JsDialogResponse {
            id: "any".to_string(),
            confirmed: true,
            input: None,
        };

        assert!(manager.handle_response(response).is_err());
    }

    #[test]
    fn test_format_dialog_instruction() {
        let dialog = JsDialogRequest {
            id: "dialog-1".to_string(),
            dialog_type: JsDialogType::Confirm,
            message: "Are you sure?".to_string(),
            default_value: None,
            origin_url: "https://example.com".to_string(),
            shown_at: Instant::now(),
        };

        let instr = format_dialog_instruction(&dialog);
        let instr_str = String::from_utf8_lossy(&instr);

        assert!(instr_str.contains("pipe"));
        assert!(instr_str.contains("js-dialog"));
        assert!(instr_str.contains("confirm"));
        assert!(instr_str.contains("Are you sure?"));
    }

    #[test]
    fn test_dialog_config_defaults() {
        let config = JsDialogConfig::default();

        assert!(config.show_dialogs);
        assert_eq!(config.auto_dismiss_alert_ms, Some(5000));
        assert!(config.auto_confirm.is_none());
        assert!(config.auto_prompt_response.is_none());
        assert!(!config.allow_beforeunload);
        assert_eq!(config.timeout_secs, DIALOG_TIMEOUT_SECS);
    }

    #[test]
    fn test_dialog_timeout_check() {
        let dialog = JsDialogRequest {
            id: "test".to_string(),
            dialog_type: JsDialogType::Alert,
            message: "Test".to_string(),
            default_value: None,
            origin_url: "https://example.com".to_string(),
            shown_at: Instant::now(),
        };

        // Just created, should not be timed out
        assert!(!dialog.is_timed_out(60));

        // Check with zero timeout
        assert!(dialog.is_timed_out(0));
    }
}
