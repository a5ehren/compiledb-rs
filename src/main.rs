use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use compiledb::{CompileDbError, Config};
use std::io::BufRead;
use std::path::PathBuf;
extern crate env_logger;
extern crate log;
use log::info;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Build log file to parse compilation commands
    #[arg(short = 'p', long = "parse")]
    build_log: Option<PathBuf>,

    /// Output file path
    #[arg(short, long, default_value = "compile_commands.json")]
    output: PathBuf,

    /// Initial build directory
    #[arg(short = 'd', long = "build-dir")]
    build_dir: Option<PathBuf>,

    /// Regular expressions to exclude files
    #[arg(short = 'e', long = "exclude")]
    exclude: Vec<String>,

    /// Skip actual build
    #[arg(short = 'n', long = "no-build")]
    no_build: bool,

    /// Enable verbose output (-v for info, -vv for debug level messages)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Skip source file existence check
    #[arg(short = 'S', long = "no-strict")]
    no_strict: bool,

    /// Add predefined compiler macros
    #[arg(short = 'm', long = "macros")]
    macros: Vec<String>,

    /// Use command style output
    #[arg(short = 'c', long = "command-style")]
    command_style: bool,

    /// Use full compiler path
    #[arg(long = "full-path")]
    full_path: bool,

    /// Regular expressions to find compile commands
    #[arg(
        long = "regex-compile",
        default_value = r"(?:[^/]*/)*(gcc|clang|cc|g\+\+|c\+\+|clang\+\+|cl)(?:-[0-9\.]+)?(?:\s|$)"
    )]
    regex_compile: String,

    /// Regular expressions to find source files
    #[arg(
        long = "regex-file",
        default_value = r"\s-c\s+(\S+\.(c|cpp|cc|cxx|c\+\+|s|m|mm|cu))\s+-o\s"
    )]
    regex_file: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run make and generate compilation database
    Make {
        /// Arguments to pass to make
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
}

fn run() -> Result<(), CompileDbError> {
    let cli = Cli::parse();

    // Configure logging based on verbose flag
    let log_level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level)).init();

    let config = Config {
        build_log: cli.build_log,
        output_file: cli.output,
        build_dir: cli
            .build_dir
            .unwrap_or_else(|| std::env::current_dir().unwrap()),
        exclude_patterns: cli.exclude,
        no_build: cli.no_build,
        verbose: cli.verbose,
        no_strict: cli.no_strict,
        macros: cli.macros,
        command_style: cli.command_style,
        full_path: cli.full_path,
        regex_compile: cli.regex_compile,
        regex_file: cli.regex_file,
    };

    match cli.command {
        Some(Commands::Make { args }) => {
            let wrapper = compiledb::make_wrapper::MakeWrapper::new();

            // First run make with -Bnwk to get compilation commands
            let commands = wrapper.execute(&args, &config)?;

            // Write compilation database
            let file = std::fs::File::create(&config.output_file)
                .with_context(|| {
                    format!(
                        "Failed to create output file: {}",
                        config.output_file.display()
                    )
                })
                .map_err(|e| CompileDbError::Io(std::io::Error::other(e)))?;

            serde_json::to_writer_pretty(file, &commands).map_err(CompileDbError::Json)?;

            info!(
                "Wrote compilation database to {}",
                config.output_file.display()
            );

            // Run actual build if requested
            wrapper.run_build(&args, &config)?;
        }
        None => {
            // Parse from file or stdin
            let mut parser = compiledb::parser::Parser::new(&config)?;

            let commands = if let Some(log_file) = config.build_log.as_ref() {
                parser.parse_file(log_file, &config)?
            } else {
                // Read from stdin
                info!("Reading build output from stdin...");
                let stdin = std::io::stdin();
                let reader = std::io::BufReader::new(stdin);
                let mut commands = Vec::new();
                let mut line_count = 0;

                for line in reader.lines() {
                    line_count += 1;
                    let line = line.map_err(CompileDbError::Io)?;
                    let parsed_commands = parser.parse_line(&line, &config);
                    if !parsed_commands.is_empty() {
                        info!(
                            "Found {} compile commands in line {}",
                            parsed_commands.len(),
                            line_count
                        );
                        for (i, cmd) in parsed_commands.iter().enumerate() {
                            info!(
                                "  Command {}.{}: file={}, dir={}",
                                line_count,
                                i + 1,
                                cmd.file,
                                cmd.directory
                            );
                        }
                    }
                    commands.extend(parsed_commands);
                }

                info!("Total lines processed: {line_count}");
                info!("Total compile commands found: {}", commands.len());

                commands
            };

            // Write compilation database
            let file = std::fs::File::create(&config.output_file)
                .with_context(|| {
                    format!(
                        "Failed to create output file: {}",
                        config.output_file.display()
                    )
                })
                .map_err(|e| CompileDbError::Io(std::io::Error::other(e)))?;

            serde_json::to_writer_pretty(file, &commands).map_err(CompileDbError::Json)?;

            info!(
                "Wrote compilation database to {}",
                config.output_file.display()
            );
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
