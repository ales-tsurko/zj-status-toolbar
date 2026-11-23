# Windows uses Git Bash with the native MSVC Rust toolchain.
set minimum-version := "1.59.0"
set shell := ["bash", "-euo", "pipefail", "-c"]
set default-list

cargo-packages := env("CARGO_PACKAGES", "--workspace")
coverage-lcov := env("COVERAGE_LCOV", "target/coverage.lcov")
crap-report := env("CRAP_REPORT", "target/crap.md")
crap-excludes := "--exclude '**/build.rs' --exclude '**/benches/**' --exclude '**/examples/**' --exclude '**/tests.rs' --exclude '**/*_tests.rs' --exclude '**/tests/**'"
# Override this when adding crates outside src/.
similarity-args := env("SIMILARITY_ARGS", "src --threshold 0.92 --min-lines 12 --min-tokens 80 --fail-on-duplicates")
# Share the same unversioned tool installation between local setup and CI.
platform-tools := "cargo-nextest rumdl"
quality-tools := "cargo-llvm-cov cargo-crap similarity-rs cargo-machete"

# Show the available commands, also the default behavior.
help:
    @just --list

# Prepare development tooling.
init: _init-platform _init-quality

# Install the components and tools needed for check, test, and lint.
_init-platform: _stable _rustfmt _platform-tools

# Install the stable components and tools needed for code-health.
_init-quality: _stable _platform-tools _quality-tools

# Check all workspace features and targets.
check:
    cargo check {{cargo-packages}} --all-features --all-targets

# Build the workspace with all features.
build:
    cargo build {{cargo-packages}} --all-features

# Build the Zellij plugin.
wasm:
    cargo build --locked --release --target wasm32-wasip1

# Run tests with nextest, using cargo test for --doc.
[positional-arguments]
test *args:
    @doc_tests=false; \
    for arg in "$@"; do \
        case "$arg" in \
            --) break ;; \
            --doc) doc_tests=true ;; \
        esac; \
    done; \
    if [ "$#" -eq 0 ]; then \
        cargo nextest run {{cargo-packages}} --all-features --all-targets; \
        cargo test {{cargo-packages}} --all-features --doc; \
    elif [ "$doc_tests" = true ]; then \
        cargo test {{cargo-packages}} --all-features "$@"; \
    else \
        cargo nextest run {{cargo-packages}} --all-features "$@"; \
    fi

# Check Clippy, Rust/Markdown formatting, and rustdoc warnings.
lint:
    @status=0; \
    cargo clippy {{cargo-packages}} --all-targets --all-features -- -D warnings || status=$?; \
    cargo +nightly fmt --check --all || status=$?; \
    rumdl check . || status=$?; \
    rumdl fmt --check . || status=$?; \
    RUSTDOCFLAGS="-D warnings" cargo doc {{cargo-packages}} --all-features \
        --no-deps --document-private-items || status=$?; \
    exit $status

# Format Rust and Markdown.
fmt:
    cargo +nightly fmt --all
    rumdl fmt .

# Check duplicates, unused dependencies, and coverage/CRAP.
code-health: _duplicates _unused-deps _coverage _crap

_ci: check test lint

_stable:
    rustup toolchain install stable --component clippy --component llvm-tools-preview

_rustfmt:
    rustup toolchain install nightly --component rustfmt

_binstall:
    @if ! command -v cargo-binstall >/dev/null 2>&1; then \
        cargo install --locked cargo-binstall; \
    fi

_platform-tools: _binstall
    cargo binstall --no-confirm --locked {{platform-tools}}

_quality-tools: _binstall
    cargo binstall --no-confirm --locked {{quality-tools}}

_duplicates:
    similarity-rs {{similarity-args}}

_unused-deps:
    cargo machete --skip-target-dir

_coverage:
    cargo llvm-cov nextest {{cargo-packages}} --all-features --all-targets --lcov --output-path {{quote(coverage-lcov)}}

_crap:
    cargo crap --workspace --lcov {{quote(coverage-lcov)}} {{crap-excludes}} --format markdown --output {{quote(crap-report)}}
    cargo crap --workspace --lcov {{quote(coverage-lcov)}} {{crap-excludes}} --summary --fail-above
