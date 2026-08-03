use crate::{CompileCommand, CompileDbError, Config};
use anyhow::Context;
use log::{debug, info, warn};
use once_cell::sync::Lazy;
use regex::Regex;
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Command,
};

// Static regex patterns - compiled once at startup
static CD_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^cd\s+(.*)$"#).expect("Invalid CD_REGEX"));
static SH_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"\s*(;|&&|\|\|)\s*"#).expect("Invalid SH_REGEX"));
static NESTED_CMD_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"`([^`]+)`"#).expect("Invalid NESTED_CMD_REGEX"));
static MAKE_ENTER_DIR: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"^.*?(?:mingw32-make|gmake|make).*?: Entering directory .*['\`](.*)['\`]$"#)
        .expect("Invalid MAKE_ENTER_DIR")
});
static MAKE_LEAVE_DIR: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"^.*?(?:mingw32-make|gmake|make).*?: Leaving directory .*'(.*)'$"#)
        .expect("Invalid MAKE_LEAVE_DIR")
});
static MAKE_CMD_DIR: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"^\s*(?:mingw32-make|gmake|make).*?-C\s+(.*?)(\s|$)"#)
        .expect("Invalid MAKE_CMD_DIR")
});
static CHECKING_MAKE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^\s?checking whether .*(yes|no)$"#).expect("Invalid CHECKING_MAKE"));

pub struct Parser {
    compile_regex: Regex,
    file_regex: Regex,
    exclude_regex: Option<Regex>,
    dir_stack: Vec<PathBuf>,
    working_dir: PathBuf,
}

impl Parser {
    pub fn new(config: &Config) -> Result<Self, CompileDbError> {
        info!("Initializing parser with compile regex: {}", config.regex_compile);
        info!("File regex: {}", config.regex_file);

        let compile_regex = Regex::new(&config.regex_compile)
            .map_err(|e| CompileDbError::InvalidCommand(e.to_string()))?;
        let file_regex = Regex::new(&config.regex_file)
            .map_err(|e| CompileDbError::InvalidCommand(e.to_string()))?;

        // Initialize exclude regex if pattern is provided
        let exclude_regex = if !config.exclude_patterns.is_empty() {
            info!("Exclude patterns: {:?}", config.exclude_patterns);
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

        info!("Working directory: {}", working_dir.display());

        Ok(Self {
            compile_regex,
            file_regex,
            exclude_regex,
            dir_stack: vec![working_dir.clone()],
            working_dir,
        })
    }

    /// Parse a single line of build output
    pub fn parse_line(&mut self, line: &str, config: &Config) -> Vec<CompileCommand> {
        let line = line.trim();
        let mut commands = Vec::new();

        // Skip empty lines and make checking lines
        if line.is_empty() || CHECKING_MAKE.is_match(line) {
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
            if let Some(caps) = CD_REGEX.captures(&cmd) {
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
        info!("Parsing build log file: {}", path.display());

        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open build log file: {}", path.display()))
            .map_err(|e| CompileDbError::Io(std::io::Error::other(e)))?;

        let reader = BufReader::new(file);
        let mut commands = Vec::new();
        let mut cmd_count = 0;
        let mut line_count = 0;

        for line in reader.lines() {
            line_count += 1;
            let line = line.map_err(CompileDbError::Io)?;
            let new_commands = self.parse_line(&line, config);
            for cmd in new_commands {
                debug!("Adding command {}: {:?}", cmd_count, cmd);
                commands.push(cmd);
                cmd_count += 1;
            }
        }

        info!("Processed {} lines from build log", line_count);
        info!("Found {} compilation commands", commands.len());
        Ok(commands)
    }

    /// Split a command string into individual commands based on shell operators
    fn split_commands(&self, command: &str) -> Vec<String> {
        SH_REGEX.split(command).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
    }

    /// Process nested commands (backtick substitution)
    fn process_nested_commands(&self, line: &str) -> String {
        let mut result = line.to_string();
        while let Some(caps) = NESTED_CMD_REGEX.captures(&result) {
            if let Some(nested_cmd) = caps.get(1) {
                let output = Command::new("sh").arg("-c").arg(nested_cmd.as_str()).output();

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
        if let Some(caps) = MAKE_ENTER_DIR.captures(line) {
            if let Some(dir) = caps.get(1) {
                let enter_dir = PathBuf::from(dir.as_str());
                self.dir_stack.insert(0, enter_dir.clone());
                self.working_dir = enter_dir;
                info!("Entering directory: {}", self.working_dir.display());
                return true;
            }
        } else if MAKE_LEAVE_DIR.captures(line).is_some() {
            if !self.dir_stack.is_empty() {
                self.dir_stack.remove(0);
                if !self.dir_stack.is_empty() {
                    self.working_dir = self.dir_stack[0].clone();
                }
                info!("Leaving directory: {}", self.working_dir.display());
                return true;
            }
        } else if let Some(caps) = MAKE_CMD_DIR.captures(line) {
            if let Some(dir) = caps.get(1) {
                let enter_dir = PathBuf::from(dir.as_str());
                if enter_dir.as_os_str() != "." {
                    self.dir_stack.insert(0, enter_dir.clone());
                    self.working_dir = if enter_dir.is_absolute() {
                        enter_dir
                    } else {
                        self.working_dir.join(enter_dir)
                    };
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
        let compile_idx = args.iter().position(|arg| self.compile_regex.is_match(arg))?;
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

        info!(
            "Found compile command for file: {} in directory: {}",
            file,
            self.working_dir.display()
        );
        debug!("Command arguments: {:?}", final_args);

        Some(CompileCommand {
            directory: self.working_dir.to_string_lossy().into_owned(),
            file,
            command: if config.command_style { Some(final_args.join(" ")) } else { None },
            arguments: if config.command_style { None } else { Some(final_args) },
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
        let config = Config { no_strict: true, ..Config::default() };
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
        let config = Config { no_strict: true, ..Config::default() };
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
        let config = Config { no_strict: true, ..Config::default() };
        let mut parser = Parser::new(&config).unwrap();
        let initial_dir = parser.working_dir.clone();

        // Test cd command
        let result = parser.parse_line("cd src && gcc -c test.c -o test.o", &config);
        assert_eq!(result.len(), 1);
        assert_eq!(parser.working_dir, initial_dir.join("src"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_parse_complex_build_log() {
        // enable logging, since log defaults to silent
        let mut builder = env_logger::Builder::from_default_env();
        builder.filter_level(log::LevelFilter::Debug);
        builder.init();

        let config = Config { no_strict: true, ..Config::default() };
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
        assert_eq!(cmd.file, "core/src/xyz/widget.c", "Parser did not find correct file");

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
            "_build/platform_x64_release/widget.o",
        ];

        assert_eq!(
            cmd.arguments.as_ref().unwrap(),
            &expected_args,
            "Parser did not find correct arguments"
        );
    }

    #[test]
    fn test_exclude_patterns() {
        let config = Config {
            no_strict: true,
            exclude_patterns: vec![r"test\.c"].into_iter().map(String::from).collect(),
            ..Config::default()
        };
        let mut parser = Parser::new(&config).unwrap();

        // This should be excluded
        let cmd = "gcc -c test.c -o test.o";
        let result = parser.parse_line(cmd, &config);
        assert_eq!(result.len(), 0, "File matching exclude pattern should be filtered");

        // This should not be excluded
        let cmd = "gcc -c other.c -o other.o";
        let result = parser.parse_line(cmd, &config);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_full_path_compiler() {
        let config = Config { no_strict: true, full_path: true, ..Config::default() };
        let mut parser = Parser::new(&config).unwrap();

        let cmd = "gcc -c test.c -o test.o";
        let result = parser.parse_line(cmd, &config);
        assert_eq!(result.len(), 1);
        let cmd = &result[0];
        let args = cmd.arguments.as_ref().unwrap();
        // When full_path is true, the compiler should be resolved to full path
        // In test environment, gcc might not exist, so we just check it doesn't panic
        assert!(args[0].contains("gcc"));
    }

    #[test]
    fn test_command_style_output() {
        let config = Config { no_strict: true, command_style: true, ..Config::default() };
        let mut parser = Parser::new(&config).unwrap();

        let cmd = "gcc -c test.c -o test.o";
        let result = parser.parse_line(cmd, &config);
        assert_eq!(result.len(), 1);
        let cmd = &result[0];
        // In command_style, command should be Some, arguments should be None
        assert!(cmd.command.is_some());
        assert!(cmd.arguments.is_none());
        assert!(cmd.command.as_ref().unwrap().contains("gcc"));
    }

    #[test]
    fn test_macros_handling() {
        let config = Config {
            no_strict: true,
            macros: vec!["-DTEST_MACRO=1", "-DANOTHER_MACRO"]
                .into_iter()
                .map(String::from)
                .collect(),
            ..Config::default()
        };
        let mut parser = Parser::new(&config).unwrap();

        let cmd = "gcc -c test.c -o test.o";
        let result = parser.parse_line(cmd, &config);
        assert_eq!(result.len(), 1);
        let cmd = &result[0];
        let args = cmd.arguments.as_ref().unwrap();
        // Macros should be appended to the arguments
        assert!(args.contains(&"-DTEST_MACRO=1".to_string()));
        assert!(args.contains(&"-DANOTHER_MACRO".to_string()));
    }

    #[test]
    fn test_no_strict_mode() {
        let config = Config { no_strict: true, ..Config::default() };
        let mut parser = Parser::new(&config).unwrap();

        // File doesn't exist, but no_strict is true so it should still parse
        let cmd = "gcc -c nonexistent.c -o nonexistent.o";
        let result = parser.parse_line(cmd, &config);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_strict_mode_missing_file() {
        let config = Config {
            no_strict: false, // strict mode
            ..Config::default()
        };
        let mut parser = Parser::new(&config).unwrap();

        // File doesn't exist and strict mode is on, should be filtered
        let cmd = "gcc -c nonexistent.c -o nonexistent.o";
        let result = parser.parse_line(cmd, &config);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_make_c_directory() {
        let config = Config { no_strict: true, ..Config::default() };
        let mut parser = Parser::new(&config).unwrap();
        let initial_dir = parser.working_dir.clone();

        // Test make -C directory
        let result = parser.parse_line("make -C subdir target", &config);
        assert_eq!(result.len(), 0);
        assert_eq!(parser.working_dir, initial_dir.join("subdir"));

        // Test make -C .
        parser.working_dir = initial_dir.clone();
        let result = parser.parse_line("make -C . target", &config);
        assert_eq!(result.len(), 0);
        assert_eq!(parser.working_dir, initial_dir);
    }

    #[test]
    fn test_split_commands_edge_cases() {
        let config = Config { no_strict: true, ..Config::default() };
        let parser = Parser::new(&config).unwrap();

        // Test multiple commands separated by &&
        let cmds = parser.split_commands("gcc -c a.c -o a.o && gcc -c b.c -o b.o");
        assert_eq!(cmds.len(), 2);

        // Test commands separated by ||
        let cmds = parser.split_commands("cmd1 || cmd2");
        assert_eq!(cmds.len(), 2);

        // Test commands separated by ;
        let cmds = parser.split_commands("cmd1; cmd2");
        assert_eq!(cmds.len(), 2);

        // Test empty and whitespace
        let cmds = parser.split_commands("   ");
        assert_eq!(cmds.len(), 0);

        // Test single command
        let cmds = parser.split_commands("gcc -c test.c -o test.o");
        assert_eq!(cmds.len(), 1);
    }

    #[test]
    fn test_nested_commands_multiple() {
        let config = Config { no_strict: true, ..Config::default() };
        let mut parser = Parser::new(&config).unwrap();

        // Multiple backtick commands
        let cmd = "gcc -c `echo test1.c` -o test1.o && gcc -c `echo test2.c` -o test2.o";
        let result = parser.parse_line(cmd, &config);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_cpp_file_extensions() {
        let config = Config { no_strict: true, ..Config::default() };
        let mut parser = Parser::new(&config).unwrap();

        let extensions = vec!["cpp", "cc", "cxx", "c++", "s", "m", "mm", "cu"];
        for ext in extensions {
            let cmd = format!("gcc -c test.{ext} -o test.o");
            let result = parser.parse_line(&cmd, &config);
            assert_eq!(result.len(), 1, "Failed for extension: {ext}");
            assert_eq!(result[0].file, format!("test.{ext}"));
        }
    }

    #[test]
    fn test_make_leave_directory_stack() {
        let config = Config { no_strict: true, ..Config::default() };
        let mut parser = Parser::new(&config).unwrap();
        let initial_dir = parser.working_dir.clone();

        // Enter two directories
        parser.parse_line("make[1]: Entering directory '/path/to/src'", &config);
        parser.parse_line("make[2]: Entering directory '/path/to/src/sub'", &config);
        assert_eq!(parser.working_dir, PathBuf::from("/path/to/src/sub"));

        // Leave one
        parser.parse_line("make[2]: Leaving directory '/path/to/src/sub'", &config);
        assert_eq!(parser.working_dir, PathBuf::from("/path/to/src"));

        // Leave another
        parser.parse_line("make[1]: Leaving directory '/path/to/src'", &config);
        assert_eq!(parser.working_dir, initial_dir);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_absolute_file_path_conversion() {
        let config = Config { no_strict: true, ..Config::default() };
        let mut parser = Parser::new(&config).unwrap();
        // Set working dir to a known path
        parser.working_dir = PathBuf::from("/home/user/project");

        // Test absolute file path that's under working dir
        let cmd = "gcc -c /home/user/project/src/main.c -o main.o";
        let result = parser.parse_line(cmd, &config);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].file, "src/main.c");
    }

    #[test]
    fn test_compiler_with_version_suffix() {
        let config = Config { no_strict: true, ..Config::default() };
        let mut parser = Parser::new(&config).unwrap();

        let compilers = vec!["gcc-11", "clang-14", "g++-12", "clang++-15"];
        for compiler in compilers {
            let cmd = format!("{compiler} -c test.c -o test.o");
            let result = parser.parse_line(&cmd, &config);
            assert_eq!(result.len(), 1, "Failed for compiler: {compiler}");
        }
    }
}
