pub mod app;
pub mod suite;
pub mod installer;
pub mod pkg;
pub mod binary;
pub mod manpage;
pub mod colorpicker;
pub mod dictionary;
pub mod font;
pub mod input_method;
pub mod internet_plugin;
pub mod keyboard_layout;
pub mod prefpane;
pub mod qlplugin;
pub mod mdimporter;
pub mod screen_saver;
pub mod service;
pub mod audio_unit_plugin;
pub mod vst_plugin;
pub mod vst3_plugin;
pub mod zap;
pub mod preflight;
pub mod uninstall;

// Re‑export a single enum if you like:
pub use self::{
    app::install_app_from_staged,
    suite::install_suite,
    installer::run_installer,
    pkg::install_pkg_from_path,
    binary::install_binary,
    manpage::install_manpage,
    colorpicker::install_colorpicker,
    dictionary::install_dictionary,
    font::install_font,
    input_method::install_input_method,
    internet_plugin::install_internet_plugin,
    keyboard_layout::install_keyboard_layout,
    prefpane::install_prefpane,
    qlplugin::install_qlplugin,
    mdimporter::install_mdimporter,
    screen_saver::install_screen_saver,
    service::install_service,
    audio_unit_plugin::install_audio_unit_plugin,
    vst_plugin::install_vst_plugin,
    vst3_plugin::install_vst3_plugin,
    preflight::run_preflight,
    uninstall::record_uninstall
};
