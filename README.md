# Sapphire

> **WARNING: ALPHA SOFTWARE** > Sapphire is experimental, under heavy development, and may be unstable. Use at your own risk!
>
> Uninstalling a cask with brew then reinstalling it with Sapphire will have it installed with slightly different paths, your user settings etc. will not be migrated automatically.

Sapphire is a next‑generation, Rust‑powered package manager inspired by Homebrew. It installs and manages:

- **Formulae:** command‑line tools, libraries, and languages  
- **Casks:** desktop applications and related artifacts on macOS

> _ARM only for now, might add x86 support eventually_

---

## ⚙️ Project Structure

- **sapphire‑core** Core library: fetching, dependency resolution, archive extraction, artifact handling (apps, binaries, pkg installers, fonts, plugins, zap/preflight/uninstall stanzas, etc.)

- **sapphire‑cli** Command‑line interface: `sapphire` executable wrapping the core library.

---

## 🚧 Current Status

- Bottle installation and uninstallation  
- Cask installation and uninstallation
- Parallel downloads and installs for speed  
- Automatic dependency resolution and installation
- Building Formulae from source (very early impl)

---

## 🚀 Roadmap

1. **Upgrade** command to update installed packages  
2. **Cleanup** old downloads, versions, caches  
3. **Reinstall** command for quick re‑pours  
4. **Prefix isolation:** support `/opt/sapphire` as standalone layout  
5. **`sapphire init`** helper to bootstrap your environment
6. **Ongoing** Bug fixes and stability improvements

---

## 📦 Usage

```sh
# Print help
sapphire --help

# Update metadata
sapphire update

# Search for packages
sapphire search <formula/cask>

# Get package info
sapphire info <formula/cask>

# Install bottles or casks
sapphire install <formula/cask>

# Build and install a formula from source
sapphire install --build-from-source <formula>

# Uninstall
sapphire uninstall <app>

# (coming soon)
sapphire upgrade [--all] <name>
sapphire cleanup
sapphire init
````

-----

## 🏗️ Building from Source

**Prerequisites:** Rust toolchain (stable).

```sh
git clone <repo-url>
cd sapphire
cargo build --release
```

The `sapphire` binary will be at `target/release/sapphire`. Add it to your `PATH`.

-----

## 🤝 Contributing

Sapphire lives and grows by your feedback and code\! We’re particularly looking for:

  - Testing and bug reports for Cask & Bottle installation + `--build-from-source`
  - Test coverage for core and cask modules
  - CLI UI/UX improvements
  - See [CONTRIBUTING.md](https://www.google.com/search?q=CONTRIBUTING.md)

Feel free to open issues or PRs. Every contribution helps\!

-----

## 📄 License

  - **Sapphire:** BSD‑3‑Clause - see [LICENSE.md](LICENSE.md)
  - Inspired by Homebrew BSD‑2‑Clause — see [NOTICE.md](https://www.google.com/search?q=NOTICE.md)

-----

> *Alpha software. No guarantees. Use responsibly.*
