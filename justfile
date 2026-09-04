set minimum-version := "1.58.0"

default: list

# Show the available project commands.
list:
    @just --list

# Report the command runner and Rust toolchain versions.
versions:
    @just --version
    @rustc --version
    @cargo --version

# Check Rust formatting without changing files.
fmt-check:
    @cargo fmt --all -- --check

# Run the complete standalone test surface.
test:
    @cargo test --all-targets --all-features --locked

# Reject every Clippy warning across all targets.
lint:
    @cargo clippy --all-targets --all-features --locked -- -D warnings

# Build strict library documentation without dependency docs.
docs:
    @RUSTDOCFLAGS="-D warnings" cargo doc --lib --no-deps --all-features --locked

# Build docs and emit Cargo timing data for local profiling.
docs-timings:
    @RUSTDOCFLAGS="-D warnings" cargo doc --lib --no-deps --all-features --locked --timings

# Run the complete local verification surface.
check: fmt-check test lint docs
