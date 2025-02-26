#!/bin/bash
# Build the wheel
maturin build --release

# Install the wheel
pip install $(find target/wheels -name "*.whl" | head -1) --force-reinstall

# Run tests
cd tests
python -m pytest test_webrtc.py -v