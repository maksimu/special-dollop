# AI Threat Detection with BAML

## Overview

The threat detection system integrates with BAML (Boundary ML) REST API to analyze **live session activity** and detect malicious commands or suspicious behavior. Works with **all protocol handlers** (SSH, Telnet, RDP, VNC, Database, SFTP, RBI).

**Three Modes:**
1. **Reactive**: Analyzes after command sent, can terminate session
2. **Proactive**: Buffers and requires AI approval before execution
3. **Stateful**: AI has full terminal state access for context-aware decisions

**Key Difference from Python Gateway**: 
- **Python**: Parses `.tys` recording files after session ends
- **Rust**: Direct access to live Guacamole protocol + terminal state during session

The system uses BAML's structured functions:
- **`ExtractKeystrokeAnalysis`**: Analyzes live keyboard input (keystroke sequences) as they're typed
- **`ExtractCommandSummary`**: Generates session summaries from command history built from live input

## Architecture

```
SSH Handler (Live Terminal Data)
    ├─ Keyboard Input (bytes) ──────┐
    └─ Terminal Output (bytes) ─────┤
                                     ↓
Threat Detector (guacr-threat-detection)
    ├─ analyze_keystroke_sequence() ──┐
    └─ analyze_terminal_output() ─────┤
                                       ↓ HTTP POST
BAML REST API
    ├─ ExtractKeystrokeAnalysis (live keyboard input)
    └─ ExtractCommandSummary (from command history)
    ↓
Threat Analysis Result
    ↓
Session Termination (if critical/high threat)
```

**Data Flow**:
1. **Keyboard Input**: Raw bytes from user keystrokes → `analyze_keystroke_sequence()` → BAML `ExtractKeystrokeAnalysis`
2. **Terminal Output**: Raw bytes from SSH server → `analyze_terminal_output()` → BAML `ExtractKeystrokeAnalysis`
3. **Command History**: Built from live keyboard input, stored per session
4. **Session Summary**: Generated from command history → BAML `ExtractCommandSummary`

## Features

- **Live Data Analysis**: Analyzes actual keyboard input and terminal output bytes directly from SSH connection (not parsed from recording files)
- **Real-time Analysis**: Commands analyzed as they're typed using `ExtractKeystrokeAnalysis`
- **Terminal Output Analysis**: Analyzes server output for suspicious patterns (errors, permission denied, etc.)
- **Tag-Based Rules**: Immediate termination on deny tag matches (regex patterns)
- **BAML Integration**: Uses BAML REST API functions for AI-powered threat detection
- **Automatic Termination**: Sessions terminated on high/critical threats
- **Command History**: Builds command history from live keyboard input, maintains context
- **Session Summaries**: Generates summaries using `ExtractCommandSummary` from command history
- **Configurable**: Threat levels, auto-termination, logging thresholds, tag rules

## Modes of Operation

### Reactive Mode (Default)

Analyzes commands **after** they're sent to target. Can terminate session on high/critical threats.

```rust
params.insert("threat_detection_enabled", "true");
params.insert("threat_detection_auto_terminate", "true");
```

### Proactive Mode (Command Approval)

Buffers commands and requires AI approval **before** execution. Can block individual commands without terminating session.

```rust
params.insert("threat_detection_proactive_mode", "true");
params.insert("threat_detection_approval_timeout_ms", "2000");
params.insert("threat_detection_fail_closed_on_error", "false");
params.insert("threat_detection_auto_approve_safe_commands", "true");
```

**How it works:**
1. User types command
2. Keystrokes buffered until Enter
3. AI analyzes command
4. If approved → sent to target
5. If blocked → user sees error, command not sent

**Auto-approved commands** (no AI call): ls, pwd, cd, echo, cat, less, more, head, tail, grep, find, which, whoami, hostname, date, uptime, w, who, ps, top, df, du, free, uname, clear, history, exit, logout

### Stateful Mode (Full Context)

AI receives full terminal state for context-aware decisions:
- Screen contents (what user sees)
- Current directory
- Command history
- Recent output
- User behavior metrics (typing speed, corrections, patterns)

Enable with `terminal-state` feature:
```rust
// In handler initialization
let mut state_extractor = TerminalStateExtractor::new();

// Before approval request
let terminal_state = state_extractor.extract_state(&terminal, command, &history);
```

## Configuration

### Common Parameters (All Handlers)

Enable threat detection by providing these parameters:

**Basic Settings:**
- `threat_detection_baml_endpoint` - BAML REST API base endpoint URL (required)
  - Functions are appended: `/ExtractKeystrokeAnalysis` and `/ExtractCommandSummary`
  - Example: `http://localhost:8000/api`
- `threat_detection_baml_api_key` - Optional API key for BAML authentication (sent as `Authorization: Bearer {key}` header)
- `threat_detection_enabled` - Enable/disable threat detection (default: `false`)
- `threat_detection_auto_terminate` - Auto-terminate on threats in reactive mode (default: `true`)
- `threat_detection_min_log_level` - Minimum threat level to log (`None`, `Low`, `Medium`, `High`, `Critical`, default: `Low`)
- `threat_detection_command_history_size` - Command history size for context (default: `10`)
- `threat_detection_timeout_seconds` - Request timeout in seconds (default: `5`)

**Tag-Based Rules:**
- `threat_detection_enable_tag_checking` - Enable tag-based rules (default: `true`)
- `threat_detection_deny_tags` - Map of risk level → list of regex patterns for deny tags (immediate termination)
  - Example: `{"critical": ["rm -rf", "sudo.*rm"], "high": ["chmod.*777"]}`
- `threat_detection_allow_tags` - Map of risk level → list of regex patterns for allow tags (explicitly allowed)
  - Example: `{"low": ["ls", "pwd", "cd"]}`

**Proactive Mode Settings:**
- `threat_detection_proactive_mode` - Enable proactive mode (AI approval before execution) (default: `false`)
- `threat_detection_approval_timeout_ms` - Max time to wait for AI approval in milliseconds (default: `2000`)
- `threat_detection_fail_closed_on_error` - Block commands if AI unavailable (default: `false`)
- `threat_detection_show_approval_status` - Show approval status to user (default: `true`)
- `threat_detection_auto_approve_safe_commands` - Auto-approve safe commands (default: `true`)

### Example Configuration

```rust
let mut params = HashMap::new();
params.insert("hostname".to_string(), "example.com".to_string());
params.insert("username".to_string(), "user".to_string());
params.insert("password".to_string(), "pass".to_string());

// Enable threat detection
params.insert("threat_detection_baml_endpoint".to_string(), 
    "http://localhost:8000/api".to_string());
params.insert("threat_detection_baml_api_key".to_string(), 
    "your-api-key".to_string());
params.insert("threat_detection_enabled".to_string(), 
    "true".to_string());
params.insert("threat_detection_auto_terminate".to_string(), 
    "true".to_string());
params.insert("threat_detection_min_log_level".to_string(), 
    "medium".to_string());
```

## BAML REST API Integration

### BAML Functions

The system uses two BAML functions:

1. **`ExtractKeystrokeAnalysis`**: Analyzes individual commands/keystroke sequences
2. **`ExtractCommandSummary`**: Generates session summaries from command sequences

### ExtractKeystrokeAnalysis Request Format

HTTP POST to `{baml_endpoint}/ExtractKeystrokeAnalysis`:

**For Keyboard Input** (live keystroke sequence as typed):
```json
{
  "keystroke_sequence": "rm -rf /tmp\n"
}
```

**For Terminal Output** (server response text):
```json
{
  "keystroke_sequence": "rm: cannot remove '/tmp': Permission denied"
}
```

Note: The `keystroke_sequence` field accepts any text input - it's used for both keyboard input and terminal output analysis.

### ExtractKeystrokeAnalysis Response Format

BAML returns a structured response matching the BAML schema:

```json
{
  "analysis_report": [
    {
      "risk_level": "Critical",
      "risk_category": "DestructiveActivity",
      "reasoning": "Command deletes files irreversibly"
    }
  ],
  "overall_summary": "The user performed destructive file deletion"
}
```

### ExtractCommandSummary Request Format

HTTP POST to `{baml_endpoint}/ExtractCommandSummary`:

```json
{
  "command_sequence": ["ls", "cd /tmp", "rm -rf *"]
}
```

### ExtractCommandSummary Response Format

```json
{
  "overall_summary": "The user navigated to a temporary directory and deleted all files"
}
```

### Threat Level Mapping

The system maps BAML's `risk_level` strings to `ThreatLevel` enum:
- `"Critical"` → `ThreatLevel::Critical` → Terminate
- `"High"` → `ThreatLevel::High` → Terminate
- `"Medium"` → `ThreatLevel::Medium` → Warn
- `"Low"` → `ThreatLevel::Low` → Monitor
- Otherwise → `ThreatLevel::None` → Continue

### Tag-Based Rules

Tag-based rules provide immediate termination without waiting for BAML API calls:

- **Deny Tags**: Regex patterns that immediately terminate the session when matched
  - Checked before BAML API calls
  - Highest priority - if a deny tag matches, session terminates immediately
  - Example: `{"critical": ["rm -rf", "sudo.*rm"], "high": ["chmod.*777"]}`
- **Allow Tags**: Regex patterns that explicitly allow commands (bypass threat detection)
  - If an allow tag matches, the command is considered safe and no BAML call is made
  - Example: `{"low": ["ls", "pwd", "cd"]}`

Tag matching uses Rust's `regex` crate with case-insensitive matching.

## Protocol Handler Support

Threat detection works with **all protocol handlers**:

- ✅ **SSH** - Full support (reactive, proactive, stateful)
- ✅ **Telnet** - Full support (reactive, proactive, stateful)
- ✅ **RDP** - Keyboard input analysis (reactive, proactive)
- ✅ **VNC** - Keyboard input analysis (reactive, proactive)
- ✅ **Database** - SQL query analysis (reactive, proactive)
- ✅ **SFTP** - File operation analysis (reactive, proactive)
- ✅ **RBI** - Browser command analysis (reactive, proactive)

All handlers use the same Guacamole protocol for input, so threat detection is protocol-agnostic.

## Usage

### Building with Threat Detection

Enable the `threat-detection` feature:

```bash
cargo build --features threat-detection
```

For terminal state extraction (stateful mode):

```bash
cargo build --features threat-detection,terminal-state
```

Or in `Cargo.toml`:

```toml
[dependencies]
guacr-threat-detection = { path = "../guacr-threat-detection", features = ["terminal-state"] }
```

### Programmatic Usage

**CRITICAL**: Must call `cleanup_session()` when session ends to prevent memory leak!

#### Reactive Mode

```rust
use guacr_threat_detection::{ThreatDetector, ThreatDetectorConfig, SessionGuard};
use std::sync::Arc;

let detector = Arc::new(ThreatDetector::new(config)?);
let session_id = uuid::Uuid::new_v4().to_string();

// Option 1: Use SessionGuard (RAII - automatic cleanup)
let _guard = SessionGuard::new(detector.clone(), session_id.clone());

// Analyze after command sent
let threat = detector.analyze_keystroke_sequence(
    &session_id,
    "rm -rf /",
    "username",
    "hostname",
    "ssh"
).await?;

if threat.should_terminate() {
    // Terminate session
}

// Guard automatically calls cleanup_session() when dropped

// Option 2: Manual cleanup (if not using guard)
// detector.cleanup_session(&session_id);
```

#### Proactive Mode

```rust
use guacr_threat_detection::{
    ApprovalManager, CommandBuffer, handle_proactive_input, ProactiveResult
};

let approval_manager = ApprovalManager::new(detector, timeout, fail_closed);
let mut command_buffer = CommandBuffer::new();

// Handle keystroke
match handle_proactive_input(
    &mut command_buffer,
    &bytes,
    keysym,
    ctrl_pressed,
    &approval_manager,
    &session_id,
    &username,
    &hostname,
    "ssh",
    auto_approve_safe,
).await {
    ProactiveResult::Approved(cmd) => {
        // Send to target
        socket.write_all(&cmd).await?;
    }
    ProactiveResult::Blocked { reason, .. } => {
        // Show error, don't send
        let error = format!("error,0.Command blocked: {};", reason);
        to_client.send(Bytes::from(error)).await?;
    }
    ProactiveResult::Buffered => {
        // Waiting for Enter key
    }
    ProactiveResult::Timeout => {
        // Handle timeout
    }
}
```

#### Stateful Mode (with Terminal State)

```rust
use guacr_threat_detection::{TerminalStateExtractor, TerminalStateContext};

let mut state_extractor = TerminalStateExtractor::new();

// Record keystrokes for behavior analysis
state_extractor.record_keystroke(is_backspace);

// On Enter key, extract state
let terminal_state = state_extractor.extract_state(
    &terminal,
    command_buffer.as_str(),
    &command_history,
);

// Request approval with full context
let decision = approval_manager.request_approval_with_state(
    &session_id,
    command_buffer.as_str(),
    &terminal_state,
    &username,
    &hostname,
    "ssh",
).await?;

// Terminal state includes:
// - screen_contents: Full terminal screen text
// - current_directory: Parsed from prompt
// - command_history: Recent commands
// - recent_output: Server responses
// - behavior: UserBehaviorMetrics (typing speed, corrections, patterns)
```

## Threat Detection Points

### 1. Keyboard Input Analysis (Live)

Every keystroke sequence is analyzed **before** being sent to the SSH server. The raw bytes from keyboard input are converted to a string and sent directly to BAML:

```rust
// In SSH handler - live keyboard input
if let Ok(keystroke_sequence) = String::from_utf8(bytes) {
    let threat = detector.analyze_keystroke_sequence(
        &session_id, 
        &keystroke_sequence, 
        username, 
        hostname, 
        "ssh"
    ).await?;
    
    if threat.should_terminate() {
        // Terminate session immediately
        break;
    }
}
```

**What gets sent**: The actual keystroke sequence as typed (e.g., `"ls -la\n"`, `"rm -rf /tmp\n"`)

### 2. Terminal Output Analysis (Live)

Terminal output from the SSH server is analyzed for suspicious patterns. The raw bytes are converted to text and sent to BAML:

```rust
// In SSH handler - live terminal output from server
let threat = detector.analyze_terminal_output(
    &session_id,
    &terminal_output_bytes,  // Raw bytes from russh::ChannelMsg::Data
    username,
    hostname,
    "ssh"
).await?;

if threat.should_terminate() {
    // Terminate session
    break;
}
```

**What gets sent**: The actual terminal output text (e.g., error messages, command results, permission denied messages)

**Key Difference**: Unlike the Python implementation that parses `.tys` files, this analyzes the **live data stream** directly from the SSH connection.

## Session Termination

When a high or critical threat is detected:

1. **Error Message Sent**: Client receives error message via Guacamole protocol
2. **Connection Closed**: SSH connection is immediately closed
3. **Session Cleanup**: Command history and resources are cleaned up
4. **Logging**: Critical threat is logged with full context

## Error Handling

- **API Errors**: Non-fatal - session continues (fail-open policy)
- **Timeout**: Request times out after configured duration
- **Network Errors**: Logged but don't block session

For production, consider implementing fail-closed policy for critical environments.

## Security Considerations

- **API Key Security**: Store API keys securely (environment variables, secrets manager)
- **Network Security**: Use HTTPS for BAML endpoint in production
- **Rate Limiting**: BAML API may have rate limits - consider batching
- **Privacy**: Command history sent to BAML - ensure compliance with data policies
- **Fail-Safe**: Decide on fail-open vs fail-closed policy for API errors
- **Tag Rules**: Validate regex patterns to prevent ReDoS attacks

## Performance

- **Async**: All BAML API calls are async and non-blocking
- **Timeout**: Configurable timeout prevents hanging sessions
- **Tag Checking**: Tag-based rules checked first (no API call needed)
- **Batching**: Command history provides context without multiple API calls per command
- **Caching**: Consider caching results for repeated commands

## Example BAML Endpoint Implementation

Your BAML endpoint should implement the two functions:

### ExtractKeystrokeAnalysis Endpoint

```python
from flask import Flask, request, jsonify

app = Flask(__name__)

@app.route('/api/ExtractKeystrokeAnalysis', methods=['POST'])
def extract_keystroke_analysis():
    data = request.json
    keystroke_sequence = data.get('keystroke_sequence', '')
    
    # Call BAML function (or your LLM)
    # This is a simplified example - actual BAML integration uses baml-py client
    
    if 'rm -rf' in keystroke_sequence or 'sudo rm' in keystroke_sequence:
        return jsonify({
            "analysis_report": [{
                "risk_level": "Critical",
                "risk_category": "DestructiveActivity",
                "reasoning": "Command deletes files irreversibly"
            }],
            "overall_summary": "The user performed destructive file deletion"
        })
    elif 'cat /etc/shadow' in keystroke_sequence:
        return jsonify({
            "analysis_report": [{
                "risk_level": "High",
                "risk_category": "DataExfiltration",
                "reasoning": "Accessing sensitive password file"
            }],
            "overall_summary": "The user accessed sensitive system files"
        })
    else:
        return jsonify({
            "analysis_report": [{
                "risk_level": "Low",
                "risk_category": "RoutineOperations",
                "reasoning": "Standard command execution"
            }],
            "overall_summary": "The user performed routine operations"
        })

@app.route('/api/ExtractCommandSummary', methods=['POST'])
def extract_command_summary():
    data = request.json
    command_sequence = data.get('command_sequence', [])
    
    # Generate summary from command sequence
    return jsonify({
        "overall_summary": f"The user executed {len(command_sequence)} commands"
    })

if __name__ == '__main__':
    app.run(port=8000)
```

**Note**: For production, use the official BAML Python client (`baml-py`) which handles the actual LLM calls and response parsing according to your BAML schema definitions (`risk_analysis.baml` and `session_summary.baml`).

## Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_threat_detection() {
        let config = ThreatDetectorConfig {
            baml_endpoint: "http://localhost:8000/api".to_string(),
            enabled: true,
            ..Default::default()
        };
        
        let detector = ThreatDetector::new(config).unwrap();
        let threat = detector.analyze("session-1", "rm -rf /", "user", "host", "ssh").await.unwrap();
        
        assert!(threat.is_threat());
        assert!(threat.should_terminate());
    }
}
```

## Implementation in Protocol Handlers

All protocol handlers can use the same threat detection code since they all use Guacamole protocol:

```rust
// In any handler (SSH, Telnet, RDP, VNC, Database, SFTP, RBI)
if let Some(key_event) = parse_key_instruction(&msg_str) {
    let bytes = convert_keysym_to_bytes(key_event.keysym, ...);
    
    if let Some(ref manager) = approval_manager {
        // Proactive mode - all handlers use same code
        match handle_proactive_input(...).await {
            ProactiveResult::Approved(cmd) => send_to_target(cmd),
            ProactiveResult::Blocked { reason, .. } => show_error(reason),
            // ... handle other cases
        }
    } else {
        // Reactive mode - all handlers use same code
        send_to_target(bytes);
        if let Some(ref detector) = threat_detector {
            let threat = detector.analyze_keystroke_sequence(...).await?;
            if threat.should_terminate() {
                terminate_session();
            }
        }
    }
}
```

## Advantages Over Gateway Detection

| Feature | Gateway (Python) | pam-guacr (Rust) |
|---------|-----------------|------------------|
| **Data Source** | `.tys` log files | Live Guacamole protocol |
| **Timing** | Post-session | Pre-execution |
| **Can Block** | No | Yes (per-command) |
| **Terminal State** | No | Yes (full screen buffer) |
| **User Behavior** | No | Yes (typing speed, patterns) |
| **Interactive** | No | Yes (can guide users) |
| **Protocol Support** | SSH only | All protocols |
| **Context** | Command text only | Full terminal context |

## Future Enhancements

- [ ] Command caching (same command in same session)
- [ ] User profiles (learn per-user patterns)
- [ ] Batch approval (approve scripts at once)
- [ ] Client-side indicator (show "Checking..." in browser)
- [ ] Integration with SIEM systems
- [ ] Threat scoring aggregation over time
