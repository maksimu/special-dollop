#!/bin/bash
set -e

echo "Building keeper-pam-connections Python package (full build)"
cd crates/python-bindings
exec ./build_full.sh "$@"
