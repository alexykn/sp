//! UI utility functions for creating common elements like spinners.

use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

/// Creates and configures a default spinner ProgressBar.
///
/// # Arguments
///
/// * `message` - The initial message to display next to the spinner.
///
/// # Returns
///
/// A configured `ProgressBar` instance ready to be used.
pub fn create_spinner(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template("{spinner:.blue.bold} {msg}").unwrap());
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(100)); // Standard tick rate
    pb
}
