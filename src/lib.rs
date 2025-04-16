use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod make_wrapper;
pub mod parser;

#[derive(Debug, Error)]
pub enum CompileDbError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Invalid command: {0}")]
    InvalidCommand(String),

    #[error("Make execution failed: {0}")]
    MakeError(String),
}

/// Represents a single compilation command in the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileCommand {
    /// The working directory for the compilation
    pub directory: String,

    /// The main source file to compile
    pub file: String,

    /// The command as a single string (when command_style is true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// The command as a list of arguments (when command_style is false)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<String>>,

    /// Optional output file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Configuration for the compilation database generator
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the build log file
    pub build_log: Option<PathBuf>,

    /// Output file path
    pub output_file: PathBuf,

    /// Initial build directory
    pub build_dir: PathBuf,

    /// File exclusion patterns
    pub exclude_patterns: Vec<String>,

    /// Skip actual build
    pub no_build: bool,

    /// Enable verbose output
    pub verbose: u8,

    /// Skip source file existence check
    pub no_strict: bool,

    /// Predefined compiler macros
    pub macros: Vec<String>,

    /// Use command style output
    pub command_style: bool,

    /// Use full compiler path
    pub full_path: bool,

    /// Regex pattern for compile commands
    pub regex_compile: String,

    /// Regex pattern for source files
    pub regex_file: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            build_log: None,
            output_file: PathBuf::from("compile_commands.json"),
            build_dir: std::env::current_dir().unwrap_or_default(),
            exclude_patterns: Vec::new(),
            no_build: false,
            verbose: 0,
            no_strict: false,
            macros: Vec::new(),
            command_style: false,
            full_path: false,
            regex_compile: String::from(
                r"(?:[^/]*/)*(gcc|clang|cc|g\+\+|c\+\+|clang\+\+|cl)(?:-[0-9\.]+)?(?:\s|$)",
            ),
            regex_file: String::from(r"\s-c\s+(\S+\.(c|cpp|cc|cxx|c\+\+|s|m|mm|cu))\s+-o\s"),
        }
    }
}

/// Main interface for generating compilation database
pub trait CompileDbGenerator {
    /// Generate compilation database from build log
    fn generate(&self, config: &Config) -> Result<Vec<CompileCommand>, CompileDbError>;

    /// Write compilation database to file
    fn write_to_file(&self, commands: &[CompileCommand], path: &Path)
        -> Result<(), CompileDbError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(!config.no_build);
        assert!(config.verbose == 0);
        assert!(!config.no_strict);
        assert!(!config.command_style);
        assert!(!config.full_path);
    }

    #[test]
    fn test_compile_command_serialization() {
        let cmd = CompileCommand {
            directory: String::from("/tmp"),
            file: String::from("test.c"),
            command: Some(String::from("gcc -c test.c")),
            arguments: None,
            output: Some(String::from("test.o")),
        };

        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: CompileCommand = serde_json::from_str(&json).unwrap();

        assert_eq!(cmd.directory, decoded.directory);
        assert_eq!(cmd.file, decoded.file);
        assert_eq!(cmd.command, decoded.command);
        assert_eq!(cmd.arguments, decoded.arguments);
        assert_eq!(cmd.output, decoded.output);
    }
}
