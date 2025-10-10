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
# Build the wheel with release configuration
maturin build --release

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
