#!/bin/bash
set -e

# Build Linux x86_64 wheel with manylinux2014 compliance
# This script builds inside the manylinux2014 container to ensure glibc 2.17 compatibility

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Optional: Check for combined_certs.pem (LOCAL DEVELOPMENT ONLY)
# This is for developers behind corporate VPNs that intercept SSL.
# GitHub Actions and normal environments don't need this - the check will
# simply not find the file and skip SSL configuration (which is correct).
CERT_MOUNT=""
CERT_ENVS=""
if [ -f "$SCRIPT_DIR/combined_certs.pem" ]; then
    echo "Using combined_certs.pem for SSL (local VPN environment detected)..."
    CERT_MOUNT="-v $SCRIPT_DIR/combined_certs.pem:/tmp/combined_certs.pem:ro"
    CERT_ENVS="-e REQUESTS_CA_BUNDLE=/tmp/combined_certs.pem -e SSL_CERT_FILE=/tmp/combined_certs.pem -e CURL_CA_BUNDLE=/tmp/combined_certs.pem -e GIT_SSL_CAINFO=/tmp/combined_certs.pem"
fi

echo "Building Linux x86_64 wheel with manylinux 2014 compliance..."

docker run --rm --platform linux/amd64 \
  -v "$(pwd)":/io \
  $CERT_MOUNT \
  $CERT_ENVS \
  quay.io/pypa/manylinux2014_x86_64 bash -c "
    set -e
    
    # Configure SSL certificates for all tools if available
    if [ -f /tmp/combined_certs.pem ]; then
        echo 'Configuring SSL certificates...'
        
        # For git
        git config --global http.sslCAInfo /tmp/combined_certs.pem
        
        # For yum (CentOS 7 package manager in manylinux2014)
        # Copy custom certs to the ca-trust source directory
        cp /tmp/combined_certs.pem /etc/pki/ca-trust/source/anchors/custom-vpn-certs.pem
        
        # Update CA trust (this rebuilds the bundle)
        update-ca-trust extract
        
        echo 'SSL certificates configured.'
    fi
    
    # Install Rust stable (latest) - maturin will handle manylinux compliance
    # The manylinux2014 container ensures glibc 2.17 compatibility
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source \$HOME/.cargo/env
    
    # Configure Cargo SSL (must be done AFTER Rust installation)
    if [ -f /tmp/combined_certs.pem ]; then
        echo 'Configuring Cargo SSL certificates...'
        
        # Create cargo config directory if it does not exist
        mkdir -p \$HOME/.cargo
        
        # Configure Cargo to use custom CA bundle
        # This is critical for corporate VPNs that intercept HTTPS
        cat >> \$HOME/.cargo/config.toml <<'CARGO_EOF'

# Custom CA bundle for corporate VPN (SSL interception)
[http]
cainfo = "/tmp/combined_certs.pem"

# Increase timeout for slow VPN connections
[net]
git-fetch-with-cli = true
CARGO_EOF
        
        echo 'Cargo SSL configuration complete.'
    fi
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
