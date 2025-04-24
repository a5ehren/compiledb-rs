use crate::{CompileCommand, CompileDbError, Config};
use anyhow::Context;
use regex::Regex;
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Command,
};
extern crate env_logger;
extern crate log;
use log::{debug, info, warn};

pub struct Parser {
    compile_regex: Regex,
    file_regex: Regex,
    exclude_regex: Option<Regex>,
    cd_regex: Regex,
    sh_regex: Regex,
    nested_cmd_regex: Regex,
    make_enter_dir: Regex,
    make_leave_dir: Regex,
    make_cmd_dir: Regex,
    checking_make: Regex,
    dir_stack: Vec<PathBuf>,
    working_dir: PathBuf,
}

impl Parser {
    pub fn new(config: &Config) -> Result<Self, CompileDbError> {
        let compile_regex = Regex::new(&config.regex_compile)
            .map_err(|e| CompileDbError::InvalidCommand(e.to_string()))?;
        let file_regex = Regex::new(&config.regex_file)
            .map_err(|e| CompileDbError::InvalidCommand(e.to_string()))?;

        // Initialize exclude regex if pattern is provided
        let exclude_regex = if !config.exclude_patterns.is_empty() {
            Some(
                Regex::new(&config.exclude_patterns[0])
                    .map_err(|e| CompileDbError::InvalidCommand(e.to_string()))?,
            )
        } else {
            None
        };

        // Initialize working directory
        let working_dir = if !config.build_dir.as_os_str().is_empty() {
            config.build_dir.clone()
        } else {
            std::env::current_dir().map_err(CompileDbError::Io)?
        };

        Ok(Self {
            compile_regex,
            file_regex,
            exclude_regex,
            cd_regex: Regex::new(r#"^cd\s+(.*)$"#).unwrap(),
            sh_regex: Regex::new(r#"\s*(;|&&|\|\|)\s*"#).unwrap(),
            nested_cmd_regex: Regex::new(r#"`([^`]+)`"#).unwrap(),
            make_enter_dir: Regex::new(
                r#"^.*?(?:mingw32-make|gmake|make).*?: Entering directory .*['`"](.*)['`"]$"#,
            )
            .unwrap(),
            make_leave_dir: Regex::new(
                r#"^.*?(?:mingw32-make|gmake|make).*?: Leaving directory .*'(.*)'$"#,
            )
            .unwrap(),
            make_cmd_dir: Regex::new(r#"^\s*(?:mingw32-make|gmake|make).*?-C\s+(.*?)(\s|$)"#)
                .unwrap(),
            checking_make: Regex::new(r#"^\s?checking whether .*(yes|no)$"#).unwrap(),
            dir_stack: vec![working_dir.clone()],
            working_dir,
        })
    }

    /// Parse a single line of build output
    pub fn parse_line(&mut self, line: &str, config: &Config) -> Vec<CompileCommand> {
        let line = line.trim();
        let mut commands = Vec::new();

        // Skip empty lines and make checking lines
        if line.is_empty() || self.checking_make.is_match(line) {
            return commands;
        }

        // Handle directory changes
        if self.update_working_dir(line) {
            return commands;
        }

        // Skip non-compilation commands
        if !self.compile_regex.is_match(line) {
            debug!("Line did not match compile regex: {line}");
            return commands;
        }
        debug!("Found potential compile command: {line}");

        // Process nested commands (backticks)
        let line = self.process_nested_commands(line);

        // Replace escaped quotes
        let line = line.replace(r#"\""#, r#"""#);

        // Split into individual commands
        for cmd in self.split_commands(&line) {
            // Handle cd commands
            if let Some(caps) = self.cd_regex.captures(&cmd) {
                if let Some(dir) = caps.get(1) {
                    let new_dir = PathBuf::from(dir.as_str());
                    self.working_dir = if new_dir.is_absolute() {
                        new_dir
                    } else {
                        self.working_dir.join(new_dir)
                    };
                    info!("Changed directory to: {}", self.working_dir.display());
                }
                continue;
            }

            // Process compilation command
            if self.compile_regex.is_match(&cmd) {
                if let Some(compile_cmd) = self.process_compile_command(&cmd, config) {
                    commands.push(compile_cmd);
                }
            }
        }

        commands
    }

    /// Parse build log file and extract compilation commands
    pub fn parse_file(
        &mut self,
        path: &Path,
        config: &Config,
    ) -> Result<Vec<CompileCommand>, CompileDbError> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open build log file: {}", path.display()))
            .map_err(|e| CompileDbError::Io(std::io::Error::other(e)))?;

        let reader = BufReader::new(file);
        let mut commands = Vec::new();
        let mut cmd_count = 0;

        for line in reader.lines() {
            let line = line.map_err(CompileDbError::Io)?;
            let new_commands = self.parse_line(&line, config);
            for cmd in new_commands {
                debug!("Adding command {cmd_count}: {cmd:?}");
                commands.push(cmd);
                cmd_count += 1;
            }
        }

        Ok(commands)
    }

    /// Split a command string into individual commands based on shell operators
    fn split_commands(&self, command: &str) -> Vec<String> {
        self.sh_regex
            .split(command)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    }

    /// Process nested commands (backtick substitution)
    fn process_nested_commands(&self, line: &str) -> String {
        let mut result = line.to_string();
        while let Some(caps) = self.nested_cmd_regex.captures(&result) {
            if let Some(nested_cmd) = caps.get(1) {
                let output = Command::new("sh")
                    .arg("-c")
                    .arg(nested_cmd.as_str())
                    .output();

                match output {
                    Ok(output) if output.status.success() => {
                        let cmd_output = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        result = result.replace(&caps[0], &cmd_output);
                    }
                    _ => {
                        warn!("Failed to execute nested command: {}", nested_cmd.as_str());
                        break;
                    }
                }
            }
        }
        result
    }

    /// Update working directory based on make directory commands
    fn update_working_dir(&mut self, line: &str) -> bool {
        if let Some(caps) = self.make_enter_dir.captures(line) {
            if let Some(dir) = caps.get(1) {
                let enter_dir = PathBuf::from(dir.as_str());
                self.dir_stack.insert(0, enter_dir.clone());
                self.working_dir = enter_dir;
                info!("Entering directory: {}", self.working_dir.display());
                return true;
            }
        } else if self.make_leave_dir.captures(line).is_some() {
            if !self.dir_stack.is_empty() {
                self.dir_stack.remove(0);
                if !self.dir_stack.is_empty() {
                    self.working_dir = self.dir_stack[0].clone();
                }
                info!("Leaving directory: {}", self.working_dir.display());
                return true;
            }
        } else if let Some(caps) = self.make_cmd_dir.captures(line) {
            if let Some(dir) = caps.get(1) {
                let enter_dir = PathBuf::from(dir.as_str());
                if enter_dir.as_os_str() != "." {
                    self.dir_stack.insert(0, enter_dir.clone());
                    self.working_dir = enter_dir;
                    info!("Make -C directory: {}", self.working_dir.display());
                }
                return true;
            }
        }
        false
    }

    /// Process a compilation command
    fn process_compile_command(&self, command: &str, config: &Config) -> Option<CompileCommand> {
        // Split command into arguments
        let args: Vec<String> = command.split_whitespace().map(String::from).collect();

        // Find compiler command
        let compile_idx = args
            .iter()
            .position(|arg| self.compile_regex.is_match(arg))?;
        let arguments = args[compile_idx..].to_vec();

        // Extract source file
        let file_match = self.file_regex.captures(command)?;
        let file = file_match.get(1)?.as_str().to_string();
        debug!("Found source file: {file}");

        // Convert absolute path to relative path if needed
        let file = if Path::new(&file).is_absolute() {
            let file_path = PathBuf::from(&file);
            // Try to strip the working directory prefix
            if let Ok(rel_path) = file_path.strip_prefix(&self.working_dir) {
                rel_path.to_string_lossy().into_owned()
            } else {
                // If the file path doesn't start with working_dir, try to find the common suffix
                let file_components: Vec<_> = file_path.components().collect();
                let working_dir_components: Vec<_> = self.working_dir.components().collect();

                // Find where the paths start to match
                let mut match_start = None;
                for i in 0..file_components.len() {
                    for j in 0..working_dir_components.len() {
                        if file_components[i..].starts_with(&working_dir_components[j..]) {
                            match_start = Some(i);
                            break;
                        }
                    }
                    if match_start.is_some() {
                        break;
                    }
                }

                // If we found a match, use that as the relative path
                if let Some(start) = match_start {
                    let rel_path = file_components[start..].iter().collect::<PathBuf>();
                    rel_path.to_string_lossy().into_owned()
                } else {
                    file
                }
            }
        } else {
            file
        };

        // Get full path for compiler if requested
        let mut final_args = if config.full_path {
            let mut args = arguments.clone();
            if let Ok(full_path) = which::which(&args[0]) {
                args[0] = full_path.to_string_lossy().into_owned();
            }
            args
        } else {
            arguments
        };

        // Make file path in arguments relative if needed
        if let Some(c_idx) = final_args.iter().position(|arg| arg == "-c") {
            if c_idx + 1 < final_args.len() {
                let arg_file = &final_args[c_idx + 1];
                if Path::new(arg_file).is_absolute() {
                    if let Ok(rel_path) = PathBuf::from(arg_file).strip_prefix(&self.working_dir) {
                        final_args[c_idx + 1] = rel_path.to_string_lossy().into_owned();
                    }
                }
            }
        }

        // Check exclusion
        if let Some(ref exclude_re) = self.exclude_regex {
            if exclude_re.is_match(&file) {
                info!("File {file} excluded");
                return None;
            }
        }

        // Check file existence in strict mode
        if !config.no_strict {
            let file_path = self.working_dir.join(&file);
            if !file_path.exists() {
                warn!("Source file not found: {}", file_path.display());
                return None;
            }
        }

        // Add custom macros if specified
        final_args.extend(config.macros.iter().cloned());

        Some(CompileCommand {
            directory: self.working_dir.to_string_lossy().into_owned(),
            file,
            command: if config.command_style {
                Some(final_args.join(" "))
            } else {
                None
            },
            arguments: if config.command_style {
                None
            } else {
                Some(final_args)
            },
            output: None,
        })
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
        let mut parser = Parser::new(&config).unwrap();

        let cmd = "gcc -c test.c -o test.o";
        let result = parser.parse_line(cmd, &config);

        assert_eq!(result.len(), 1);
        let cmd = &result[0];
        assert_eq!(cmd.file, "test.c");
        assert!(cmd.arguments.is_some());
        assert_eq!(cmd.arguments.as_ref().unwrap().len(), 5);
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
        let mut parser = Parser::new(&config).unwrap();

        let commands = parser.parse_file(&log_path, &config).unwrap();
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn test_directory_handling() {
        let config = Config {
            no_strict: true,
            ..Config::default()
        };
        let mut parser = Parser::new(&config).unwrap();
        let initial_dir = parser.working_dir.clone();

        // Test make enter directory
        let result = parser.parse_line("make[1]: Entering directory '/path/to/src'", &config);
        assert_eq!(result.len(), 0);
        assert_eq!(parser.working_dir, PathBuf::from("/path/to/src"));

        // Test make leave directory
        let result = parser.parse_line("make[1]: Leaving directory '/path/to/src'", &config);
        assert_eq!(result.len(), 0);
        assert_eq!(parser.working_dir, initial_dir);
    }

    #[test]
    fn test_nested_commands() {
        let config = Config {
            no_strict: true,
            ..Config::default()
        };
        let mut parser = Parser::new(&config).unwrap();

        // Test command with backticks
        let cmd = "gcc -c `echo test.c` -o test.o";
        let result = parser.parse_line(cmd, &config);
        assert_eq!(result.len(), 1);
        let cmd = &result[0];
        assert_eq!(cmd.file, "test.c");
    }

    #[test]
    fn test_cd_command() {
        let config = Config {
            no_strict: true,
            ..Config::default()
        };
        let mut parser = Parser::new(&config).unwrap();
        let initial_dir = parser.working_dir.clone();

        // Test cd command
        let result = parser.parse_line("cd src && gcc -c test.c -o test.o", &config);
        assert_eq!(result.len(), 1);
        assert_eq!(parser.working_dir, initial_dir.join("src"));
    }

    #[test]
    fn test_parse_complex_build_log() {
        // Skip this test on Windows platforms
        if cfg!(target_os = "windows") {
            println!("Skipping test_parse_complex_build_log on Windows");
            return;
        }

        // enable logging, since log defaults to silent
        std::env::set_var("RUST_LOG", "debug");
        env_logger::init();

        let config = Config {
            no_strict: true,
            ..Config::default()
        };
        let mut parser = Parser::new(&config).unwrap();
        parser.working_dir = PathBuf::from("/foo/bar/workspace/project/core/engine/drivers/module");

        let complex_cmd = r#"/usr/bin/printf " [ %-17.17s ]  CC           drivers/module/core/src/xyz/widget.c\n" ""module/core"" && ( set -e ;  /foo/bar/workspace/tools/hosts/platform-x64/compiler/gcc-9.2.0/bin/x86_64-none-linux-gcc  -include /foo/bar/workspace/project/core/engine/sdk/vendor/inc/sysdef.h  -isystem/foo/bar/workspace/tools/hosts/platform-x64/compiler/gcc-9.2.0/lib/gcc/x86_64-none-linux/9.2.0/include -isystem/foo/bar/workspace/tools/hosts/platform-x64/compiler/gcc-9.2.0/lib/gcc/x86_64-none-linux/9.2.0/include-fixed -isystem/foo/bar/workspace/tools/hosts/platform-x64/compiler/gcc-9.2.0/include/c++/9.2.0 -Werror -Wextra -Wshadow -Wcast-align -Wno-unused-parameter -Wno-missing-field-initializers  -fPIC        -g -fno-omit-frame-pointer -fdebug-prefix-map -fstack-protector           -DNDEBUG -DPLATFORM_X64 -DFEATURE_XYZ -DVENDOR_ABC -DCONFIG_TYPE=platform_release_config -D_STRICT_ANSI -D_XOPEN_SOURCE=700 -I_build/platform_x64_release/include/mirror/core/tools/xyz/include -I/foo/bar/workspace/project/core/engine/drivers/common/inc -I/foo/bar/workspace/project/core/engine/drivers/common/inc -isystem/foo/bar/workspace/project/core/engine/drivers/vendor/interface/public/ -fvisibility=hidden -DENABLE_FEATURE_A=1 -DFEATURE_B_SUPPORT=1  -DUSE_NEW_API     -x c         -pedantic -Wno-long-long     -std=c11 -MMD -MP -MT _build/platform_x64_release/widget.o -MF _build/platform_x64_release/widget_dep.mk.tmp -c /foo/bar/workspace/project/core/engine/drivers/module/core/src/xyz/widget.c -o _build/platform_x64_release/widget.o ; /usr/bin/sed -i _build/platform_x64_release/widget_dep.mk.tmp -e ' 1,3s| /foo/bar/workspace/project/core/engine/drivers/module/core/src/xyz/widget.c | |' ; /usr/bin/mv -f _build/platform_x64_release/widget_dep.mk.tmp _build/platform_x64_release/widget_dep.mk )"#;

        let result = parser.parse_line(complex_cmd, &config);
        assert_eq!(result.len(), 1, "Parser did not find any commands");

        let cmd = &result[0];
        assert_eq!(
            cmd.directory, "/foo/bar/workspace/project/core/engine/drivers/module",
            "Parser did not find correct directory"
        );
        assert_eq!(
            cmd.file, "core/src/xyz/widget.c",
            "Parser did not find correct file"
        );

        let expected_args = vec![
            "/foo/bar/workspace/tools/hosts/platform-x64/compiler/gcc-9.2.0/bin/x86_64-none-linux-gcc",
            "-include",
            "/foo/bar/workspace/project/core/engine/sdk/vendor/inc/sysdef.h",
            "-isystem/foo/bar/workspace/tools/hosts/platform-x64/compiler/gcc-9.2.0/lib/gcc/x86_64-none-linux/9.2.0/include",
            "-isystem/foo/bar/workspace/tools/hosts/platform-x64/compiler/gcc-9.2.0/lib/gcc/x86_64-none-linux/9.2.0/include-fixed",
            "-isystem/foo/bar/workspace/tools/hosts/platform-x64/compiler/gcc-9.2.0/include/c++/9.2.0",
            "-Werror",
            "-Wextra", 
            "-Wshadow",
            "-Wcast-align",
            "-Wno-unused-parameter",
            "-Wno-missing-field-initializers",
            "-fPIC",
            "-g",
            "-fno-omit-frame-pointer",
            "-fdebug-prefix-map",
            "-fstack-protector",
            "-DNDEBUG",
            "-DPLATFORM_X64",
            "-DFEATURE_XYZ",
            "-DVENDOR_ABC",
            "-DCONFIG_TYPE=platform_release_config",
            "-D_STRICT_ANSI",
            "-D_XOPEN_SOURCE=700",
            "-I_build/platform_x64_release/include/mirror/core/tools/xyz/include",
            "-I/foo/bar/workspace/project/core/engine/drivers/common/inc",
            "-I/foo/bar/workspace/project/core/engine/drivers/common/inc",
            "-isystem/foo/bar/workspace/project/core/engine/drivers/vendor/interface/public/",
            "-fvisibility=hidden",
            "-DENABLE_FEATURE_A=1",
            "-DFEATURE_B_SUPPORT=1",
            "-DUSE_NEW_API",
            "-x",
            "c",
            "-pedantic",
            "-Wno-long-long",
            "-std=c11",
            "-MMD",
            "-MP",
            "-MT",
            "_build/platform_x64_release/widget.o",
            "-MF",
            "_build/platform_x64_release/widget_dep.mk.tmp",
            "-c",
            "core/src/xyz/widget.c",
            "-o",
            "_build/platform_x64_release/widget.o"
        ];

        assert_eq!(
            cmd.arguments.as_ref().unwrap(),
            &expected_args,
            "Parser did not find correct arguments"
        );
    }
}
