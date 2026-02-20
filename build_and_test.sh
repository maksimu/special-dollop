#!/bin/bash
set -e  # Exit on any error

# WORKSPACE BUILD SCRIPT
# This script builds and tests the unified keeper-pam-connections Python package
#
# Prerequisites:
#   - Rust toolchain (rustup, cargo, rustfmt, clippy)
#   - Python 3.7+ with pip and maturin
#   - unixODBC (for guacr-database ODBC handler):
#       macOS:  brew install unixodbc
#       Ubuntu: apt-get install unixodbc-dev
#       RHEL:   dnf install unixODBC-devel
#   - Oracle Instant Client (optional, for Oracle DB handler)
#   - Docker (optional, for database integration tests):
#       docker compose -f crates/guacr-database/docker-compose.test.yml up -d
#       Starts: MySQL, MariaDB, PostgreSQL, SQL Server, MongoDB, DynamoDB,
#               Cassandra, Elasticsearch, Redis, ODBC-PostgreSQL

echo "Building keeper-pam-connections (unified Python package)"
echo ""

echo "========================================"
echo "Running code quality checks..."
echo "========================================"

echo "Checking code formatting..."
cargo fmt --all -- --check
echo "✓ Formatting check passed"
echo ""

echo "Running clippy..."
cargo clippy --workspace --all-targets --all-features -- -D warnings
echo "✓ Clippy check passed"
echo ""

echo "Running Rust unit tests..."
# Test keeper-pam-webrtc-rs without Python support (pure Rust tests)
cargo test -p keeper-pam-webrtc-rs --lib --no-default-features
# Test all other workspace crates with all features (excluding python-bindings which needs maturin)
cargo test --workspace --lib --all-features --exclude keeper-pam-webrtc-rs --exclude keeper-pam-connections-py
echo "✓ Rust tests passed"
echo ""

echo "========================================"
echo "Building Python package..."
echo "========================================"

cd crates/python-bindings

echo "Cleaning previous builds..."
# Clean Rust build artifacts
cargo clean

# Make sure to remove any cached wheels, but don't error if none exist
rm -rf target/wheels && mkdir -p target/wheels

echo "Building wheel..."
# Detect platform and build accordingly
if [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS build - skip manylinux checks (not applicable to macOS)
    echo "Building for macOS (native platform)..."
    maturin build --release --auditwheel skip
elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
    # Linux build - check if we want manylinux compliance
    if [ "$BUILD_MANYLINUX" = "1" ]; then
        echo "Building Linux wheel with manylinux_2_28 compliance (using Docker)..."
        docker run --rm -v "$(pwd)":/io ghcr.io/pyo3/maturin:v1.8.1 build --release --manylinux 2_28
    else
        echo "Building Linux wheel without manylinux checks (for local testing only)..."
        echo "Note: For manylinux-compliant wheels, use: BUILD_MANYLINUX=1 ./build_and_test.sh"
        maturin build --release --auditwheel skip
    fi
else
    # Other platforms (Windows, etc.)
    echo "Building for $OSTYPE..."
    maturin build --release --auditwheel skip
fi

# Find the newly built wheel (maturin puts it in workspace root target/wheels)
WHEEL=$(find ../../target/wheels -name "*.whl" 2>/dev/null | head -1)

if [ -z "$WHEEL" ]; then
    echo "ERROR: No wheel found in ../../target/wheels/"
    ls -la ../../target/wheels/ || echo "../../target/wheels/ directory not found"
    exit 1
fi

echo "Installing wheel: $WHEEL"

# Force reinstall to ensure the latest version is used
pip uninstall -y keeper_pam_connections || true
pip install "$WHEEL" --force-reinstall

echo "========================================"
echo "Running Python tests..."
echo "========================================"

cd tests

# Run all tests
export RUST_BACKTRACE=1
python3 -m pytest -v --log-cli-level=DEBUG

echo ""
echo "========================================"
echo "✓ All checks passed!"
echo "========================================"
