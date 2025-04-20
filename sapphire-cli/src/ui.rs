// sapphire-cli/src/ui.rs
//! UI utility functions for creating common elements like spinners.

use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

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
    pb.set_style(
        ProgressStyle::with_template("{spinner:.blue.bold} {msg}")
            .unwrap()
            // Optional: Add tick strings for a different spinner appearance
            // .tick_strings(&[
            //     "▹▹▹▹▹",
            //     "▸▹▹▹▹",
            //     "▹▸▹▹▹",
            //     "▹▹▸▹▹",
            //     "▹▹▹▸▹",
            //     "▹▹▹▹▸",
            //     "▪▪▪▪▪",
            // ])
    );
    pb.set_message(message.to_string());
    pb.enable_steady_tick(Duration::from_millis(100)); // Standard tick rate
    pb
}

// You can add more functions here for different styles or progress bars later.
// For example:
// pub fn create_progress_bar(total_items: u64) -> ProgressBar { ... }