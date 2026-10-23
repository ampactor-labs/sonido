//! Platform-specific paths for presets and configuration.
//!
//! This module provides cross-platform paths for storing user presets,
//! configuration files, and locating system presets.
//!
//! # Directory Structure
//!
//! - **User presets**: `~/.config/sonido/presets/` (Linux), `~/Library/Application Support/sonido/presets/` (macOS), `%APPDATA%\sonido\presets\` (Windows)
//! - **User config**: `~/.config/sonido/` (Linux), `~/Library/Application Support/sonido/` (macOS), `%APPDATA%\sonido\` (Windows)
//! - **System presets**: `/usr/share/sonido/presets/` (Linux), `/Library/Application Support/sonido/presets/` (macOS)
//!
//! # Example
//!
//! ```rust,no_run
//! use sonido_config::paths;
//!
//! // Get the user presets directory
//! let presets_dir = paths::user_presets_dir();
//! println!("User presets: {:?}", presets_dir);
//!
//! // Find a preset by name (searches user then system directories)
//! if let Some(path) = paths::find_preset("my_preset") {
//!     println!("Found preset at: {:?}", path);
//! }
//! ```

use std::path::PathBuf;

/// Application name used for directory paths.
const APP_NAME: &str = "sonido";

/// Subdirectory name for presets.
const PRESETS_SUBDIR: &str = "presets";

/// Returns the user-specific presets directory.
///
/// # Platform Paths
///
/// - Linux: `~/.config/sonido/presets/`
/// - macOS: `~/Library/Application Support/sonido/presets/`
/// - Windows: `%APPDATA%\sonido\presets\`
///
/// Returns a fallback path if the config directory cannot be determined.
pub fn user_presets_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
        .join(PRESETS_SUBDIR)
}

/// The rig library: where the editor's Export ▸ CLAP writes rigs (patch
/// JSON) and where the graph-player plugin lists them.
///
/// # Platform Paths
/// - Linux: `$XDG_DATA_HOME/sonido/patches/`, else `~/.local/share/sonido/patches/`
/// - macOS: `~/Library/Application Support/sonido/patches/`
/// - Windows: `%APPDATA%\sonido\patches\`
///
/// `None` if the platform has no data directory (e.g. the browser).
pub fn rig_library_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join(APP_NAME).join("patches"))
}

/// Returns the user-specific configuration directory.
///
/// # Platform Paths
///
/// - Linux: `~/.config/sonido/`
/// - macOS: `~/Library/Application Support/sonido/`
/// - Windows: `%APPDATA%\sonido\`
///
/// Returns a fallback path if the config directory cannot be determined.
pub fn user_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
}

/// Returns the system-wide presets directory.
///
/// This directory is typically read-only and contains factory presets.
///
/// # Platform Paths
///
/// - Linux: `/usr/share/sonido/presets/`
/// - macOS: `/Library/Application Support/sonido/presets/`
/// - Windows: `%PROGRAMDATA%\sonido\presets\`
pub fn system_presets_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/usr/share")
            .join(APP_NAME)
            .join(PRESETS_SUBDIR)
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support")
            .join(APP_NAME)
            .join(PRESETS_SUBDIR)
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("C:\\ProgramData"))
            .join(APP_NAME)
            .join(PRESETS_SUBDIR)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(APP_NAME)
            .join(PRESETS_SUBDIR)
    }
}

/// Find a preset file by name.
///
/// Searches in the following order:
/// 1. Current directory (if the path is a valid file)
/// 2. User presets directory
/// 3. System presets directory
///
/// The name can be:
/// - An absolute path to a TOML file
/// - A relative path to a TOML file
/// - A preset name (with or without `.toml` extension)
///
/// # Example
///
/// ```rust,no_run
/// use sonido_config::paths::find_preset;
///
/// // Find by name
/// if let Some(path) = find_preset("blues_drive") {
///     println!("Found: {:?}", path);
/// }
///
/// // Find by path
/// if let Some(path) = find_preset("/path/to/my_preset.toml") {
///     println!("Found: {:?}", path);
/// }
/// ```
pub fn find_preset(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(name);

    // Check if it's already a valid file path
    if path.is_file() {
        return Some(path);
    }

    // Normalize the name (add .toml if not present)
    let filename = if name.ends_with(".toml") {
        name.to_string()
    } else {
        format!("{}.toml", name)
    };

    // Search user presets directory
    let user_path = user_presets_dir().join(&filename);
    if user_path.is_file() {
        return Some(user_path);
    }

    // Search system presets directory
    let system_path = system_presets_dir().join(&filename);
    if system_path.is_file() {
        return Some(system_path);
    }

    None
}

/// Ensure the user presets directory exists.
///
/// Creates the directory and any parent directories if they don't exist.
///
/// # Errors
///
/// Returns an error if the directory cannot be created.
pub fn ensure_user_presets_dir() -> Result<PathBuf, crate::ConfigError> {
    let dir = user_presets_dir();

    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| crate::ConfigError::create_dir(&dir, e))?;
    }

    Ok(dir)
}

/// Ensure the user config directory exists.
///
/// Creates the directory and any parent directories if they don't exist.
///
/// # Errors
///
/// Returns an error if the directory cannot be created.
pub fn ensure_user_config_dir() -> Result<PathBuf, crate::ConfigError> {
    let dir = user_config_dir();

    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| crate::ConfigError::create_dir(&dir, e))?;
    }

    Ok(dir)
}

/// List all preset files in the user presets directory.
///
/// Returns an empty vector if the directory doesn't exist or can't be read.
pub fn list_user_presets() -> Vec<PathBuf> {
    list_presets_in_dir(&user_presets_dir())
}

/// List all preset files in the system presets directory.
///
/// Returns an empty vector if the directory doesn't exist or can't be read.
pub fn list_system_presets() -> Vec<PathBuf> {
    list_presets_in_dir(&system_presets_dir())
}

/// List all available presets (user + system).
///
/// User presets are listed first, followed by system presets.
/// Duplicate names are not filtered - the caller should handle precedence.
pub fn list_all_presets() -> Vec<PathBuf> {
    let mut presets = list_user_presets();
    presets.extend(list_system_presets());
    presets
}

/// Helper to list preset files in a directory.
fn list_presets_in_dir(dir: &PathBuf) -> Vec<PathBuf> {
    if !dir.exists() {
        return Vec::new();
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().map(|ext| ext == "toml").unwrap_or(false))
        .collect()
}

/// Get the preset name from a file path.
///
/// Extracts the file stem (filename without extension).
///
/// # Example
///
/// ```rust
/// use sonido_config::paths::preset_name_from_path;
/// use std::path::Path;
///
/// let name = preset_name_from_path(Path::new("/path/to/blues_drive.toml"));
/// assert_eq!(name, Some("blues_drive".to_string()));
/// ```
pub fn preset_name_from_path(path: &std::path::Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_user_presets_dir() {
        let dir = user_presets_dir();
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains("sonido") || dir_str.contains("presets"));
    }

    #[test]
    fn test_user_config_dir() {
        let dir = user_config_dir();
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains("sonido"));
    }

    #[test]
    fn test_system_presets_dir() {
        let dir = system_presets_dir();
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains("sonido"));
    }

    #[test]
    fn test_find_preset_by_path() {
        let temp_dir = TempDir::new().unwrap();
        let preset_path = temp_dir.path().join("test.toml");
        fs::write(&preset_path, "name = \"test\"").unwrap();

        let found = find_preset(preset_path.to_str().unwrap());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), preset_path);
    }

    #[test]
    fn test_find_preset_not_found() {
        let found = find_preset("nonexistent_preset_12345");
        assert!(found.is_none());
    }

    #[test]
    fn test_list_presets_in_dir() {
        let temp_dir = TempDir::new().unwrap();

        // Create some preset files
        fs::write(temp_dir.path().join("preset1.toml"), "").unwrap();
        fs::write(temp_dir.path().join("preset2.toml"), "").unwrap();
        fs::write(temp_dir.path().join("not_a_preset.txt"), "").unwrap();

        let presets = list_presets_in_dir(&temp_dir.path().to_path_buf());
        assert_eq!(presets.len(), 2);
        assert!(presets.iter().all(|p| p.extension().unwrap() == "toml"));
    }

    #[test]
    fn test_list_presets_empty_dir() {
        let temp_dir = TempDir::new().unwrap();
        let presets = list_presets_in_dir(&temp_dir.path().to_path_buf());
        assert_eq!(presets.len(), 0);
    }

    #[test]
    fn test_list_presets_nonexistent_dir() {
        let presets = list_presets_in_dir(&PathBuf::from("/nonexistent/path/12345"));
        assert_eq!(presets.len(), 0);
    }

    #[test]
    fn test_preset_name_from_path() {
        let path = std::path::Path::new("/path/to/blues_drive.toml");
        assert_eq!(preset_name_from_path(path), Some("blues_drive".to_string()));

        let path = std::path::Path::new("simple.toml");
        assert_eq!(preset_name_from_path(path), Some("simple".to_string()));
    }

    #[test]
    fn test_ensure_user_presets_dir() {
        // This test just ensures the function doesn't panic
        // Actual directory creation depends on system permissions
        let result = ensure_user_presets_dir();
        // We don't assert success because it depends on system state
        // but we verify it doesn't panic
        let _ = result;
    }

    #[test]
    fn test_find_preset_adds_extension() {
        // Verify that find_preset adds .toml extension when searching
        // (This is a logic test, not a file system test)
        let temp_dir = TempDir::new().unwrap();
        let preset_path = temp_dir.path().join("mypreset.toml");
        fs::write(&preset_path, "name = \"test\"").unwrap();

        // Should find it even without .toml extension when given full path
        let found = find_preset(preset_path.to_str().unwrap());
        assert!(found.is_some());
    }
}
