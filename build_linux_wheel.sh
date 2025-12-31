#!/bin/bash
set -e

# Build Linux x86_64 wheel with manylinux2014 compliance
# This script builds inside the manylinux2014 container to ensure glibc 2.17 compatibility

echo "Building Linux x86_64 wheel with manylinux 2014 compliance..."

docker run --rm --platform linux/amd64 -v "$(pwd)":/io quay.io/pypa/manylinux2014_x86_64 bash -c "
    # Install Rust stable (latest) - maturin will handle manylinux compliance
    # The manylinux2014 container ensures glibc 2.17 compatibility
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source \$HOME/.cargo/env
    rustc --version
    
    # Install maturin
    /opt/python/cp311-cp311/bin/pip install 'maturin>=1.8'
    
    # Build the wheel with manylinux 2014 compliance
    # Building inside manylinux2014 container ensures glibc 2.17 compatibility
    cd /io
    /opt/python/cp311-cp311/bin/maturin build --release --manylinux 2014
    
    # Ensure wheels are in target/wheels
    mkdir -p /io/target/wheels
    for wheel in \$(find /io/target -name \"*.whl\" -type f); do
        if [[ \"\$wheel\" != \"/io/target/wheels/\"* ]]; then
            cp \"\$wheel\" /io/target/wheels/
        fi
    done
"

echo "Wheels built:"
find target/wheels -name "*.whl" -type f | sort
