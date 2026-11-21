use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MAX_TIMEOUT_MS: u64 = 6 * 60 * 60 * 1000;
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_ARGS: usize = 1024;
const MAX_ENV_VARS: usize = 256;
const MAX_STRING_BYTES: usize = 64 * 1024;

pub const INHERITED_ENV_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "TZ",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SYSTEMROOT",
    "USERPROFILE",
    "XDG_CACHE_HOME",
    "JAVA_HOME",
    "MAVEN_HOME",
    "M2_HOME",
    "MAVEN_OPTS",
    "GRADLE_USER_HOME",
    "GOROOT",
    "GOPATH",
    "GOCACHE",
    "GOMODCACHE",
    "GOFLAGS",
    "GOPROXY",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "NPM_CONFIG_CACHE",
    "npm_config_cache",
    "COREPACK_HOME",
    "PYTHONPATH",
    "VIRTUAL_ENV",
    "UV_CACHE_DIR",
    "UV_PYTHON_INSTALL_DIR",
    "UV_PYTHON_BIN_DIR",
    "PIP_CACHE_DIR",
    "CI",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    NotFound,
    SpawnFailed,
    Rejected,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecFailure {
    pub kind: FailureKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    pub output_tail: String,
    pub timed_out: bool,
    pub duration_ms: u64,
    #[serde(default)]
    pub failure: Option<ExecFailure>,
}

impl ExecResult {
    pub fn failed(kind: FailureKind, message: impl Into<String>) -> Self {
        Self { failure: Some(ExecFailure { kind, message: message.into() }), ..Self::default() }
    }

    pub fn success(&self) -> bool {
        self.failure.is_none() && !self.timed_out && self.exit_code == Some(0)
    }
}

pub fn validate(request: &ExecRequest) -> Result<(), String> {
    let Some(program) = request.argv.first() else {
        return Err("empty command".into());
    };
    if program.is_empty() {
        return Err("empty command".into());
    }
    if request.argv.len() > MAX_ARGS {
        return Err(format!("too many arguments ({} > {MAX_ARGS})", request.argv.len()));
    }
    if request.argv.iter().any(|arg| !is_clean(arg)) {
        return Err("arguments must not contain NUL bytes or exceed 64 KiB".into());
    }
    if request.env.len() > MAX_ENV_VARS {
        return Err(format!("too many environment variables ({} > {MAX_ENV_VARS})", request.env.len()));
    }
    for (name, value) in &request.env {
        if name.is_empty() || name.contains('=') || !is_clean(name) || !is_clean(value) {
            return Err(format!("invalid environment variable {name:?}"));
        }
    }
    if !request.cwd.is_absolute() {
        return Err(format!("working directory {} is not absolute", request.cwd.display()));
    }
    if request.timeout_ms == 0 || request.timeout_ms > MAX_TIMEOUT_MS {
        return Err(format!("timeout must be between 1 ms and {MAX_TIMEOUT_MS} ms"));
    }
    if request.max_output_bytes == 0 || request.max_output_bytes > MAX_OUTPUT_BYTES {
        return Err(format!("max_output_bytes must be between 1 and {MAX_OUTPUT_BYTES}"));
    }
    Ok(())
}

fn is_clean(text: &str) -> bool {
    text.len() <= MAX_STRING_BYTES && !text.contains('\0')
}

pub fn is_inside(path: &Path, root: &Path) -> bool {
    path != root && path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ExecRequest {
        ExecRequest {
            argv: vec!["go".into(), "test".into()],
            cwd: PathBuf::from("/work/snap"),
            env: BTreeMap::new(),
            timeout_ms: 1000,
            max_output_bytes: 8192,
        }
    }

    #[test]
    fn a_plain_request_is_valid() {
        assert_eq!(validate(&request()), Ok(()));
    }

    type Case = (fn(&mut ExecRequest), &'static str);

    #[test]
    fn invalid_fields_are_rejected() {
        let cases: Vec<Case> = vec![
            (|r| r.argv.clear(), "empty command"),
            (|r| r.argv[0].clear(), "empty command"),
            (|r| r.argv = vec!["x".into(); MAX_ARGS + 1], "too many arguments"),
            (|r| r.argv.push("a\0b".into()), "NUL"),
            (|r| r.cwd = PathBuf::from("relative"), "not absolute"),
            (|r| r.timeout_ms = 0, "timeout"),
            (|r| r.timeout_ms = MAX_TIMEOUT_MS + 1, "timeout"),
            (|r| r.max_output_bytes = 0, "max_output_bytes"),
            (|r| r.max_output_bytes = MAX_OUTPUT_BYTES + 1, "max_output_bytes"),
            (|r| _ = r.env.insert("A=B".into(), "x".into()), "invalid environment"),
            (|r| _ = r.env.insert(String::new(), "x".into()), "invalid environment"),
            (|r| _ = r.env.insert("A".into(), "x\0".into()), "invalid environment"),
            (
                |r| r.env = (0..=MAX_ENV_VARS).map(|i| (format!("V{i}"), String::new())).collect(),
                "too many environment",
            ),
        ];
        for (mutate, expected) in cases {
            let mut r = request();
            mutate(&mut r);
            let error = validate(&r).unwrap_err();
            assert!(error.contains(expected), "{error} should mention {expected}");
        }
    }

    #[test]
    fn inside_means_strictly_below_the_root() {
        let root = Path::new("/work");
        assert!(is_inside(Path::new("/work/a"), root));
        assert!(is_inside(Path::new("/work/a/b"), root));
        assert!(!is_inside(Path::new("/work"), root));
        assert!(!is_inside(Path::new("/workshop"), root));
        assert!(!is_inside(Path::new("/data"), root));
    }

    #[test]
    fn result_helpers() {
        let failed = ExecResult::failed(FailureKind::Unavailable, "down");
        assert_eq!(failed.failure.as_ref().unwrap().kind, FailureKind::Unavailable);
        assert!(!failed.success());
        assert!(ExecResult { exit_code: Some(0), ..ExecResult::default() }.success());
        assert!(!ExecResult { exit_code: Some(0), timed_out: true, ..ExecResult::default() }.success());
    }

    #[test]
    fn unknown_request_fields_are_refused_on_the_wire() {
        let json = r#"{"argv":["ls"],"cwd":"/w/a","timeout_ms":1,"max_output_bytes":1,"shell":true}"#;
        assert!(serde_json::from_str::<ExecRequest>(json).is_err());
        let json = r#"{"argv":["ls"],"cwd":"/w/a","timeout_ms":1,"max_output_bytes":1}"#;
        assert!(serde_json::from_str::<ExecRequest>(json).unwrap().env.is_empty());
    }
}
