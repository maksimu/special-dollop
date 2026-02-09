#!/bin/bash
set -e

# Build Linux AMD64 wheel via Docker with maturin
#
# This script ALWAYS builds with protocol handlers (SSH, Telnet, VNC, RDP, etc.)
# Requirements:
# - guacr crate is part of the monorepo workspace at crates/guacr
#
# This script handles:
# - Mounting the entire workspace for access to all crates
# - SSL certificates for VPN environments
#
# Note: RDP uses IronRDP (pure Rust) - no C FreeRDP dependency needed.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# guacr is now part of the monorepo at crates/guacr
# Verify it exists
if [ ! -d "$WORKSPACE_ROOT/crates/guacr" ]; then
    echo "ERROR: guacr not found at $WORKSPACE_ROOT/crates/guacr"
    echo "The guacr crate should be part of the monorepo workspace."
    exit 1
fi

echo "Building with protocol handlers (SSH, Telnet, VNC, RDP, etc.)..."
echo "Note: IronRDP will be fetched from GitHub (https://github.com/miroberts/IronRDP.git)"

# Optional: Check for combined_certs.pem (LOCAL DEVELOPMENT ONLY)
# This is for developers behind corporate VPNs that intercept SSL.
# GitHub Actions and normal environments don't need this - the check will
# simply not find the file and skip SSL configuration (which is correct).
CERT_MOUNT=""
CERT_ENVS=""
if [ -f "$WORKSPACE_ROOT/combined_certs.pem" ]; then
    echo "Using combined_certs.pem for SSL (local VPN environment detected)..."
    CERT_MOUNT="-v $WORKSPACE_ROOT/combined_certs.pem:/tmp/combined_certs.pem:ro"
    CERT_ENVS="-e REQUESTS_CA_BUNDLE=/tmp/combined_certs.pem -e SSL_CERT_FILE=/tmp/combined_certs.pem -e CURL_CA_BUNDLE=/tmp/combined_certs.pem -e GIT_SSL_CAINFO=/tmp/combined_certs.pem"
fi

# Build with handlers using manylinux_2_28 (AlmaLinux 8) for better compatibility
# Mount the workspace root at /io so maturin can find the workspace
docker run --rm --platform linux/amd64 \
  -v "$WORKSPACE_ROOT":/io \
  $CERT_MOUNT \
  $CERT_ENVS \
  quay.io/pypa/manylinux_2_28_x86_64 bash -c '
        set -e
        
        # Configure SSL certificates for all tools if available
        if [ -f /tmp/combined_certs.pem ]; then
            echo "Configuring SSL certificates..."
            
            # For git
            git config --global http.sslCAInfo /tmp/combined_certs.pem
            
            # For dnf (AlmaLinux package manager)
            # Copy custom certs to the ca-trust source directory
            cp /tmp/combined_certs.pem /etc/pki/ca-trust/source/anchors/custom-vpn-certs.pem
            
            # Update CA trust (this rebuilds the bundle)
            update-ca-trust extract
            
            echo "SSL certificates configured. Testing with curl..."
            curl -I https://mirrors.almalinux.org/ || echo "Curl test failed, but continuing..."
        fi
        
        # Install Rust BEFORE configuring cargo (needs ~/.cargo to exist)
        echo "Installing Rust..."
        # If using custom certs, configure environment for rustup installer
        if [ -f /tmp/combined_certs.pem ]; then
            echo "Configuring rustup to use custom CA bundle..."
            # Point to the system CA bundle that includes our custom cert
            export SSL_CERT_FILE=/etc/pki/tls/certs/ca-bundle.crt
            export CURL_CA_BUNDLE=/etc/pki/tls/certs/ca-bundle.crt
            # Increase timeout for slow VPN connections (30 seconds per download)
            export RUSTUP_IO_THREADS=1
            export RUSTUP_UNPACK_RAM=536870912
            export RUSTUP_DIST_SERVER=https://static.rust-lang.org
            
            # Rustup reqwest backend does not respect SSL_CERT_FILE properly in containers
            # Workaround: Use curl to pre-download Rust, then install offline
            echo "Pre-downloading Rust toolchain with curl - VPN-aware..."
            
            # Get the latest stable version info
            echo "Fetching Rust version info..."
            curl --proto "=https" --tlsv1.2 -sSf --connect-timeout 30 --max-time 60 \
                -o /tmp/channel-rust-stable.toml \
                https://static.rust-lang.org/dist/channel-rust-stable.toml
            
            # Extract version from TOML - get it from the actual URL, not the version field
            # The version field is cargo'\''s version, but we need rustc'\''s version from the URL
            RUST_VERSION=$(grep "url.*rustc.*x86_64-unknown-linux-gnu.tar" /tmp/channel-rust-stable.toml | head -1 | sed -E '\''s|.*rustc-([0-9]+\.[0-9]+\.[0-9]+)-.*|\1|'\'')
            echo "Latest Rust version: $RUST_VERSION"
            
            # Download rustup-init
            echo "Downloading rustup-init..."
            curl --proto "=https" --tlsv1.2 -sSf --connect-timeout 30 --max-time 300 \
                -o /tmp/rustup-init https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init
            chmod +x /tmp/rustup-init
            
            # Install rustup without installing a toolchain
            echo "Installing rustup without toolchain..."
            /tmp/rustup-init -y --default-toolchain none --profile minimal
            source $HOME/.cargo/env
            
            # Download Rust components with curl
            RUST_DIST="https://static.rust-lang.org/dist"
            RUST_TARGET="x86_64-unknown-linux-gnu"
            RUST_DATE=$(grep "^date = " /tmp/channel-rust-stable.toml | head -1 | cut -d\" -f2)
            
            echo "Downloading Rust $RUST_VERSION components with curl..."
            mkdir -p /tmp/rust-dist
            
            # Download cargo (with retry and HTTP/1.1 for stability)
            echo "  - cargo..."
            curl --proto "=https" --tlsv1.2 --http1.1 -sSfL --connect-timeout 30 --max-time 900 \
                --retry 3 --retry-delay 5 --retry-max-time 1800 \
                -o /tmp/rust-dist/cargo.tar.xz \
                "$RUST_DIST/$RUST_DATE/cargo-$RUST_VERSION-$RUST_TARGET.tar.xz"
            
            # Download rustc
            echo "  - rustc..."
            curl --proto "=https" --tlsv1.2 --http1.1 -sSfL --connect-timeout 30 --max-time 900 \
                --retry 3 --retry-delay 5 --retry-max-time 1800 \
                -o /tmp/rust-dist/rustc.tar.xz \
                "$RUST_DIST/$RUST_DATE/rustc-$RUST_VERSION-$RUST_TARGET.tar.xz"
            
            # Download rust-std
            echo "  - rust-std..."
            curl --proto "=https" --tlsv1.2 --http1.1 -sSfL --connect-timeout 30 --max-time 900 \
                --retry 3 --retry-delay 5 --retry-max-time 1800 \
                -o /tmp/rust-dist/rust-std.tar.xz \
                "$RUST_DIST/$RUST_DATE/rust-std-$RUST_VERSION-$RUST_TARGET.tar.xz"
            
            # Extract and install components manually
            echo "Installing Rust components..."
            cd /tmp/rust-dist
            
            for component in cargo rustc rust-std; do
                echo "  Installing $component..."
                tar -xf $component.tar.xz
                cd ${component}-*
                ./install.sh --prefix=$HOME/.cargo
                cd ..
            done
            
            echo "Rust installation complete via offline method"
            rustc --version
            cargo --version
        else
            # Standard installation without custom certs
            curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        fi
        source $HOME/.cargo/env
        
        # Install minimal system dependencies
        # RDP uses IronRDP (pure Rust) so no C FreeRDP library is needed
        echo "Installing development packages..."
        dnf install -y openssl-devel git gcc gcc-c++ pkgconfig perl-IPC-Cmd

        # Verify workspace is mounted with guacr crate
        if [ ! -d /io/crates/guacr ]; then
            echo "ERROR: workspace not properly mounted at /io or guacr crate missing"
            exit 1
        fi

        echo "Workspace mounted successfully at /io"
        echo "Available crates:"
        ls -la /io/crates/
        
        # Install maturin
        echo "Installing maturin..."
        /opt/python/cp311-cp311/bin/pip install "maturin>=1.8,<1.9"

        echo "Building wheel..."
        cd /io/crates/python-bindings
        /opt/python/cp311-cp311/bin/maturin build --release --manylinux 2_28

        echo "Build complete!"
        ls -la /io/target/wheels/
      '

echo "Build complete! Wheels are in target/wheels/"