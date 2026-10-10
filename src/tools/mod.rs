mod bash;
mod bash_bg;
mod doc_index;
mod doc_search;
mod fetch_page;
mod fetch_url;
pub mod file_edit;
mod file_read;
pub(crate) mod image_fit;
mod list_directory;
mod pdf_read;
mod powershell;
mod search;
mod shell_detect;
mod think;
mod thought_gate;
pub(crate) mod time_budget;
pub mod todo;
pub(crate) mod view_image;

pub use bash::BashTool;
pub use bash_bg::BashBgTool;
pub use doc_index::DocIndexTool;
pub use doc_search::DocSearchTool;
pub use fetch_page::FetchPageTool;
pub use fetch_url::FetchUrlTool;
pub use file_edit::{FileCreateTool, FileInsertTool, FileStrReplaceTool};
pub use file_read::FileReadTool;
pub use list_directory::ListDirectoryTool;
pub use pdf_read::PdfReadTool;
pub use powershell::PowerShellTool;
pub use search::SearchTool;
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
