use crate::{CompileCommand, CompileDbError, Config};
use anyhow::Context;
use regex::Regex;
use std::{
    io::{BufRead, BufReader},
    path::Path,
};
use tracing::{debug, warn};

pub struct Parser {
    compile_regex: Regex,
    file_regex: Regex,
}

impl Parser {
    pub fn new(config: &Config) -> Result<Self, CompileDbError> {
        let compile_regex = Regex::new(&config.regex_compile)
            .map_err(|e| CompileDbError::InvalidCommand(e.to_string()))?;
        let file_regex = Regex::new(&config.regex_file)
            .map_err(|e| CompileDbError::InvalidCommand(e.to_string()))?;

        Ok(Self {
            compile_regex,
            file_regex,
        })
    }

    /// Parse a single line of build output
    pub fn parse_line(&self, line: &str, config: &Config) -> Option<CompileCommand> {
        // Skip empty lines and non-compilation commands
        if line.trim().is_empty() || !self.compile_regex.is_match(line) {
            return None;
        }

        // Extract source file
        let file_match = self.file_regex.captures(line)?;
        let file = file_match.get(1)?.as_str().to_string();

        // Check if file should be excluded
        if config.exclude_patterns.iter().any(|pattern| {
            Regex::new(pattern)
                .map(|re| re.is_match(&file))
                .unwrap_or(false)
        }) {
            return None;
        }

        // Check if file exists when strict mode is enabled
        if !config.no_strict {
            let file_path = Path::new(&file);
            if !file_path.exists() {
                warn!("Source file not found: {}", file);
                return None;
            }
        }

        // Split command into arguments or keep as single string
        let (command, arguments) = if config.command_style {
            (Some(line.to_string()), None)
        } else {
            // Simple shell-like argument splitting
            // Note: This is a simplified version, a real implementation would need
            // to handle quotes and escapes properly
            let args: Vec<String> = line.split_whitespace().map(|s| s.to_string()).collect();
            (None, Some(args))
        };

        // Get absolute path for compiler if requested
        let arguments = if config.full_path {
            arguments.map(|args| {
                let mut args = args;
                if let Some(first) = args.first() {
                    if let Ok(full_path) = which::which(first) {
                        args[0] = full_path.to_string_lossy().into_owned();
                    }
                }
                args
            })
        } else {
            arguments
        };

        Some(CompileCommand {
            directory: config.build_dir.to_string_lossy().into_owned(),
            file,
            command,
            arguments,
            output: None, // Could be extracted from -o flag if needed
        })
    }

    /// Parse build log file and extract compilation commands
    pub fn parse_file(
        &self,
        path: &Path,
        config: &Config,
    ) -> Result<Vec<CompileCommand>, CompileDbError> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open build log file: {}", path.display()))
            .map_err(|e| CompileDbError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

        let reader = BufReader::new(file);
        let mut commands = Vec::new();

        for line in reader.lines() {
            let line = line.map_err(CompileDbError::Io)?;
            if let Some(cmd) = self.parse_line(&line, config) {
                debug!("Found compilation command: {:?}", cmd);
                commands.push(cmd);
            }
        }

        Ok(commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_parse_gcc_command() {
        let config = Config {
            no_strict: true, // Don't check for file existence in test
            ..Config::default()
        };
        let parser = Parser::new(&config).unwrap();

        let cmd = "gcc -c test.c -o test.o";
        let result = parser.parse_line(cmd, &config);

        assert!(result.is_some());
        let cmd = result.unwrap();
        assert_eq!(cmd.file, "test.c");
        assert!(cmd.arguments.is_some());
        assert_eq!(cmd.arguments.unwrap().len(), 5);
    }

    #[test]
    fn test_parse_build_log() {
        let dir = tempdir().unwrap();
        let log_path = dir.path().join("build.log");
        let mut file = File::create(&log_path).unwrap();

        writeln!(file, "gcc -c test1.c -o test1.o").unwrap();
        writeln!(file, "gcc -c test2.c -o test2.o").unwrap();
        writeln!(file, "echo 'Not a compile command'").unwrap();

        let config = Config {
            no_strict: true, // Don't check for file existence in test
            ..Config::default()
        };
        let parser = Parser::new(&config).unwrap();

        let commands = parser.parse_file(&log_path, &config).unwrap();
        assert_eq!(commands.len(), 2);
    }
}
