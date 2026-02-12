// Recording configuration parsed from connection parameters.
//
// Supports both hyphenated ("recording-path") and non-separator ("recordingpath")
// parameter names. The Python gateway sends the latter while the external guacd
// path uses the former.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Look up a parameter supporting both hyphenated ("recording-path") and
/// non-separator ("recordingpath") forms.
pub fn get_param<'a>(params: &'a HashMap<String, String>, canonical: &str) -> Option<&'a String> {
    params
        .get(canonical)
        .or_else(|| params.get(&canonical.replace('-', "")))
}

/// Parse a boolean parameter value ("true" or "1").
pub fn parse_bool(value: Option<&String>) -> bool {
    value.map(|v| v == "true" || v == "1").unwrap_or(false)
}

/// Find a unique file path by appending .1, .2, ... .N suffixes.
/// Returns the first path that does not already exist, or None if all
/// candidates up to `max_suffix` are taken.
pub fn find_unique_path(base: &Path, max_suffix: u32) -> Option<PathBuf> {
    if !base.exists() {
        return Some(base.to_path_buf());
    }
    let name = base.file_name()?.to_str()?;
    for i in 1..=max_suffix {
        let candidate = base.with_file_name(format!("{}.{}", name, i));
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Recording format types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecordingFormat {
    /// Guacamole .ses format - binary protocol recording
    /// Compatible with Guacamole's guaclog/guacplay utilities
    GuacamoleSes,

    /// Asciicast v2 format - JSON terminal recording
    /// Compatible with asciinema player
    Asciicast,

    /// Typescript format - legacy text logging
    /// Simple text file with timing information
    Typescript,
}

impl RecordingFormat {
    /// Get file extension for this format
    pub fn extension(&self) -> &str {
        match self {
            RecordingFormat::GuacamoleSes => "ses",
            RecordingFormat::Asciicast => "cast",
            RecordingFormat::Typescript => "typescript",
        }
    }
}

/// Recording configuration parsed from connection parameters
///
/// Based on KCM's GUAC_DB_CLIENT_ARGS from settings.c:
/// - recording-path: Directory to store recordings
/// - recording-name: Filename template (supports variables)
/// - recording-exclude-output: Exclude graphical output
/// - recording-exclude-mouse: Exclude mouse movements
/// - recording-include-keys: Include keyboard input (security risk!)
/// - create-recording-path: Create directory if missing
/// - recording-write-existing: Allow overwriting existing files
///
/// Plus typescript-specific parameters:
/// - typescript-path: Directory for typescript files
/// - typescript-name: Filename for typescript
/// - create-typescript-path: Create directory if missing
/// - typescript-write-existing: Allow overwriting
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    // ========================================================================
    // Guacamole Recording (.ses format)
    // ========================================================================
    /// Path to store Guacamole recording files
    pub recording_path: Option<String>,

    /// Recording filename (supports templates: ${GUAC_DATE}, ${GUAC_TIME}, ${GUAC_USERNAME})
    pub recording_name: String,

    /// Create recording path if it doesn't exist
    pub create_recording_path: bool,

    /// Allow overwriting existing recording files
    pub recording_write_existing: bool,

    /// Exclude graphical output from recording (reduces file size)
    pub recording_exclude_output: bool,

    /// Exclude mouse movements from recording
    pub recording_exclude_mouse: bool,

    /// Include key events in recording
    /// WARNING: May capture passwords - default is false
    pub recording_include_keys: bool,

    // ========================================================================
    // Typescript Recording (text format)
    // ========================================================================
    /// Path to store typescript files
    pub typescript_path: Option<String>,

    /// Typescript filename
    pub typescript_name: String,

    /// Create typescript path if it doesn't exist
    pub create_typescript_path: bool,

    /// Allow overwriting existing typescript files
    pub typescript_write_existing: bool,

    // ========================================================================
    // Asciicast Recording (.cast format)
    // ========================================================================
    /// Path to store asciicast files (uses recording_path if not set)
    pub asciicast_path: Option<String>,

    /// Asciicast filename (uses recording_name with .cast extension if not set)
    pub asciicast_name: Option<String>,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            recording_path: None,
            recording_name: "recording".to_string(),
            create_recording_path: false,
            recording_write_existing: false,
            recording_exclude_output: false,
            recording_exclude_mouse: false,
            recording_include_keys: false,
            typescript_path: None,
            typescript_name: "typescript".to_string(),
            create_typescript_path: false,
            typescript_write_existing: false,
            asciicast_path: None,
            asciicast_name: None,
        }
    }
}

impl RecordingConfig {
    /// Parse recording configuration from connection parameters.
    ///
    /// Supports both hyphenated ("recording-path") and non-separator
    /// ("recordingpath") parameter names. The Python gateway sends the
    /// non-hyphenated form while the external guacd path uses hyphens.
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        // Check boolean enable flags that Python may send
        let recording_enabled = parse_bool(
            get_param(params, "recording-enabled").or(get_param(params, "enable-recording")),
        );
        let typescript_enabled = parse_bool(get_param(params, "typescript-recording-enabled"));

        // If the enable flag is explicitly false, ignore the path
        let recording_path = get_param(params, "recording-path").cloned().and_then(|p| {
            // An explicit "false" enable flag overrides the path
            if get_param(params, "recording-enabled").is_some() && !recording_enabled {
                None
            } else {
                Some(p)
            }
        });

        let typescript_path = get_param(params, "typescript-path").cloned().and_then(|p| {
            if get_param(params, "typescript-recording-enabled").is_some() && !typescript_enabled {
                None
            } else {
                Some(p)
            }
        });

        Self {
            recording_path,
            recording_name: get_param(params, "recording-name")
                .cloned()
                .unwrap_or_else(|| "recording".to_string()),
            create_recording_path: parse_bool(get_param(params, "create-recording-path")),
            recording_write_existing: parse_bool(get_param(params, "recording-write-existing")),
            recording_exclude_output: parse_bool(get_param(params, "recording-exclude-output")),
            recording_exclude_mouse: parse_bool(get_param(params, "recording-exclude-mouse")),
            recording_include_keys: parse_bool(get_param(params, "recording-include-keys")),
            typescript_path,
            typescript_name: get_param(params, "typescript-name")
                .cloned()
                .unwrap_or_else(|| "typescript".to_string()),
            create_typescript_path: parse_bool(get_param(params, "create-typescript-path")),
            typescript_write_existing: parse_bool(get_param(params, "typescript-write-existing")),
            asciicast_path: get_param(params, "asciicast-path").cloned(),
            asciicast_name: get_param(params, "asciicast-name").cloned(),
        }
    }

    /// Check if any recording is enabled
    pub fn is_enabled(&self) -> bool {
        self.recording_path.is_some()
            || self.typescript_path.is_some()
            || self.asciicast_path.is_some()
    }

    /// Check if Guacamole .ses recording is enabled
    pub fn is_ses_enabled(&self) -> bool {
        self.recording_path.is_some()
    }

    /// Check if typescript recording is enabled
    pub fn is_typescript_enabled(&self) -> bool {
        self.typescript_path.is_some()
    }

    /// Check if asciicast recording is enabled
    pub fn is_asciicast_enabled(&self) -> bool {
        self.asciicast_path.is_some() || self.recording_path.is_some()
    }

    /// Expand filename template with actual values
    ///
    /// Supports:
    /// - ${GUAC_DATE}: Current date (YYYYMMDD)
    /// - ${GUAC_TIME}: Current time (HHMMSS)
    /// - ${GUAC_USERNAME}: Username from connection
    /// - ${GUAC_HOSTNAME}: Hostname from connection
    /// - ${GUAC_PROTOCOL}: Protocol name
    pub fn expand_filename(
        template: &str,
        params: &HashMap<String, String>,
        protocol: &str,
    ) -> String {
        let now = chrono::Utc::now();
        let date = now.format("%Y%m%d").to_string();
        let time = now.format("%H%M%S").to_string();

        let username = params.get("username").cloned().unwrap_or_default();
        let hostname = params.get("hostname").cloned().unwrap_or_default();

        template
            .replace("${GUAC_DATE}", &date)
            .replace("${GUAC_TIME}", &time)
            .replace("${GUAC_USERNAME}", &username)
            .replace("${GUAC_HOSTNAME}", &hostname)
            .replace("${GUAC_PROTOCOL}", protocol)
    }

    /// Get full path for Guacamole .ses recording.
    ///
    /// If the recording name already contains a file extension (e.g. Python
    /// gateway appends ".ses"), no additional extension is added.
    pub fn get_ses_path(
        &self,
        params: &HashMap<String, String>,
        protocol: &str,
    ) -> Option<PathBuf> {
        self.recording_path.as_ref().map(|dir| {
            let filename = Self::expand_filename(&self.recording_name, params, protocol);
            if filename.contains('.') {
                PathBuf::from(dir).join(filename)
            } else {
                PathBuf::from(dir).join(format!("{}.ses", filename))
            }
        })
    }

    /// Get full path for typescript recording
    pub fn get_typescript_path(
        &self,
        params: &HashMap<String, String>,
        protocol: &str,
    ) -> Option<PathBuf> {
        self.typescript_path.as_ref().map(|dir| {
            let filename = Self::expand_filename(&self.typescript_name, params, protocol);
            PathBuf::from(dir).join(filename)
        })
    }

    /// Get full path for asciicast recording.
    ///
    /// If the filename already has an extension, no additional extension is added.
    pub fn get_asciicast_path(
        &self,
        params: &HashMap<String, String>,
        protocol: &str,
    ) -> Option<PathBuf> {
        let base_path = self
            .asciicast_path
            .as_ref()
            .or(self.recording_path.as_ref())?;
        let name = self.asciicast_name.as_ref().unwrap_or(&self.recording_name);
        let filename = Self::expand_filename(name, params, protocol);
        if filename.contains('.') {
            // Caller-provided name already has extension -- use a .cast sibling
            // by replacing the extension
            let stem = filename
                .rsplit_once('.')
                .map(|(s, _)| s)
                .unwrap_or(&filename);
            Some(PathBuf::from(base_path).join(format!("{}.cast", stem)))
        } else {
            Some(PathBuf::from(base_path).join(format!("{}.cast", filename)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_config_default() {
        let config = RecordingConfig::default();
        assert!(!config.is_enabled());
        assert_eq!(config.recording_name, "recording");
        assert!(!config.recording_include_keys);
    }

    #[test]
    fn test_recording_config_from_params() {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), "/tmp/recordings".to_string());
        params.insert(
            "recording-name".to_string(),
            "session-${GUAC_DATE}".to_string(),
        );
        params.insert("recording-include-keys".to_string(), "true".to_string());
        params.insert("create-recording-path".to_string(), "1".to_string());

        let config = RecordingConfig::from_params(&params);

        assert!(config.is_enabled());
        assert!(config.is_ses_enabled());
        assert_eq!(config.recording_path, Some("/tmp/recordings".to_string()));
        assert!(config.recording_include_keys);
        assert!(config.create_recording_path);
    }

    #[test]
    fn test_param_normalization_recordingpath() {
        // Python gateway sends "recordingpath" (no hyphens)
        let mut params = HashMap::new();
        params.insert("recordingpath".to_string(), "/tmp/recordings".to_string());
        params.insert("recordingname".to_string(), "test-session".to_string());
        params.insert("createrecordingpath".to_string(), "true".to_string());

        let config = RecordingConfig::from_params(&params);

        assert!(config.is_enabled());
        assert_eq!(config.recording_path, Some("/tmp/recordings".to_string()));
        assert_eq!(config.recording_name, "test-session");
        assert!(config.create_recording_path);
    }

    #[test]
    fn test_no_double_ses_extension() {
        // Python gateway appends .ses to the name
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), "/recordings".to_string());
        params.insert("recording-name".to_string(), "session.ses".to_string());

        let config = RecordingConfig::from_params(&params);
        let ses_path = config.get_ses_path(&params, "ssh");

        // Should NOT produce "session.ses.ses"
        assert_eq!(ses_path, Some(PathBuf::from("/recordings/session.ses")));
    }

    #[test]
    fn test_no_double_cast_extension() {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), "/recordings".to_string());
        params.insert("recording-name".to_string(), "session.ses".to_string());

        let config = RecordingConfig::from_params(&params);
        let cast_path = config.get_asciicast_path(&params, "ssh");

        // Should produce "session.cast" not "session.ses.cast"
        assert_eq!(cast_path, Some(PathBuf::from("/recordings/session.cast")));
    }

    #[test]
    fn test_unique_path_base_does_not_exist() {
        let tmp = std::env::temp_dir().join("guacr-recording-test-unique-nonexist.ses");
        // Clean up in case of leftover
        let _ = std::fs::remove_file(&tmp);

        let result = find_unique_path(&tmp, 255);
        assert_eq!(result, Some(tmp));
    }

    #[test]
    fn test_unique_path_with_existing_file() {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let base = tmp_dir.path().join("test.ses");
        std::fs::File::create(&base).unwrap();

        let result = find_unique_path(&base, 255);
        assert_eq!(result, Some(tmp_dir.path().join("test.ses.1")));
    }

    #[test]
    fn test_filename_expansion() {
        let mut params = HashMap::new();
        params.insert("username".to_string(), "testuser".to_string());
        params.insert("hostname".to_string(), "server.example.com".to_string());

        let template = "${GUAC_USERNAME}-${GUAC_HOSTNAME}-${GUAC_PROTOCOL}";
        let result = RecordingConfig::expand_filename(template, &params, "ssh");

        assert_eq!(result, "testuser-server.example.com-ssh");
    }

    #[test]
    fn test_recording_format_extension() {
        assert_eq!(RecordingFormat::GuacamoleSes.extension(), "ses");
        assert_eq!(RecordingFormat::Asciicast.extension(), "cast");
        assert_eq!(RecordingFormat::Typescript.extension(), "typescript");
    }

    #[test]
    fn test_get_recording_paths() {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), "/recordings".to_string());
        params.insert("recording-name".to_string(), "session".to_string());

        let config = RecordingConfig::from_params(&params);

        let ses_path = config.get_ses_path(&params, "ssh");
        assert_eq!(ses_path, Some(PathBuf::from("/recordings/session.ses")));

        let cast_path = config.get_asciicast_path(&params, "ssh");
        assert_eq!(cast_path, Some(PathBuf::from("/recordings/session.cast")));
    }

    #[test]
    fn test_recording_path_with_template() {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), "/recordings".to_string());
        params.insert(
            "recording-name".to_string(),
            "${GUAC_USERNAME}-${GUAC_HOSTNAME}".to_string(),
        );
        params.insert("username".to_string(), "admin".to_string());
        params.insert("hostname".to_string(), "server1".to_string());

        let config = RecordingConfig::from_params(&params);

        let ses_path = config.get_ses_path(&params, "ssh");
        assert_eq!(
            ses_path,
            Some(PathBuf::from("/recordings/admin-server1.ses"))
        );
    }

    #[test]
    fn test_recording_enabled_flag_false_disables() {
        let mut params = HashMap::new();
        params.insert("recording-path".to_string(), "/recordings".to_string());
        params.insert("recording-enabled".to_string(), "false".to_string());

        let config = RecordingConfig::from_params(&params);
        assert!(!config.is_enabled());
        assert!(config.recording_path.is_none());
    }
}
