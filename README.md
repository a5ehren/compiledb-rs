# Compilation Database Generator (Rust)

A fast Rust implementation of [Clang's JSON Compilation Database][compdb] generator for GNU `make`-based build systems. This is a Rust rewrite of [compiledb-go](https://github.com/fcying/compiledb-go) for even better performance and safety.

## Project Status

- I consider this project to be feature-complete. I will sometimes release updates for dependency refreshes. Please open an issue or PR if you find a bug.

## Features

- Fast compilation database generation
- No clean build required in most cases
- Cross-compilation friendly
- Supports both command string and arguments list formats
- Configurable file exclusion patterns
- Full path resolution for compiler executables
- Async I/O for improved performance with large build logs

## Installation

### From crates.io
```bash
cargo install compiledb
```

### From source
```bash
git clone https://github.com/yourusername/compiledb-rs
cd compiledb-rs
cargo install --path .
```

## Usage

### Basic Usage

Generate compilation database from make output:
```bash
# Using the make wrapper
compiledb make

# Using make output directly
make -Bnwk | compiledb
```

### Command-line Options

```
USAGE: compiledb [options] command [command options] [args]...

OPTIONS:
    -p, --parse <file>           Build log file to parse compilation commands
    -o, --output <file>          Output file [default: compile_commands.json]
    -d, --build-dir <path>       Path to be used as initial build dir
    -e, --exclude <pattern>      Regular expressions to exclude files
    -n, --no-build              Only generates compilation db file
    -v, --verbose               Print verbose messages
    -S, --no-strict            Do not check if source files exist
    -m, --macros <macro>        Add predefined compiler macros
    -c, --command-style        Use command string format instead of arguments list
        --full-path            Write full path to compiler executable
        --regex-compile <re>   Regular expressions to find compile commands
        --regex-file <re>      Regular expressions to find source files

COMMANDS:
    make    Run make and generate compilation database
    help    Print this message or help for a command
```

### Examples

1. Generate database using make wrapper:
```bash
compiledb make
```

2. Parse from existing build log:
```bash
compiledb --parse build.log
```

3. Use command style output:
```bash
compiledb --command-style make
```

4. Use full compiler paths:
```bash
PATH=/opt/gcc/bin:$PATH compiledb --full-path make
```

5. Custom make target with flags:
```bash
compiledb make -f custom.mk -j8 target
```

## Performance

This Rust implementation offers several performance improvements over the original Go version:

- Zero-copy parsing where possible
- Async I/O for file operations
- Efficient string handling
- Thread-safe by design

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request. For major changes, please open an issue first to discuss what you would like to change.

## License

GNU GPLv3 - See [LICENSE](LICENSE) for details

[compdb]: https://clang.llvm.org/docs/JSONCompilationDatabase.html
