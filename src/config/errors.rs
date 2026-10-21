//! The one error shape of the 0.24 config boundary (spec §4.1), shared by the
//! CLI, reload and the Settings HTTP API.

/// Which file an error points into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFileKind {
    Global,
    Repo,
}

/// One segment of a YAML key path. A segment array, not a dotted string,
/// because profile/agent names may themselves contain `.` and spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSeg {
    Key(String),
    Index(usize),
}

/// A single config error: the file, the YAML path (`[]` = document root) and
/// a human sentence without a path prefix.
#[derive(Debug, Clone)]
pub struct ConfigError {
    pub file: ConfigFileKind,
    pub path: Vec<PathSeg>,
    pub message: String,
}

impl ConfigError {
    /// The path joined with `.` for human eyes (CLI/reload). Never parsed
    /// back — it is ambiguous when names contain dots.
    pub fn dotted_path(&self) -> String {
        dotted(&self.path)
    }
}

/// Every collected error of one `Config::parse` call: global file first,
/// then the repo file, each in document order.
#[derive(Debug)]
pub struct ConfigErrors(pub Vec<ConfigError>);

pub(crate) fn dotted(path: &[PathSeg]) -> String {
    path.iter()
        .map(|seg| match seg {
            PathSeg::Key(k) => k.clone(),
            PathSeg::Index(i) => i.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}
