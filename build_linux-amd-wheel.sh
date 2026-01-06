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

# Parse command line arguments for handler support
HANDLERS_FLAG=""
if [ "$1" = "--handlers" ]; then
    HANDLERS_FLAG="--handlers"
    # Check if pam-guacr exists when building with handlers
    if [ ! -d "$PAM_GUACR_DIR" ]; then
        echo "ERROR: pam-guacr not found at $PAM_GUACR_DIR"
        echo "Clone it with: git clone git@github.com:Keeper-Security/pam-guacr.git ../pam-guacr"
        exit 1
    fi
    
    # Verify Cargo.toml is using local path
    if ! grep -q 'guacr = { path = "../pam-guacr/crates/guacr"' "$SCRIPT_DIR/Cargo.toml"; then
        echo "ERROR: Cargo.toml must use local path for guacr dependency"
        echo "Please uncomment the local path line and comment out the git line in Cargo.toml"
        exit 1
    fi
fi

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

if [ -n "$HANDLERS_FLAG" ]; then
    echo "Building with protocol handlers (SSH, Telnet, etc)..."
    
    # For handlers, we need a shell to set up cargo config patches
    # Use manylinux_2_28 (AlmaLinux 8) for better compatibility
    docker run --rm --platform linux/amd64 \
      -v "$SCRIPT_DIR":/io \
      -v "$PAM_GUACR_DIR":/pam-guacr:ro \
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
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        source $HOME/.cargo/env
        
        # Configure Cargo SSL (must be done AFTER Rust installation)
        if [ -f /tmp/combined_certs.pem ]; then
            echo "Configuring Cargo SSL certificates..."
            
            # Create cargo config directory if it does not exist
            mkdir -p $HOME/.cargo
            
            # Configure Cargo to use custom CA bundle
            # This is critical for corporate VPNs that intercept HTTPS
            cat >> $HOME/.cargo/config.toml <<EOF

# Custom CA bundle for corporate VPN (SSL interception)
[http]
cainfo = "/tmp/combined_certs.pem"

# Increase timeout for slow VPN connections
[net]
git-fetch-with-cli = true
EOF
            
            echo "Cargo SSL configuration complete."
        fi
        
        # manylinux_2_28 is based on AlmaLinux 8
        # AlmaLinux 8 only has FreeRDP 2.x, so we build FreeRDP 3.x from source
        echo "Installing development packages..."
        
        # Enable EPEL and PowerTools for additional packages
        dnf install -y epel-release
        dnf config-manager --set-enabled powertools || dnf config-manager --set-enabled crb || true
        
        # Install OpenSSL (required by native-tls/openssl-sys)
        dnf install -y openssl-devel
        
        # Install FreeRDP 3.x build dependencies (with audio support)
        echo "Installing FreeRDP 3.x build dependencies..."
        dnf install -y \
            cmake3 \
            ninja-build \
            gcc \
            gcc-c++ \
            git \
            pkgconfig \
            openssl-devel \
            libX11-devel \
            libXcursor-devel \
            libXext-devel \
            libXi-devel \
            libXinerama-devel \
            libXrandr-devel \
            libXv-devel \
            libxkbfile-devel \
            alsa-lib-devel \
            pulseaudio-libs-devel \
            cups-devel \
            libjpeg-turbo-devel \
            libusb-devel \
            pam-devel \
            systemd-devel \
            wayland-devel \
            libicu-devel \
            fuse3-devel \
            clang-devel \
            llvm-devel
        
        # Create symlink for cmake if cmake3 was installed
        if command -v cmake3 &> /dev/null && ! command -v cmake &> /dev/null; then
            ln -sf /usr/bin/cmake3 /usr/bin/cmake
        fi
        
        # Verify cmake and ninja are available
        echo "Verifying build tools..."
        cmake --version || { echo "ERROR: cmake not found"; exit 1; }
        ninja --version || { echo "ERROR: ninja not found"; exit 1; }
        
        # Try to install FFmpeg if available (optional for audio codecs)
        echo "Attempting to install FFmpeg (optional for enhanced audio codecs)..."
        dnf install -y ffmpeg-free-devel || \
        dnf install -y ffmpeg-devel || \
        echo "FFmpeg not available - will build without FFmpeg codec support"
        
        # Build FreeRDP 3.x from source (optimized for speed)
        echo "Building FreeRDP 3.x from source (this takes ~5-10 minutes)..."
        cd /tmp
        
        # Clone FreeRDP 3.x stable release
        git clone --depth 1 --branch 3.10.2 https://github.com/FreeRDP/FreeRDP.git freerdp || {
            echo "ERROR: Failed to clone FreeRDP"
            exit 1
        }
        
        cd freerdp
        mkdir build
        cd build
        
        # Configure FreeRDP with audio support enabled
        # Check if FFmpeg is available
        FFMPEG_OPTION=OFF
        if pkg-config --exists libavcodec libavutil; then
            echo "FFmpeg detected, enabling FFmpeg support"
            FFMPEG_OPTION=ON
        else
            echo "FFmpeg not found, building without FFmpeg (ALSA/PulseAudio still enabled)"
        fi
        
        cmake .. \
            -GNinja \
            -DCMAKE_BUILD_TYPE=Release \
            -DCMAKE_INSTALL_PREFIX=/usr/local \
            -DWITH_WAYLAND=OFF \
            -DWITH_X11=ON \
            -DWITH_CUPS=OFF \
            -DWITH_FFMPEG=$FFMPEG_OPTION \
            -DWITH_GSTREAMER_1_0=OFF \
            -DWITH_PULSE=ON \
            -DWITH_ALSA=ON \
            -DWITH_OSS=OFF \
            -DWITH_PCSC=OFF \
            -DWITH_PKCS11=OFF \
            -DWITH_SWSCALE=OFF \
            -DWITH_SERVER=OFF \
            -DWITH_SAMPLE=OFF \
            -DWITH_SHADOW=OFF \
            -DBUILD_TESTING=OFF \
            -DWITH_MANPAGES=OFF \
            -DWITH_KRB5=OFF \
            -DWITH_CLIENT=OFF \
            -DWITH_CLIENT_SDL=OFF \
            || {
            echo "ERROR: FreeRDP cmake configuration failed"
            exit 1
        }
        
        # Build with all available cores
        echo "Compiling FreeRDP 3.x (using $(nproc) cores)..."
        ninja -j$(nproc) || {
            echo "ERROR: FreeRDP build failed"
            exit 1
        }
        
        # Install
        echo "Installing FreeRDP 3.x..."
        ninja install || {
            echo "ERROR: FreeRDP install failed"
            exit 1
        }
        
        # Update library cache
        ldconfig
        
        # Verify installation
        echo "Verifying FreeRDP 3.x installation..."
        export PKG_CONFIG_PATH="/usr/local/lib64/pkgconfig:/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH"
        
        if pkg-config --exists freerdp3; then
            echo "FreeRDP 3.x installed successfully! Version: $(pkg-config --modversion freerdp3)"
        else
            echo "ERROR: FreeRDP 3.x not found by pkg-config"
            echo "PKG_CONFIG_PATH=$PKG_CONFIG_PATH"
            ls -la /usr/local/lib*/pkgconfig/freerdp* || true
            exit 1
        fi
        
        # Return to /io
        cd /io
        
        # Verify pam-guacr is mounted
        if [ ! -d /pam-guacr/crates/guacr ]; then
            echo "ERROR: pam-guacr not properly mounted at /pam-guacr"
            exit 1
        fi
        
        echo "pam-guacr mounted successfully at /pam-guacr"
        ls -la /pam-guacr/crates/
        
        # Install maturin
        echo "Installing maturin..."
        /opt/python/cp311-cp311/bin/pip install "maturin>=1.8,<1.9"
        
        # Build with handlers - use manylinux_2_28 (better compatibility than 2_34)
        echo "Building wheel with handlers..."
        
        # Ensure PKG_CONFIG_PATH is set for cargo to find FreeRDP 3.x
        export PKG_CONFIG_PATH="/usr/local/lib64/pkgconfig:/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH"
        
        cd /io && /opt/python/cp311-cp311/bin/maturin build --release --features handlers --manylinux 2_28
        
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