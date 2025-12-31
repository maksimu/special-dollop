#!/bin/bash
set -e  # Exit on any error

# TUBE LIFECYCLE NOTES:
# - Multiple "Drop called for tube" messages are NORMAL and expected
# - They represent Arc reference drops, not premature tube destruction
# - Tubes remain fully functional after these drops
# - The actual tube cleanup only happens when marked Closed and removed from registry
# - Look for "TUBE CLEANUP COMPLETE" message to confirm full cleanup

echo "Cleaning previous builds..."
# Clean Rust build artifacts
cargo clean

# Make sure to remove any cached wheels, but don't error if none exist
rm -rf target/wheels && mkdir -p target/wheels
# Alternatively: if [ -d "target/wheels" ]; then rm -rf target/wheels/*; fi

echo "Building wheel..."
# Detect platform and build accordingly
if [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS build - skip manylinux checks (not applicable to macOS)
    echo "Building for macOS (native platform)..."
    maturin build --release --auditwheel skip
elif [[ "$OSTYPE" == "linux-gnu"* ]]; then
    # Linux build - check if we want manylinux compliance
    # Use explicit check to only enable when BUILD_MANYLINUX=1 (not just any non-empty value)
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

# Find the newly built wheel
WHEEL=$(find target/wheels -name "*.whl" | head -1)
echo "Installing wheel: $WHEEL"

# Force reinstall to ensure the latest version is used
pip uninstall -y keeper_pam_webrtc_rs || true
pip install "$WHEEL" --force-reinstall

echo "Running tests..."
cd tests

# Run all tests
export RUST_BACKTRACE=1
python3 -m pytest -v --log-cli-level=DEBUG
