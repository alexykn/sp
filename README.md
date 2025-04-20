# Sapphire

> **WARNING: ALPHA SOFTWARE**  
> Sapphire is experimental, under heavy development, and may be unstable. Use at your own risk!
>
> Uninstalling a cask with brew then reinstalling it with Sapphire will have it installed with slightly different paths, your user settings etc. will not be migrated automatically.

Sapphire is a next‑generation, Rust‑powered package manager inspired by Homebrew. It installs and manages:

- **Formulae:** command‑line tools, libraries, and languages  
- **Casks:** desktop applications and related artifacts on macOS

> _ARM only for now, might add x86 support eventually_

---

## ⚙️ Project Structure

- **sapphire‑core**  
  Core library: fetching, dependency resolution, archive extraction, artifact handling (apps, binaries, pkg installers, fonts, plugins, zap/preflight/uninstall stanzas, etc.)

- **sapphire‑cli**  
  Command‑line interface: `sapphire` executable wrapping the core library.

---

## 🚧 Current Status

### Formulae

- Bottle installation and uninstallation  
- Parallel downloads and installs for speed  
- Dependencies, recommended/optional, tests support  
- _Temporary:_ source‑build (`--build-from-source`) is paused pending flags‑rework

### Casks

- **Info**, **search**, **install**, **uninstall** all implemented  
- (untested for the most part) Supports _all_ Homebrew artifact stanzas, including:
  - **app**, **suite**, **installer**, **pkg**, **zip/tar**, **binary**, **manpage**, **font**, **colorpicker**, **dictionary**, **input_method**, **internet_plugin**, **keyboard_layout**, **prefpane**, **qlplugin**, **mdimporter**, **screen_saver**, **service**, **audio_unit_plugin**, **vst_plugin**, **vst3_plugin**  
  - **preflight** (run commands before moving files)  
  - **uninstall** (record and replay uninstall steps)  
  - **zap** (deep‑clean user data, logs, caches, receipts, launch agents)  
- Automatic wrapper‑script generation for “binary only” casks (e.g. Firefox)

---

## 🚀 Roadmap

1. **Finish source‑build support** (restore `--build-from-source`)  
2. **Upgrade** command to update installed packages  
3. **Cleanup** old downloads, versions, caches  
4. **Reinstall** command for quick re‑pours  
5. **Prefix isolation:** support `/opt/sapphire` as standalone layout  
6. **`sapphire init`** helper to bootstrap your environment  

---

## 📦 Usage

```sh
# Update metadata
sapphire update

# Search for packages
sapphire search <app>

# Get package info
sapphire info <app>

# Install bottles or casks
sapphire install <app>

# Uninstall
sapphire uninstall <app>

# (coming soon)
sapphire install --build-from-source <formula>
sapphire upgrade [--all] <name>…
sapphire cleanup
sapphire init
```

---

## 🏗️ Building from Source

**Prerequisites:**  
Rust toolchain (stable), C compiler, CMake, Ninja, pkg‑config.

```sh
git clone <repo-url>
cd sapphire
cargo build --release
```

The `sapphire` binary will be at `target/release/sapphire`. Add it to your `PATH`.

---

## 🤝 Contributing

Sapphire lives and grows by your feedback and code! We’re particularly looking for:

- More real‑world cask testing  
- Bug reports and reproducible cases  
- Test coverage for core and cask modules  
- CLI usability improvements

Feel free to open issues or PRs. Every contribution helps!

---

## 📄 License

- **Sapphire:** BSD‑3‑Clause  
- Inspired by Homebrew (BSD‑2‑Clause) — see [licenses/LICENSE‑Homebrew.md](licenses/LICENSE‑Homebrew.md)

---

> _Alpha software. No guarantees. Use responsibly._
