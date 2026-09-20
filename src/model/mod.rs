use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Evidence {
    pub file: String,
    pub line: usize,
    pub end_line: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub pid: String,
    pub kind: String,
    pub operation: String,
    pub path: Option<String>,
    pub outcome: String,
    pub evidence: Evidence,
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub child_pid: Option<String>,
    /// Last observed/lexically described CWD context, never current physical identity.
    /// Older records may have used this context to derive absolute path spellings.
    #[serde(default)]
    pub cwd_before: Option<String>,
    /// Unresolved relative path or shared/changed filesystem context.
    /// False does not certify physical path identity (especially in older records).
    #[serde(default)]
    pub context_uncertain: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub schema_version: u32,
    pub tool_version: String,
    pub id: String,
    pub name: String,
    pub command: Vec<String>,
    pub cwd: String,
    pub started: u128,
    pub ended: Option<u128>,
    pub backend: String,
    pub state: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub warnings: Vec<String>,
    pub parsed_events: usize,
    pub raw_lines: usize,
    /// None means legacy format did not record an environment selection.
    #[serde(default)]
    pub environment: Option<std::collections::BTreeMap<String, Option<String>>>,
    #[serde(default)]
    pub completeness: Option<String>,
    #[serde(default)]
    pub collector_exit_code: Option<i32>,
    #[serde(default)]
    pub collector_signal: Option<i32>,
    #[serde(default)]
    pub target_exit_raw: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Run {
    pub metadata: Metadata,
    pub events: Vec<Event>,
    pub storage_integrity: StorageIntegrity,
}
// ASCII representation is reversible for all Unix bytes, including non-UTF-8.
pub fn bytes(v: &[u8]) -> String {
    let mut s = String::new();
    for &b in v {
        match b {
            b' '..=b'~' if b != b'\\' => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
    }
    s
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageIntegrity {
    pub status: String,
    pub detail: String,
}
