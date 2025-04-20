Okay, let's compare the list of artifact types you gathered with the current implementation in sapphire-core/src/build/cask/mod.rs.

Based on the code in install_cask, here's a breakdown:

Artifact Types with Handling Logic (Install):

app: Explicitly handled by calling app::install_app_from_staged.
pkg: Handled directly if the download is a .pkg, and also handled if found within a staged directory (like from a DMG) by calling pkg::install_pkg_from_path.
Artifact Types with Partial/Placeholder Handling (Install):

binary: There's a match arm for "binary", but it currently only logs a warning that it's not implemented.
Artifact Types Missing Explicit Handling Logic (Install):

Based on the loop processing artifacts_def in sapphire-core/src/build/cask/mod.rs, the following types from your list don't have specific installation code blocks:

suite
installer
manpage
colorpicker
dictionary
font
input_method
internet_plugin
keyboard_layout
prefpane
qlplugin
mdimporter
screen_saver
service (Although uninstall logic handles Launchd artifacts)
audio_unit_plugin
vst_plugin
vst3_plugin
So, while the container formats (.dmg, .zip) are handled for extraction, and app and pkg artifacts inside them are installed, the logic for installing these other specific artifact types still needs to be implemented in the install_cask function.


sapphire-core/
└── src/
    └── build/
        └── cask/
            ├── artifacts/
            │   ├── mod.rs
            │   ├── app.rs
            │   ├── suite.rs
            │   ├── installer.rs
            │   ├── pkg.rs
            │   ├── binary.rs
            │   ├── manpage.rs
            │   ├── colorpicker.rs
            │   ├── dictionary.rs
            │   ├── font.rs
            │   ├── input_method.rs
            │   ├── internet_plugin.rs
            │   ├── keyboard_layout.rs
            │   ├── prefpane.rs
            │   ├── qlplugin.rs
            │   ├── mdimporter.rs
            │   ├── screen_saver.rs
            │   ├── service.rs
            │   ├── audio_unit_plugin.rs
            │   ├── vst_plugin.rs
            │   └── vst3_plugin.rs
            ├── mod.rs
            ├── dmg.rs
            └── extract.rs
