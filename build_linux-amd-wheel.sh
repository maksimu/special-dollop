#!/bin/bash
set -e

# Build Linux AMD64 wheel via Docker with maturin
#
# This script ALWAYS builds with protocol handlers (SSH, Telnet, VNC, RDP, etc.)
# Requirements:
# - pam-guacr repo checked out at ../pam-guacr
# - Cargo.toml must use path dependency for guacr
#
# This script handles:
# - Mounting local pam-guacr to avoid GitHub auth issues in Docker
# - Building FreeRDP 3.x from source for RDP support
# - SSL certificates for VPN environments
# - Cargo patch to redirect git dependency to local mount

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PAM_GUACR_DIR="$SCRIPT_DIR/../pam-guacr"

# Check if pam-guacr exists (REQUIRED for handler builds)
if [ ! -d "$PAM_GUACR_DIR" ]; then
    echo "ERROR: pam-guacr not found at $PAM_GUACR_DIR"
    echo "Clone it with: git clone git@github.com:Keeper-Security/pam-guacr.git ../pam-guacr"
    echo ""
    echo "This build ALWAYS includes protocol handlers (RDP, SSH, VNC, Telnet, etc.)"
    echo "The handlers feature can be controlled at runtime via KEEPER_GATEWAY_USE_GUACR flag."
    exit 1
fi

# Verify Cargo.toml is using local path
if ! grep -q 'guacr = { path = "../pam-guacr/crates/guacr"' "$SCRIPT_DIR/Cargo.toml"; then
    echo "ERROR: Cargo.toml must use local path for guacr dependency"
    echo "Please uncomment the local path line and comment out the git line in Cargo.toml"
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
if [ -f "$SCRIPT_DIR/combined_certs.pem" ]; then
    echo "Using combined_certs.pem for SSL (local VPN environment detected)..."
    CERT_MOUNT="-v $SCRIPT_DIR/combined_certs.pem:/tmp/combined_certs.pem:ro"
    CERT_ENVS="-e REQUESTS_CA_BUNDLE=/tmp/combined_certs.pem -e SSL_CERT_FILE=/tmp/combined_certs.pem -e CURL_CA_BUNDLE=/tmp/combined_certs.pem -e GIT_SSL_CAINFO=/tmp/combined_certs.pem"
fi

# Build with handlers using manylinux_2_28 (AlmaLinux 8) for better compatibility
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
        
        # Try to install FFmpeg if available - optional for audio codecs
        echo "Attempting to install FFmpeg - optional for enhanced audio codecs..."
        dnf install -y ffmpeg-free-devel || \
        dnf install -y ffmpeg-devel || \
        echo "FFmpeg not available - will build without FFmpeg codec support"
        
        # Build FreeRDP 3.x from source - optimized for speed
        echo "Building FreeRDP 3.x from source - this takes 5-10 minutes..."
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
            echo "FFmpeg not found, building without FFmpeg - ALSA/PulseAudio still enabled"
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
        NPROC=$(nproc)
        echo "Compiling FreeRDP 3.x using $NPROC cores..."
        ninja -j$NPROC || {
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
            FREERDP_VER=$(pkg-config --modversion freerdp3)
            echo "FreeRDP 3.x installed successfully! Version: $FREERDP_VER"
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
        
        echo "IronRDP will be fetched from git during build from https://github.com/miroberts/IronRDP.git"
        
        # Install maturin
        echo "Installing maturin..."
        /opt/python/cp311-cp311/bin/pip install "maturin>=1.8,<1.9"
        
        # Build with handlers - use manylinux_2_28 (better compatibility than 2_34)
        echo "Building wheel with handlers..."
        
        # Ensure PKG_CONFIG_PATH is set for cargo to find FreeRDP 3.x
        export PKG_CONFIG_PATH="/usr/local/lib64/pkgconfig:/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH"
        
        cd /io && /opt/python/cp311-cp311/bin/maturin build --release --manylinux 2_28
        
        echo "Build complete!"
        ls -la /io/target/wheels/
      '

echo "Build complete! Wheels are in target/wheels/"