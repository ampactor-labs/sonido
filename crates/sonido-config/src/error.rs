//! Error types for configuration operations.

use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during configuration operations.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Failed to read a file
    #[error("failed to read file '{path}': {source}")]
    ReadFile {
        /// Path of the file that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to write a file
    #[error("failed to write file '{path}': {source}")]
    WriteFile {
        /// Path of the file that could not be written.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse TOML
    #[error("failed to parse TOML: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// Failed to serialize TOML
    #[error("failed to serialize TOML: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    /// Preset not found
    #[error("preset not found: {0}")]
    PresetNotFound(String),

    /// Unknown effect type
    #[error("unknown effect type: {0}")]
    UnknownEffect(String),

    /// Invalid parameter
    #[error("invalid parameter '{param}' for effect '{effect}': {reason}")]
    InvalidParameter {
        /// Name of the effect containing the invalid parameter.
        effect: String,
        /// Name of the invalid parameter.
        param: String,
        /// Description of why the parameter is invalid.
        reason: String,
    },

    /// Validation errors
    #[error("validation failed: {0}")]
    Validation(#[from] crate::validation::ValidationError),

    /// Failed to create directory
    #[error("failed to create directory '{path}': {source}")]
    CreateDir {
        /// Path of the directory that could not be created.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl ConfigError {
    /// Create a read file error.
    pub fn read_file(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        ConfigError::ReadFile {
            path: path.into(),
            source,
        }
    }

    /// Create a write file error.
    pub fn write_file(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        ConfigError::WriteFile {
            path: path.into(),
            source,
        }
    }

    /// Create a create directory error.
    pub fn create_dir(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        ConfigError::CreateDir {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    fn mock_io_err() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::NotFound, "mock")
    }

    // --- factory methods ---

    #[test]
    fn read_file_factory_produces_correct_variant() {
        let err = ConfigError::read_file("/some/path", mock_io_err());
        assert!(
            matches!(err, ConfigError::ReadFile { ref path, .. } if path == std::path::Path::new("/some/path"))
        );
    }

    #[test]
    fn write_file_factory_produces_correct_variant() {
        let err = ConfigError::write_file("/out/path", mock_io_err());
        assert!(
            matches!(err, ConfigError::WriteFile { ref path, .. } if path == std::path::Path::new("/out/path"))
        );
    }

    #[test]
    fn create_dir_factory_produces_correct_variant() {
        let err = ConfigError::create_dir("/dir/path", mock_io_err());
        assert!(
            matches!(err, ConfigError::CreateDir { ref path, .. } if path == std::path::Path::new("/dir/path"))
        );
    }

    // --- Display formatting ---

    #[test]
    fn read_file_display() {
        let err = ConfigError::read_file("/a/b.toml", mock_io_err());
        let msg = err.to_string();
        assert!(msg.contains("failed to read file"), "got: {msg}");
        assert!(msg.contains("/a/b.toml"), "got: {msg}");
    }

    #[test]
    fn write_file_display() {
        let err = ConfigError::write_file("/a/b.toml", mock_io_err());
        let msg = err.to_string();
        assert!(msg.contains("failed to write file"), "got: {msg}");
        assert!(msg.contains("/a/b.toml"), "got: {msg}");
    }

    #[test]
    fn create_dir_display() {
        let err = ConfigError::create_dir("/a/b", mock_io_err());
        let msg = err.to_string();
        assert!(msg.contains("failed to create directory"), "got: {msg}");
        assert!(msg.contains("/a/b"), "got: {msg}");
    }

    #[test]
    fn preset_not_found_display() {
        let err = ConfigError::PresetNotFound("my-preset".to_string());
        let msg = err.to_string();
        assert_eq!(msg, "preset not found: my-preset");
    }

    #[test]
    fn unknown_effect_display() {
        let err = ConfigError::UnknownEffect("super_fuzz".to_string());
        let msg = err.to_string();
        assert_eq!(msg, "unknown effect type: super_fuzz");
    }

    #[test]
    fn invalid_parameter_display() {
        let err = ConfigError::InvalidParameter {
            effect: "distortion".to_string(),
            param: "drive".to_string(),
            reason: "out of range".to_string(),
        };
        let msg = err.to_string();
        assert_eq!(
            msg,
            "invalid parameter 'drive' for effect 'distortion': out of range"
        );
    }

    // --- Error::source() chain for I/O-wrapping variants ---

    #[test]
    fn read_file_source_is_some() {
        let err = ConfigError::read_file("/x", mock_io_err());
        assert!(err.source().is_some(), "ReadFile must expose I/O source");
    }

    #[test]
    fn write_file_source_is_some() {
        let err = ConfigError::write_file("/x", mock_io_err());
        assert!(err.source().is_some(), "WriteFile must expose I/O source");
    }

    #[test]
    fn create_dir_source_is_some() {
        let err = ConfigError::create_dir("/x", mock_io_err());
        assert!(err.source().is_some(), "CreateDir must expose I/O source");
    }

    #[test]
    fn preset_not_found_source_is_none() {
        let err = ConfigError::PresetNotFound("p".to_string());
        assert!(err.source().is_none());
    }

    #[test]
    fn unknown_effect_source_is_none() {
        let err = ConfigError::UnknownEffect("e".to_string());
        assert!(err.source().is_none());
    }

    #[test]
    fn invalid_parameter_source_is_none() {
        let err = ConfigError::InvalidParameter {
            effect: "e".to_string(),
            param: "p".to_string(),
            reason: "r".to_string(),
        };
        assert!(err.source().is_none());
    }
}
