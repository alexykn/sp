#!/bin/sh
# Fallback build script for GNU Autoconf v2.72

set -eu

# --- Arguments ---
BUILD_DIR="$1"
INSTALL_PREFIX="$2"
M4_PREFIX="$3" # Prerequisite M4 location

# Basic validation
if [ -z "$BUILD_DIR" ] || [ -z "$INSTALL_PREFIX" ] || [ -z "$M4_PREFIX" ]; then
  echo "Usage: $0 <build_dir> <install_prefix> <m4_prefix>" >&2
  exit 1
fi
if [ ! -d "$M4_PREFIX/bin" ]; then
    echo "Error: M4 prefix bin directory '$M4_PREFIX/bin' not found." >&2
    exit 1
fi
# ... (add checks for BUILD_DIR existence) ...

echo "[Build Script - autoconf] Starting build in $BUILD_DIR"
echo "[Build Script - autoconf] Installing to $INSTALL_PREFIX"
echo "[Build Script - autoconf] Using M4 from: $M4_PREFIX"

# --- Environment Setup ---
# Prepend M4 to PATH for this script's execution context
export PATH="$M4_PREFIX/bin:$PATH"
echo "[Build Script - autoconf] Using PATH: $PATH" # For debugging

# --- Build Steps ---
cd "$BUILD_DIR"

# Configure
echo "[Build Script - autoconf] Running configure..."
# Ensure configure finds the M4 in the modified PATH
# Pass EMACS=no to potentially avoid issues if emacs isn't found/needed
./configure --prefix="$INSTALL_PREFIX" EMACS=no

# Compile
echo "[Build Script - autoconf] Running make..."
make -j$(sysctl -n hw.ncpu || echo 1)

# Test
echo "[Build Script - autoconf] Running make check..."
if make -j$(sysctl -n hw.ncpu || echo 1) check; then
  echo "[Build Script - autoconf] 'make check' successful."
else
  echo "[Build Script - autoconf] Warning: 'make check' failed. Installation will continue." >&2
fi

# Install
echo "[Build Script - autoconf] Running make install..."
INSTALL_PARENT=$(dirname "$INSTALL_PREFIX")
if [ ! -w "$INSTALL_PARENT" ] && [ "$(id -u)" != "0" ]; then
    echo "[Build Script - autoconf] Install prefix parent '$INSTALL_PARENT' not writable. Using sudo for install..."
    sudo make install
else
    make install
fi

echo "[Build Script - autoconf] Autoconf build and installation complete."
exit 0