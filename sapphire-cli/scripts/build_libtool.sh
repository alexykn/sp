#!/bin/sh
# Fallback build script for GNU Libtool v2.5.4

set -eu

# --- Arguments ---
BUILD_DIR="$1"
INSTALL_PREFIX="$2"
M4_PREFIX="$3"
AUTOCONF_PREFIX="$4"

# Basic validation
if [ -z "$BUILD_DIR" ] || [ -z "$INSTALL_PREFIX" ] || [ -z "$M4_PREFIX" ] || [ -z "$AUTOCONF_PREFIX" ]; then
  echo "Usage: $0 <build_dir> <install_prefix> <m4_prefix> <autoconf_prefix>" >&2
  exit 1
fi
# ... (add checks for M4/Autoconf bin dir existence) ...

echo "[Build Script - libtool] Starting build in $BUILD_DIR"
echo "[Build Script - libtool] Installing to $INSTALL_PREFIX"
echo "[Build Script - libtool] Using M4 from: $M4_PREFIX"
echo "[Build Script - libtool] Using Autoconf from: $AUTOCONF_PREFIX"

# --- Environment Setup ---
# Prepend M4 and Autoconf to PATH
export PATH="$AUTOCONF_PREFIX/bin:$M4_PREFIX/bin:$PATH"
echo "[Build Script - libtool] Using PATH: $PATH"

# Add SED workaround for macOS if needed (check OS type)
# Using 'uname' is more portable than relying on Rust's cfg! inside the script
if [ "$(uname)" = "Darwin" ]; then
    echo "[Build Script - libtool] Applying SED=sed workaround for macOS"
    export SED="sed"
fi

# --- Build Steps ---
cd "$BUILD_DIR"

# Configure
echo "[Build Script - libtool] Running configure..."
# Use flags from user report
./configure --prefix="$INSTALL_PREFIX" \
            --disable-dependency-tracking \
            --disable-silent-rules \
            --enable-ltdl-install

# Compile
echo "[Build Script - libtool] Running make..."
make -j$(sysctl -n hw.ncpu || echo 1)

# Test
echo "[Build Script - libtool] Running make check..."
if make -j$(sysctl -n hw.ncpu || echo 1) check; then
  echo "[Build Script - libtool] 'make check' successful."
else
  echo "[Build Script - libtool] Warning: 'make check' failed. Installation will continue." >&2
fi

# Install
echo "[Build Script - libtool] Running make install..."
INSTALL_PARENT=$(dirname "$INSTALL_PREFIX")
if [ ! -w "$INSTALL_PARENT" ] && [ "$(id -u)" != "0" ]; then
    echo "[Build Script - libtool] Install prefix parent '$INSTALL_PARENT' not writable. Using sudo for install..."
    sudo make install
else
    make install
fi

echo "[Build Script - libtool] Libtool build and installation complete."
exit 0