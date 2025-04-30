// sps-core/src/io_manager.rs
// Manages asynchronous IO operations requested by synchronous worker threads,
// incorporating concurrency limiting and cleaner request patterns.

use std::collections::HashMap;
use std::path::{Path, PathBuf}; // Import Path
use std::process::Output as StdOutput;
use std::sync::Arc;

use sps_aio; // Provides the async IO functions
use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use tokio::sync::{mpsc, oneshot, Semaphore}; // Added Semaphore
use tokio::task::JoinHandle; // Added JoinHandle
use tracing::{debug, error, instrument, warn, Instrument, Span};

// Limit the number of concurrent IO tasks spawned by the manager
const MAX_INFLIGHT_IO: usize = 256;

// --- Request Enum ---
// Added #[cfg] for macOS-specific operations
#[derive(Debug)]
pub enum IoRequest {
    // --- Filesystem Read Operations ---
    CheckPathExists {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<bool>>,
    },
    IsDirectory {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<bool>>,
    },
    IsFile {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<bool>>,
    },
    ReadToString {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<String>>,
    },
    ReadToJson {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<serde_json::Value>>,
    },
    ReadToBytes {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<Vec<u8>>>,
    },
    ListDirectory {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<Vec<(String, PathBuf, bool)>>>,
    },
    GetMetadata {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<std::fs::Metadata>>,
    },
    GetSymlinkMetadata {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<std::fs::Metadata>>,
    },

    // --- Filesystem Write/Modify Operations ---
    CreateDirAll {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<()>>,
    },
    RemovePath {
        path: PathBuf,
        use_sudo: bool,
        response_tx: oneshot::Sender<Result<()>>,
    },
    CopyFile {
        source: PathBuf,
        dest: PathBuf,
        response_tx: oneshot::Sender<Result<u64>>,
    },
    CopyRecursive {
        source: PathBuf,
        dest: PathBuf,
        response_tx: oneshot::Sender<Result<()>>,
    },
    MovePath {
        source: PathBuf,
        dest: PathBuf,
        response_tx: oneshot::Sender<Result<()>>,
    },
    #[cfg(unix)] // Symlinks are unix-specific in std::os::unix
    CreateSymlink {
        target: PathBuf,
        link: PathBuf,
        response_tx: oneshot::Sender<Result<()>>,
    },
    #[cfg(unix)] // Permissions modes are unix-specific
    SetPermissions {
        path: PathBuf,
        mode: u32,
        response_tx: oneshot::Sender<Result<()>>,
    },
    AtomicWriteFile {
        path: PathBuf,
        content: Vec<u8>,
        response_tx: oneshot::Sender<Result<()>>,
    },
    WriteJson {
        path: PathBuf,
        json_bytes: Vec<u8>,
        response_tx: oneshot::Sender<Result<()>>,
    },

    // --- Process/Command Execution ---
    RunCommand {
        command: String,
        args: Vec<String>,
        cwd: Option<PathBuf>,
        envs: Option<HashMap<String, String>>,
        response_tx: oneshot::Sender<Result<StdOutput>>,
    },
    // Specific Uninstall Commands (macOS specific)
    #[cfg(target_os = "macos")]
    ForgetPkgutil {
        id: String,
        response_tx: oneshot::Sender<Result<()>>,
    },
    #[cfg(target_os = "macos")]
    UnloadLaunchd {
        label: String,
        plist_path: Option<PathBuf>,
        response_tx: oneshot::Sender<Result<()>>,
    },
    #[cfg(target_os = "macos")]
    TrashPath {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<()>>,
    },

    // --- Git Operations ---
    UpdateGitRepo {
        path: PathBuf,
        response_tx: oneshot::Sender<Result<()>>,
    },

    // --- Validation ---
    VerifyChecksum {
        path: PathBuf,
        expected: String,
        response_tx: oneshot::Sender<Result<()>>,
    },

    // --- Archive Extraction ---
    ExtractArchive {
        archive_path: PathBuf,
        target_dir: PathBuf,
        strip_components: usize,
        response_tx: oneshot::Sender<Result<()>>,
    },
}

// --- Request Builder Methods ---
// These simplify the calling code in the synchronous workers.
impl IoRequest {
    // --- Read Builders ---
    pub fn build_read_to_string(
        path: PathBuf,
    ) -> (Self, oneshot::Receiver<Result<String>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::ReadToString { path, response_tx }, response_rx)
    }

    pub fn build_read_to_json(
        path: PathBuf,
    ) -> (Self, oneshot::Receiver<Result<serde_json::Value>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::ReadToJson { path, response_tx }, response_rx)
    }

     pub fn build_read_to_bytes(
        path: PathBuf,
    ) -> (Self, oneshot::Receiver<Result<Vec<u8>>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::ReadToBytes { path, response_tx }, response_rx)
    }

    pub fn build_check_path_exists(
        path: PathBuf,
    ) -> (Self, oneshot::Receiver<Result<bool>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::CheckPathExists { path, response_tx }, response_rx)
    }

     pub fn build_is_directory(
        path: PathBuf,
    ) -> (Self, oneshot::Receiver<Result<bool>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::IsDirectory { path, response_tx }, response_rx)
    }

     pub fn build_is_file(
        path: PathBuf,
    ) -> (Self, oneshot::Receiver<Result<bool>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::IsFile { path, response_tx }, response_rx)
    }

     pub fn build_list_directory(
        path: PathBuf,
    ) -> (Self, oneshot::Receiver<Result<Vec<(String, PathBuf, bool)>>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::ListDirectory { path, response_tx }, response_rx)
    }

     pub fn build_get_metadata(
        path: PathBuf,
    ) -> (Self, oneshot::Receiver<Result<std::fs::Metadata>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::GetMetadata { path, response_tx }, response_rx)
    }

     pub fn build_get_symlink_metadata(
        path: PathBuf,
    ) -> (Self, oneshot::Receiver<Result<std::fs::Metadata>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::GetSymlinkMetadata { path, response_tx }, response_rx)
    }


    // --- Write/Modify Builders ---
    pub fn build_create_dir_all(path: PathBuf) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::CreateDirAll { path, response_tx }, response_rx)
    }

    pub fn build_remove_path(path: PathBuf, use_sudo: bool) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::RemovePath { path, use_sudo, response_tx }, response_rx)
    }

    pub fn build_copy_file(source: PathBuf, dest: PathBuf) -> (Self, oneshot::Receiver<Result<u64>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::CopyFile { source, dest, response_tx }, response_rx)
    }

     pub fn build_copy_recursive(source: PathBuf, dest: PathBuf) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::CopyRecursive { source, dest, response_tx }, response_rx)
    }

     pub fn build_move_path(source: PathBuf, dest: PathBuf) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::MovePath { source, dest, response_tx }, response_rx)
    }

    #[cfg(unix)]
    pub fn build_create_symlink(target: PathBuf, link: PathBuf) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::CreateSymlink { target, link, response_tx }, response_rx)
    }

    #[cfg(unix)]
    pub fn build_set_permissions(path: PathBuf, mode: u32) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::SetPermissions { path, mode, response_tx }, response_rx)
    }

    pub fn build_atomic_write_file(path: PathBuf, content: Vec<u8>) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::AtomicWriteFile { path, content, response_tx }, response_rx)
    }

    pub fn build_write_json(path: PathBuf, json_bytes: Vec<u8>) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::WriteJson { path, json_bytes, response_tx }, response_rx)
    }


    // --- Process Builders ---
     pub fn build_run_command(
        command: String,
        args: Vec<String>,
        cwd: Option<PathBuf>,
        envs: Option<HashMap<String, String>>,
    ) -> (Self, oneshot::Receiver<Result<StdOutput>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::RunCommand { command, args, cwd, envs, response_tx }, response_rx)
    }

    #[cfg(target_os = "macos")]
    pub fn build_forget_pkgutil(id: String) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::ForgetPkgutil { id, response_tx }, response_rx)
    }

    #[cfg(target_os = "macos")]
    pub fn build_unload_launchd(label: String, plist_path: Option<PathBuf>) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::UnloadLaunchd { label, plist_path, response_tx }, response_rx)
    }

    #[cfg(target_os = "macos")]
    pub fn build_trash_path(path: PathBuf) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::TrashPath { path, response_tx }, response_rx)
    }


    // --- Git Builder ---
    pub fn build_update_git_repo(path: PathBuf) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::UpdateGitRepo { path, response_tx }, response_rx)
    }

    // --- Validation Builder ---
    pub fn build_verify_checksum(path: PathBuf, expected: String) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::VerifyChecksum { path, expected, response_tx }, response_rx)
    }

    // --- Archive Builder ---
    pub fn build_extract_archive(
        archive_path: PathBuf,
        target_dir: PathBuf,
        strip_components: usize,
    ) -> (Self, oneshot::Receiver<Result<()>>) {
        let (response_tx, response_rx) = oneshot::channel();
        (Self::ExtractArchive { archive_path, target_dir, strip_components, response_tx }, response_rx)
    }

}


// --- IO Manager Runner ---
pub async fn run_io_manager(
    mut request_rx: mpsc::Receiver<IoRequest>,
    config: Arc<Config>,
    semaphore: Arc<Semaphore>, // Use Arc for Semaphore
) {
    debug!("Async IO Manager started with concurrency limit: {}", MAX_INFLIGHT_IO);
    while let Some(request) = request_rx.recv().await {
        let span = tracing::info_span!("io_manager_request", op = %request_variant_name(&request));
        let config_clone = Arc::clone(&config);
        let semaphore_clone = Arc::clone(&semaphore); // Clone Arc for the task

        // Spawn a task for each request, but acquire a semaphore permit first
        tokio::spawn(
            async move {
                // Acquire permit. This will block if the limit is reached.
                let permit = match semaphore_clone.acquire_owned().await {
                     Ok(p) => p,
                     Err(_) => {
                          error!("IO Manager semaphore closed unexpectedly.");
                          // Find the response_tx and send an error back
                          send_error_response(request, SpsError::Generic("IO Manager semaphore closed".into()));
                          return;
                     }
                };

                // --- Request Handling Logic (moved inside spawned task) ---
                match request {
                    // --- Filesystem Read ---
                    IoRequest::CheckPathExists { path, response_tx } => {
                        let result = sps_aio::fs::check_path_exists_async(&path).await;
                        handle_response(response_tx, result, "CheckPathExists");
                    }
                    IoRequest::IsDirectory { path, response_tx } => {
                        let result = sps_aio::fs::is_directory_async(&path).await;
                        handle_response(response_tx, result, "IsDirectory");
                    }
                    IoRequest::IsFile { path, response_tx } => {
                        let result = sps_aio::fs::is_file_async(&path).await;
                        handle_response(response_tx, result, "IsFile");
                    }
                    IoRequest::ReadToString { path, response_tx } => {
                        let result = sps_aio::fs::read_to_string_async(&path).await;
                        handle_response(response_tx, result, "ReadToString");
                    }
                    IoRequest::ReadToJson { path, response_tx } => {
                        let result = sps_aio::json_io::read_json_async(&path).await;
                        handle_response(response_tx, result, "ReadToJson");
                    }
                    IoRequest::ReadToBytes { path, response_tx } => {
                        let result = sps_aio::fs::read_to_bytes_async(&path).await;
                        handle_response(response_tx, result, "ReadToBytes");
                    }
                    IoRequest::ListDirectory { path, response_tx } => {
                        let result = sps_aio::fs::list_directory_entries_async(&path).await;
                        handle_response(response_tx, result, "ListDirectory");
                    }
                    IoRequest::GetMetadata { path, response_tx } => {
                        let result = sps_aio::fs::get_metadata_async(&path).await;
                        handle_response(response_tx, result, "GetMetadata");
                    }
                    IoRequest::GetSymlinkMetadata { path, response_tx } => {
                        let result = sps_aio::fs::get_symlink_metadata_async(&path).await;
                        handle_response(response_tx, result, "GetSymlinkMetadata");
                    }

                    // --- Filesystem Write/Modify ---
                    IoRequest::CreateDirAll { path, response_tx } => {
                        let result = sps_aio::fs::create_dir_all_async(&path).await;
                        handle_response(response_tx, result, "CreateDirAll");
                    }
                    IoRequest::RemovePath { path, use_sudo, response_tx } => {
                        let result = sps_aio::uninstall::remove_path_async(&path, use_sudo).await;
                        handle_response(response_tx, result, "RemovePath");
                    }
                    IoRequest::CopyFile { source, dest, response_tx } => {
                        let result = sps_aio::fs::copy_file_async(&source, &dest).await;
                        handle_response(response_tx, result, "CopyFile");
                    }
                    IoRequest::CopyRecursive { source, dest, response_tx } => {
                        let result = sps_aio::fs::copy_recursive_async(&source, &dest).await;
                        handle_response(response_tx, result, "CopyRecursive");
                    }
                    IoRequest::MovePath { source, dest, response_tx } => {
                        let result = sps_aio::fs::move_path_async(&source, &dest).await;
                        handle_response(response_tx, result, "MovePath");
                    }
                    #[cfg(unix)]
                    IoRequest::CreateSymlink { target, link, response_tx } => {
                        let result = sps_aio::fs::create_symlink_async(&target, &link).await;
                        handle_response(response_tx, result, "CreateSymlink");
                    }
                    #[cfg(unix)]
                    IoRequest::SetPermissions { path, mode, response_tx } => {
                        let result = sps_aio::fs::set_permissions_async(&path, mode).await;
                        handle_response(response_tx, result, "SetPermissions");
                    }
                    IoRequest::AtomicWriteFile { path, content, response_tx } => {
                        let result = sps_aio::fs::atomic_write_file_async(&path, &content).await;
                        handle_response(response_tx, result, "AtomicWriteFile");
                    }
                    IoRequest::WriteJson { path, json_bytes, response_tx } => {
                        let result = sps_aio::fs::atomic_write_file_async(&path, &json_bytes).await;
                         handle_response(response_tx, result, "WriteJson");
                    }

                    // --- Process/Command ---
                    IoRequest::RunCommand { command, args, cwd, envs, response_tx } => {
                        let result = sps_aio::process::run_command_async(command, args, cwd, envs).await;
                        handle_response(response_tx, result, "RunCommand");
                    }
                    #[cfg(target_os = "macos")]
                    IoRequest::ForgetPkgutil { id, response_tx } => {
                        let result = sps_aio::uninstall::forget_pkgutil_async(&id).await;
                        handle_response(response_tx, result, "ForgetPkgutil");
                    }
                    #[cfg(target_os = "macos")]
                    IoRequest::UnloadLaunchd { label, plist_path, response_tx } => {
                        let result = sps_aio::uninstall::unload_launchd_async(&label, plist_path.as_deref(), &config_clone).await;
                        handle_response(response_tx, result, "UnloadLaunchd");
                    }
                    #[cfg(target_os = "macos")]
                    IoRequest::TrashPath { path, response_tx } => {
                        let result = sps_aio::uninstall::trash_path_async(&path).await;
                        handle_response(response_tx, result, "TrashPath");
                    }

                    // --- Git ---
                    IoRequest::UpdateGitRepo { path, response_tx } => {
                        let result = sps_aio::git2::update_repo_async(&path).await;
                        handle_response(response_tx, result, "UpdateGitRepo");
                    }

                    // --- Validation ---
                    IoRequest::VerifyChecksum { path, expected, response_tx } => {
                        let result = sps_aio::checksum::verify_checksum_async(&path, &expected).await;
                        handle_response(response_tx, result, "VerifyChecksum");
                    }

                    // --- Archive ---
                    IoRequest::ExtractArchive { archive_path, target_dir, strip_components, response_tx } => {
                        let result = sps_aio::extract::extract_archive_async(
                            &archive_path,
                            &target_dir,
                            strip_components,
                        ).await;
                        handle_response(response_tx, result, "ExtractArchive");
                    }

                    // --- Catch-all for non-unix specific variants on non-unix ---
                    // This handles cases like trying to build IoRequest::CreateSymlink on Windows
                    #[cfg(not(unix))]
                    IoRequest::CreateSymlink { response_tx, .. } => {
                         handle_response(response_tx, Err(SpsError::Generic("Symlinks not supported".into())), "CreateSymlink");
                    }
                     #[cfg(not(unix))]
                    IoRequest::SetPermissions { response_tx, .. } => {
                         handle_response(response_tx, Err(SpsError::Generic("Permissions not supported".into())), "SetPermissions");
                    }
                    // Catch-all for non-macOS specific variants on non-macOS
                    #[cfg(not(target_os = "macos"))]
                    IoRequest::ForgetPkgutil { response_tx, .. } => {
                         handle_response(response_tx, Err(SpsError::Generic("pkgutil not supported".into())), "ForgetPkgutil");
                    }
                    #[cfg(not(target_os = "macos"))]
                    IoRequest::UnloadLaunchd { response_tx, .. } => {
                         handle_response(response_tx, Err(SpsError::Generic("launchd not supported".into())), "UnloadLaunchd");
                    }
                    #[cfg(not(target_os = "macos"))]
                    IoRequest::TrashPath { response_tx, .. } => {
                         handle_response(response_tx, Err(SpsError::Generic("Trash not supported".into())), "TrashPath");
                    }

                }
                // Permit is dropped here when the task finishes
                drop(permit);
            }
            .instrument(span), // Apply the span to the spawned task
        );
    }
    debug!("Async IO Manager stopped.");
}

// --- Helper for sending response ---
fn handle_response<T>(tx: oneshot::Sender<Result<T>>, result: Result<T>, operation_name: &str) {
     match tx.send(result) {
          Ok(_) => {}, // Successfully sent
          Err(failed_result) => {
               // The receiving end was dropped. Log the outcome of the operation.
               match failed_result {
                    Ok(_) => warn!("IO Manager: Worker disconnected before receiving successful response for {}", operation_name),
                    Err(e) => warn!(
                         "IO Manager: Worker disconnected before receiving error response for {}: {}",
                         operation_name, e
                    ),
               }
          }
     }
}

// --- Helper to send error when semaphore fails ---
// This needs the original request to extract the response_tx channel.
fn send_error_response(request: IoRequest, error: SpsError) {
    warn!("Sending error back to worker due to semaphore issue: {}", error);
    // We need to match again just to get the tx channel. This is slightly awkward
    // but necessary because the request was moved into the spawn closure attempt.
    let send_result = match request {
         IoRequest::CheckPathExists { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::IsDirectory { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::IsFile { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::ReadToString { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::ReadToJson { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::ReadToBytes { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::ListDirectory { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::GetMetadata { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::GetSymlinkMetadata { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::CreateDirAll { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::RemovePath { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::CopyFile { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::CopyRecursive { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::MovePath { response_tx, .. } => response_tx.send(Err(error)),
         #[cfg(unix)]
         IoRequest::CreateSymlink { response_tx, .. } => response_tx.send(Err(error)),
         #[cfg(unix)]
         IoRequest::SetPermissions { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::AtomicWriteFile { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::WriteJson { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::RunCommand { response_tx, .. } => response_tx.send(Err(error)),
         #[cfg(target_os = "macos")]
         IoRequest::ForgetPkgutil { response_tx, .. } => response_tx.send(Err(error)),
         #[cfg(target_os = "macos")]
         IoRequest::UnloadLaunchd { response_tx, .. } => response_tx.send(Err(error)),
         #[cfg(target_os = "macos")]
         IoRequest::TrashPath { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::UpdateGitRepo { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::VerifyChecksum { response_tx, .. } => response_tx.send(Err(error)),
         IoRequest::ExtractArchive { response_tx, .. } => response_tx.send(Err(error)),
         // Catch-all for variants disabled by cfg
         #[cfg(not(unix))]
         IoRequest::CreateSymlink { response_tx, .. } => response_tx.send(Err(error)),
         #[cfg(not(unix))]
         IoRequest::SetPermissions { response_tx, .. } => response_tx.send(Err(error)),
         #[cfg(not(target_os="macos"))]
         IoRequest::ForgetPkgutil { response_tx, .. } => response_tx.send(Err(error)),
         #[cfg(not(target_os="macos"))]
         IoRequest::UnloadLaunchd { response_tx, .. } => response_tx.send(Err(error)),
         #[cfg(not(target_os="macos"))]
         IoRequest::TrashPath { response_tx, .. } => response_tx.send(Err(error)),

    };
    if send_result.is_err() {
         // Log if we couldn't even send the error back
         warn!("Failed to send semaphore error back to worker (already disconnected?).");
    }
}


// --- Setup Function ---
// Returns the sender and the JoinHandle for the manager task.
pub fn start_io_manager(
    runtime: &tokio::runtime::Handle,
    config: Arc<Config>,
) -> (mpsc::Sender<IoRequest>, JoinHandle<()>) { // Return JoinHandle
    const CHANNEL_BUFFER_SIZE: usize = 128;
    let (request_tx, request_rx) = mpsc::channel(CHANNEL_BUFFER_SIZE);

    // Create the semaphore, wrapped in Arc for sharing
    let semaphore = Arc::new(Semaphore::new(MAX_INFLIGHT_IO));

    // Spawn the manager task and capture its handle
    let manager_handle = runtime.spawn(
        run_io_manager(request_rx, config, semaphore) // Pass semaphore
            .instrument(tracing::info_span!("async_io_manager_task")),
    );

    debug!("IO Manager task spawned.");
    (request_tx, manager_handle) // Return both sender and handle
}


// --- Helper function to get variant name (no changes needed) ---
fn request_variant_name(request: &IoRequest) -> &'static str {
    match request {
        IoRequest::CheckPathExists { .. } => "CheckPathExists",
        IoRequest::IsDirectory { .. } => "IsDirectory",
        IoRequest::IsFile { .. } => "IsFile",
        IoRequest::ReadToString { .. } => "ReadToString",
        IoRequest::ReadToJson { .. } => "ReadToJson",
        IoRequest::ReadToBytes { .. } => "ReadToBytes",
        IoRequest::ListDirectory { .. } => "ListDirectory",
        IoRequest::GetMetadata { .. } => "GetMetadata",
        IoRequest::GetSymlinkMetadata { .. } => "GetSymlinkMetadata",
        IoRequest::CreateDirAll { .. } => "CreateDirAll",
        IoRequest::RemovePath { .. } => "RemovePath",
        IoRequest::CopyFile { .. } => "CopyFile",
        IoRequest::CopyRecursive { .. } => "CopyRecursive",
        IoRequest::MovePath { .. } => "MovePath",
        #[cfg(unix)]
        IoRequest::CreateSymlink { .. } => "CreateSymlink",
        #[cfg(unix)]
        IoRequest::SetPermissions { .. } => "SetPermissions",
        IoRequest::AtomicWriteFile { .. } => "AtomicWriteFile",
        IoRequest::WriteJson { .. } => "WriteJson",
        IoRequest::RunCommand { .. } => "RunCommand",
        #[cfg(target_os = "macos")]
        IoRequest::ForgetPkgutil { .. } => "ForgetPkgutil",
        #[cfg(target_os = "macos")]
        IoRequest::UnloadLaunchd { .. } => "UnloadLaunchd",
        #[cfg(target_os = "macos")]
        IoRequest::TrashPath { .. } => "TrashPath",
        IoRequest::UpdateGitRepo { .. } => "UpdateGitRepo",
        IoRequest::VerifyChecksum { .. } => "VerifyChecksum",
        IoRequest::ExtractArchive { .. } => "ExtractArchive",
        // Add catch-alls for cfg-disabled variants
        #[cfg(not(unix))]
        IoRequest::CreateSymlink { .. } => "CreateSymlink(Unsupported)",
        #[cfg(not(unix))]
        IoRequest::SetPermissions { .. } => "SetPermissions(Unsupported)",
        #[cfg(not(target_os = "macos"))]
        IoRequest::ForgetPkgutil { .. } => "ForgetPkgutil(Unsupported)",
        #[cfg(not(target_os = "macos"))]
        IoRequest::UnloadLaunchd { .. } => "UnloadLaunchd(Unsupported)",
        #[cfg(not(target_os = "macos"))]
        IoRequest::TrashPath { .. } => "TrashPath(Unsupported)",
    }
}

// --- Usage Example in Sync Worker (Using Request Builders) ---
/*
fn sync_worker_function(io_sender: &mpsc::Sender<IoRequest>) -> Result<()> {
    // ... some sync logic ...

    // Need to read a file
    let (request, response_rx) = IoRequest::build_read_to_string(PathBuf::from("config.txt"));
    io_sender.blocking_send(request).map_err(|e| SpsError::Generic(format!("IO send error: {e}")))?;
    let result = response_rx.blocking_recv().map_err(|e| SpsError::Generic(format!("IO recv error: {e}")))?;
    let content = result?; // Handle Result<String>
    println!("Config content: {}", content);

    // ... more sync logic ...

    // Need to remove a directory
    let (request_rm, response_rx_rm) = IoRequest::build_remove_path(PathBuf::from("/tmp/some_dir"), false);
     io_sender.blocking_send(request_rm).map_err(|e| SpsError::Generic(format!("IO send error: {e}")))?;
    let result_rm = response_rx_rm.blocking_recv().map_err(|e| SpsError::Generic(format!("IO recv error: {e}")))?;
    result_rm?; // Handle Result<()>

    Ok(())
}
*/