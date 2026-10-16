use std::fs;
use std::io;
use std::path::Path;

/// Keep read errors available: some checks require a log, others permit none.
pub fn read(path: &Path) -> io::Result<Vec<String>> {
    fs::read_to_string(path).map(|text| text.lines().map(str::to_owned).collect())
}
