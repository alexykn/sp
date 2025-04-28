# sps

> [!WARNING]
> **ALPHA SOFTWARE**
> sps his experimental, under heavy development, and may be unstable. Use at your own risk!
>
> Uninstalling a cask with brew then reinstalling it with sps will have it installed with slightly different paths, your user settings etc. will not be migrated automatically.

sps his a next‑generation, Rust‑powered package manager inspired by Homebrew. It installs and manages:

- **Formulae:** command‑line tools, libraries, and languages  
- **Casks:** desktop applications and related artifacts on macOS

> _ARM only for now, might add x86 support eventually_

---

## ⚙️ Project Structure

- **sps‑core** Core library: fetching, dependency resolution, archive extraction, artifact handling (apps, binaries, pkg installers, fonts, plugins, zap/preflight/uninstall stanzas, etc.)

- **sps‑cli** Command‑line interface: `sps` executable wrapping the core library.

---

## 🚧 Current Status

- Bottle installation and uninstallation  
- Cask installation and uninstallation
- Reinstall command for reinstalls
- Upgrade command for updates (very careful. I ran into no system breakers, my Perl install got nuked though)
- Parallel downloads and installs for speed  
- Automatic dependency resolution and installation
- Building Formulae from source (very early impl)

---

## 🚀 Roadmap

- **Cleanup** old downloads, versions, caches  
- **Prefix isolation:** support `/opt/sps` as standalone layout  
- **`sps init`** helper to bootstrap your environment
- **Ongoing** Bug fixes and stability improvements

---

## Trying it out:

```bash
cargo install sps
```

---

<img width="856" alt="Screenshot 2025-04-26 at 22 09 41" src="https://github.com/user-attachments/assets/bd4a39ed-d4b3-4d19-9b1c-2edcba5f472d" />

> I know this does not follow one defined style yet. Still thinking about how I actually want it to look so... we'll get there

---

## 📦 Usage

```sh
# Print help
sps --help

# Update metadata
sps update

# Search for packages
sps search <formula/cask>

# Get package info
sps info <formula/cask>

# Install bottles or casks
sps install <formula/cask>

# Build and install a formula from source
sps install --build-from-source <formula>

# Uninstall
sps uninstall <formula/cask>

# Reinstall
sps reinstall <formula/cask>

#Upgrade
sps upgrade <formula/cask> or --all

# (coming soon)
sps cleanup
sps init
```

-----

## 🏗️ Building from Source

**Prerequisites:** Rust toolchain (stable).

```sh
git clone <repo-url>
cd sps
cargo build --release
```

The `sps` binary will be at `target/release/sps`. Add it to your `PATH`.


-----

## 📥 Using the Latest Nightly Build

You can download the latest nightly build from [`actions/workflows/rust.yml`](../../actions/workflows/rust.yml) inside this repository (select a successful build and scroll down to `Artifacts`).

Before running the downloaded binary, remove the quarantine attribute:

```sh
xattr -d com.apple.quarantine ./sps
```

Then, you can run the binary directly:

```sh
./sps --help
```


-----

## 🤝 Contributing

sps lives and grows by your feedback and code\! We’re particularly looking for:

  - Testing and bug reports for Cask & Bottle installation + `--build-from-source`
  - Test coverage for core and cask modules
  - CLI UI/UX improvements
  - See [CONTRIBUTING.md](CONTRIBUTING.md)

Feel free to open issues or PRs. Every contribution helps\!

-----

## 📄 License

  - **sps:** BSD‑3‑Clause - see [LICENSE.md](LICENSE.md)
  - Inspired by Homebrew BSD‑2‑Clause — see [NOTICE.md](NOTICE.md)

-----

> *Alpha software. No guarantees. Use responsibly.*
