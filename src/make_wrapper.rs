use crate::{CompileCommand, CompileDbError, Config};
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
};
extern crate env_logger;
extern crate log;
use log::{debug, info};

pub struct MakeWrapper {
    make_path: PathBuf,
}

impl MakeWrapper {
    pub fn new() -> Self {
        let make_path = which::which("make").unwrap_or_else(|_| PathBuf::from("make"));

        Self { make_path }
    }

    /// Execute make command and capture its output
    pub fn execute(
        &self,
        args: &[String],
        config: &Config,
    ) -> Result<Vec<CompileCommand>, CompileDbError> {
        let mut command = Command::new(&self.make_path);

        // Add standard make flags for dry run and continue on error
        command
            .arg("-Bnkw")
            .args(args)
            .current_dir(&config.build_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        debug!("Executing make command: {command:?}");

        let mut child = command
            .spawn()
            .map_err(|e| CompileDbError::MakeError(e.to_string()))?;

        let stdout = child.stdout.take().ok_or_else(|| {
            CompileDbError::MakeError("Failed to capture make stdout".to_string())
        })?;

        let stderr = child.stderr.take().ok_or_else(|| {
            CompileDbError::MakeError("Failed to capture make stderr".to_string())
        })?;

        // Create parser for the make output
        let mut parser = crate::parser::Parser::new(config)?;
        let mut commands = Vec::new();

        // Process stdout
        let stdout_reader = BufReader::new(stdout);
        for line in stdout_reader.lines() {
            let line = line.map_err(CompileDbError::Io)?;
            commands.extend(parser.parse_line(&line, config));
        }

        // Process stderr (for warnings/errors)
        let stderr_reader = BufReader::new(stderr);
        for line in stderr_reader.lines() {
            let line = line.map_err(CompileDbError::Io)?;
            debug!("Make stderr: {line}");
        }

        // Wait for make to finish
        let status = child
            .wait()
            .map_err(|e| CompileDbError::MakeError(e.to_string()))?;

        if !status.success() && !config.no_build {
            return Err(CompileDbError::MakeError("Make command failed".to_string()));
        }

        info!("Found {} compilation commands", commands.len());
        Ok(commands)
    }

    /// Run the actual build command (when no_build is false)
    pub fn run_build(&self, args: &[String], config: &Config) -> Result<(), CompileDbError> {
        if config.no_build {
            return Ok(());
        }

        let mut command = Command::new(&self.make_path);
        command
            .args(args)
            .current_dir(&config.build_dir)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        debug!("Running build command: {command:?}");

        let status = command
            .status()
            .map_err(|e| CompileDbError::MakeError(e.to_string()))?;

        if !status.success() {
            return Err(CompileDbError::MakeError(
                "Build command failed".to_string(),
            ));
        }

        Ok(())
    }
}

impl Default for MakeWrapper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_make_wrapper_execution() {
        let dir = tempdir().unwrap();
        let makefile_path = dir.path().join("Makefile");
        let mut file = File::create(&makefile_path).unwrap();

        // Create a simple Makefile
        writeln!(file, "all: test.o\n").unwrap();
        writeln!(file, "test.o: test.c\n").unwrap();
        writeln!(file, "\tgcc -c test.c -o test.o\n").unwrap();

        // Create a dummy source file
        let source_path = dir.path().join("test.c");
        let mut file = File::create(&source_path).unwrap();
        writeln!(file, "int main() {{ return 0; }}\n").unwrap();

        let config = Config {
            build_dir: dir.path().to_path_buf(),
            no_strict: true, // Don't check for output file existence
            ..Config::default()
        };

        let wrapper = MakeWrapper::new();
        let result = wrapper.execute(&[], &config);

        assert!(result.is_ok());
        let commands = result.unwrap();
        assert_eq!(commands.len(), 1);
    }
}
