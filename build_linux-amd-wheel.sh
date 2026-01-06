#!/bin/bash
set -e

# Build Linux AMD64 wheel via Docker with maturin
#
# This script handles:
# - Mounting local pam-guacr to avoid GitHub auth issues in Docker
# - SSL certificates for VPN environments
# - Cargo patch to redirect git dependency to local mount

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PAM_GUACR_DIR="$SCRIPT_DIR/../pam-guacr"

# Check if pam-guacr exists when building with handlers
if [ "$1" = "--handlers" ] && [ ! -d "$PAM_GUACR_DIR" ]; then
    echo "ERROR: pam-guacr not found at $PAM_GUACR_DIR"
    echo "Clone it with: git clone git@github.com:Keeper-Security/pam-guacr.git ../pam-guacr"
    exit 1
fi

# Check if combined_certs.pem exists (for VPN)
CERT_MOUNT=""
CERT_ENVS=""
if [ -f "$SCRIPT_DIR/combined_certs.pem" ]; then
    echo "Using combined_certs.pem for SSL..."
    CERT_MOUNT="-v $SCRIPT_DIR/combined_certs.pem:/tmp/combined_certs.pem:ro"
    CERT_ENVS="-e REQUESTS_CA_BUNDLE=/tmp/combined_certs.pem -e SSL_CERT_FILE=/tmp/combined_certs.pem -e CURL_CA_BUNDLE=/tmp/combined_certs.pem -e GIT_SSL_CAINFO=/tmp/combined_certs.pem"
fi

# Parse command line arguments for handler support
if [ "$1" = "--handlers" ]; then
    echo "Building with protocol handlers (SSH, Telnet, etc)..."
    
    # For handlers, we need a shell to set up cargo config patches
    # Use the manylinux image directly with bash
    docker run --rm --platform linux/amd64 \
      -v "$SCRIPT_DIR":/io \
      -v "$PAM_GUACR_DIR":/pam-guacr \
      $CERT_MOUNT \
      $CERT_ENVS \
      quay.io/pypa/manylinux2014_x86_64 bash -c '
        set -e
        
        # Configure git SSL if certs available
        if [ -f /tmp/combined_certs.pem ]; then
            git config --global http.sslCAInfo /tmp/combined_certs.pem
        fi
        
        # Install OpenSSL development headers (required by openssl-sys crate)
        echo "Installing OpenSSL development packages..."
        yum install -y openssl-devel perl-IPC-Cmd
        
        # Install Rust
        echo "Installing Rust..."
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        source $HOME/.cargo/env
        
        # Create Cargo config to use local pam-guacr instead of git
        mkdir -p $HOME/.cargo
        cat >> $HOME/.cargo/config.toml << EOF
[net]
git-fetch-with-cli = true

# Patch guacr to use local mount (avoids GitHub auth in Docker)
# Note: pam-guacr is a workspace, the actual crate is in crates/guacr
[patch."https://github.com/Keeper-Security/pam-guacr.git"]
guacr = { path = "/pam-guacr/crates/guacr" }
EOF
        
        echo "Cargo config:"
        cat $HOME/.cargo/config.toml
        
        # Install maturin
        echo "Installing maturin..."
        /opt/python/cp311-cp311/bin/pip install "maturin>=1.8,<1.9"
        
        # Build with handlers
        echo "Building wheel with handlers..."
        cd /io && /opt/python/cp311-cp311/bin/maturin build --release --features handlers --manylinux 2014
        
        echo "Build complete!"
        ls -la /io/target/wheels/
      '
else
    echo "Building without handlers (default)..."
    docker run --rm --platform linux/amd64 \
      -v "$SCRIPT_DIR":/io \
      $CERT_MOUNT \
      $CERT_ENVS \
      ghcr.io/pyo3/maturin build --release --manylinux 2014
fi

echo "Build complete! Wheels are in target/wheels/"