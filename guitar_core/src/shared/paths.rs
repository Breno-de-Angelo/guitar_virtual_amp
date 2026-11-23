//! Where the app keeps its own data files (TONE3000 downloads and session).
//!
//! Desktop resolves this through `dirs::data_dir()`. Android has no such
//! directory from `dirs`, so the Android entry point calls
//! [`set_app_data_dir`] with the activity's internal data path at startup.

use std::path::PathBuf;
use std::sync::OnceLock;

const APP_SUBDIR: &str = "guitar_virtual_amp";

static DATA_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Overrides the base directory returned by [`app_data_dir`]. Only the first
/// call has an effect; later calls are ignored.
pub fn set_app_data_dir(path: PathBuf) {
    let _ = DATA_DIR_OVERRIDE.set(path);
}

/// The app's private data directory, or `None` if the platform has none and
/// no override was set.
pub fn app_data_dir() -> Option<PathBuf> {
    if let Some(path) = DATA_DIR_OVERRIDE.get() {
        return Some(path.join(APP_SUBDIR));
    }
    dirs::data_dir().map(|base| base.join(APP_SUBDIR))
}
