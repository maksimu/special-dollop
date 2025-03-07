#!/bin/bash
set -e  # Exit on any error

echo "Cleaning previous builds..."
# Clean Rust build artifacts
cargo clean

# Make sure to remove any cached wheels, but don't error if none exist
rm -rf target/wheels/* 2>/dev/null || true
# Alternatively: if [ -d "target/wheels" ]; then rm -rf target/wheels/*; fi

echo "Building wheel..."
# Build the wheel with release configuration
maturin build --release

# Find the newly built wheel
WHEEL=$(find target/wheels -name "*.whl" | head -1)
echo "Installing wheel: $WHEEL"

# Force reinstall to ensure the latest version is used
pip uninstall -y pam_rustwebrtc || true
pip install $WHEEL --force-reinstall

echo "Running tests..."
cd tests
python -m pytest test_webrtc.py -v