set windows-shell := ["powershell.exe", "-NoProfile", "-Command"]

# List recipes
default:
    @just --list

# Release build
build:
    cargo build --release

# Run the test suite
test:
    cargo test

# Clippy on all targets, warnings are errors
lint:
    cargo clippy --all-targets -- -D warnings

# Print battery levels once and exit
once:
    cargo run -- --once
