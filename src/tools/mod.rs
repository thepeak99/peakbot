#[cfg(feature = "host")]
mod bash;
#[cfg(feature = "host")]
mod doc_index;
#[cfg(feature = "host")]
mod doc_search;
pub mod env;
mod fetch_page;
pub mod fetch_url;
#[cfg(feature = "host")]
pub mod file_edit;
#[cfg(feature = "host")]
mod file_read;
pub(crate) mod image_fit;
#[cfg(feature = "host")]
mod list_directory;
#[cfg(feature = "host")]
mod pdf_read;
#[cfg(feature = "host")]
mod powershell;
mod search;
#[cfg(feature = "host")]
mod shell_bg;
mod shell_detect;
mod think;
mod thought_gate;
pub(crate) mod time_budget;
pub mod todo;
// Compiled in both editions (the UI keys on `view_image::NAME`); the Local
// edition never registers the tool.
pub(crate) mod view_image;

#[cfg(feature = "host")]
pub use bash::BashTool;
#[cfg(feature = "host")]
pub use doc_index::DocIndexTool;
#[cfg(feature = "host")]
pub use doc_search::DocSearchTool;
pub use fetch_page::FetchPageTool;
pub use fetch_url::FetchUrlTool;
#[cfg(feature = "host")]
pub use file_edit::{FileCreateTool, FileInsertTool, FileStrReplaceTool};
#[cfg(feature = "host")]
pub use file_read::FileReadTool;
#[cfg(feature = "host")]
pub use list_directory::ListDirectoryTool;
#[cfg(feature = "host")]
pub use pdf_read::PdfReadTool;
#[cfg(feature = "host")]
pub use powershell::PowerShellTool;
pub use search::SearchTool;
#[cfg(feature = "host")]
pub use shell_bg::ShellBgTool;
pub use shell_detect::{ShellKind, print_no_shell_warning};
pub use think::ThinkTool;
pub use thought_gate::ThoughtGate;
pub use time_budget::TimeBudget;
pub use todo::{TodoArgs, TodoItem, TodoStatus, TodoTool};
pub use view_image::ViewImageTool;

/// The directory both shell tools save full output to. One source of
/// truth — `temp_dir()/peakbot` is `%TEMP%\peakbot` on Windows, and the
/// model-facing descriptions must not hard-code the Unix path.
pub fn shell_output_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("peakbot")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `shell_output_dir` is the OS temp dir plus `peakbot` — the single
    /// source of truth both shell tools must use (replacing the two
    /// private `TEMP_DIR_NAME` consts).
    #[test]
    fn shell_output_dir_is_temp_dir_peakbot() {
        assert_eq!(
            shell_output_dir(),
            std::env::temp_dir().join("peakbot"),
            "shell_output_dir must be temp_dir/peakbot"
        );
    }
}
