#!/bin/bash
set -e

echo "Building Linux AMD64 wheel for keeper-pam-connections"
cd crates/python-bindings
exec ./build_linux-amd-wheel.sh "$@"
