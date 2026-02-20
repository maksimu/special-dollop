#!/bin/bash
set -e

echo "Building Linux wheel for keeper-pam-connections"
cd crates/python-bindings
exec ./build_linux_wheel.sh "$@"
