#!/bin/bash
set -e  # Exit on any error

# Comprehensive build script that:
# 1. Runs code quality checks (fmt, clippy)
# 2. Runs Rust unit tests
# 3. Builds and tests macOS wheel
# 4. Builds Linux AMD64 wheel (via Docker)

echo "========================================"
echo "COMPREHENSIVE BUILD AND TEST PIPELINE"
echo "========================================"
echo ""

echo "Building with protocol handlers (SSH, Telnet, VNC, RDP, etc) - always included"
echo ""

# ============================================================================
# PHASE 1: CODE QUALITY CHECKS
# ============================================================================
echo "========================================"
echo "PHASE 1: Code Quality Checks"
echo "========================================"

echo "Running cargo fmt check..."
if ! cargo fmt --all -- --check; then
    echo "ERROR: Code formatting check failed!"
    echo "Run 'cargo fmt --all' to fix formatting issues."
    exit 1
fi
echo "✓ Formatting check passed"
echo ""

echo "Running cargo clippy..."
if ! cargo clippy -- -D warnings; then
    echo "ERROR: Clippy linting failed!"
    echo "Fix the warnings above before proceeding."
    exit 1
fi
echo "✓ Clippy check passed"
echo ""

# ============================================================================
# PHASE 2: RUST UNIT TESTS
# ============================================================================
echo "========================================"
echo "PHASE 2: Rust Unit Tests"
echo "========================================"

echo "Running Rust unit tests (library only, no Python bindings)..."
cargo test --lib --no-default-features
echo "✓ Rust unit tests passed"
echo ""

# ============================================================================
# PHASE 3: BUILD AND TEST MACOS WHEEL
# ============================================================================
echo "========================================"
echo "PHASE 3: Build and Test macOS Wheel"
echo "========================================"

echo "Cleaning previous builds..."
cargo clean
rm -rf target/wheels && mkdir -p target/wheels

echo "Building macOS wheel..."
maturin build --release

# Find the newly built wheel
WHEEL=$(find target/wheels -name "*.whl" | head -1)
if [ -z "$WHEEL" ]; then
    echo "ERROR: No wheel found after build!"
    exit 1
fi
echo "Built wheel: $WHEEL"

echo "Installing wheel..."
pip uninstall -y keeper_pam_webrtc_rs || true
pip install "$WHEEL" --force-reinstall

echo "Running Python tests..."
cd tests
export RUST_BACKTRACE=1
python3 -m pytest -v --log-cli-level=DEBUG
cd ..
echo "✓ macOS wheel build and test passed"
echo ""

# ============================================================================
# PHASE 4: BUILD LINUX AMD64 WHEEL
# ============================================================================
echo "========================================"
echo "PHASE 4: Build Linux AMD64 Wheel"
echo "========================================"

echo "Building Linux AMD64 wheel via Docker..."
if [ ! -f "build_linux-amd-wheel.sh" ]; then
    echo "ERROR: build_linux-amd-wheel.sh not found!"
    exit 1
fi

./build_linux-amd-wheel.sh
echo "✓ Linux AMD64 wheel build completed"
echo ""

# ============================================================================
# SUMMARY
# ============================================================================
echo "========================================"
echo "BUILD PIPELINE COMPLETE!"
echo "========================================"
echo ""
echo "Summary of built wheels:"
echo "------------------------"
find target/wheels -name "*.whl" -type f | while read wheel; do
    echo "  - $(basename $wheel)"
done
echo ""
echo "All checks passed:"
echo "  ✓ Code formatting (cargo fmt)"
echo "  ✓ Linting (cargo clippy)"
echo "  ✓ Rust unit tests (cargo test --lib)"
echo "  ✓ macOS wheel + Python tests"
echo "  ✓ Linux AMD64 wheel (Docker)"
echo ""
echo "Ready for release!"
