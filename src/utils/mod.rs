//! General-purpose utilities.

pub mod path;
pub mod strings;
pub mod time;
pub use path::{normalize_lexical, resolve_cd_path};
pub use strings::{truncate_to_char_boundary, truncate_with_suffix};
pub use time::fmt_elapsed;
