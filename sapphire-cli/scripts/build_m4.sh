#!/bin/sh
# Fallback build script for GNU M4 v1.4.19

# Exit immediately if a command exits with a non-zero status or uses an unset variable.
set -eu

# --- Arguments ---
BUILD_DIR="$1"
INSTALL_PREFIX="$2"

# Basic validation
if [ -z "$BUILD_DIR" ] || [ -z "$INSTALL_PREFIX" ]; then
  echo "Usage: $0 <build_dir> <install_prefix>" >&2
  exit 1
fi
if [ ! -d "$BUILD_DIR" ]; then
  echo "Error: Build directory '$BUILD_DIR' not found." >&2
  exit 1
fi

echo "[Build Script - m4] Starting build in $BUILD_DIR"
echo "[Build Script - m4] Installing to $INSTALL_PREFIX"

# --- Build Steps ---
# Navigate into the build directory (where source was extracted)
cd "$BUILD_DIR"

# Configure
echo "[Build Script - m4] Running configure..."
# Pass CC/CFLAGS/etc. from environment if set by Rust parent process?
# For now, keep it simple. Add env vars here if needed: CC="clang" CFLAGS="-O2" ./configure ...
./configure --prefix="$INSTALL_PREFIX"

# Compile
echo "[Build Script - m4] Running make..."
make -j$(sysctl -n hw.ncpu || echo 1) # Use multiple cores if possible

# Test (Recommended)
echo "[Build Script - m4] Running make check..."
if make -j$(sysctl -n hw.ncpu || echo 1) check; then
  echo "[Build Script - m4] 'make check' successful."
else
  # Don't exit on test failure, just warn
  echo "[Build Script - m4] Warning: 'make check' failed. Installation will continue." >&2
fi

# Install (using sudo if prefix requires it)
echo "[Build Script - m4] Running make install..."
# Check if the PARENT directory of the prefix is writable. If not, assume sudo needed.
INSTALL_PARENT=$(dirname "$INSTALL_PREFIX")
if [ ! -w "$INSTALL_PARENT" ] && [ "$(id -u)" != "0" ]; then
    echo "[Build Script - m4] Install prefix parent '$INSTALL_PARENT' not writable. Using sudo for install..."
    sudo make install
else
    make install
fi

echo "[Build Script - m4] M4 build and installation complete."
exit 0