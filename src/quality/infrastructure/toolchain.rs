use std::path::{Path, PathBuf};

const BIN_DIR: &str = "current/bin";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolchainPaths {
    pub gitleaks: Option<PathBuf>,
    pub osv_scanner: Option<PathBuf>,
    pub govulncheck: Option<PathBuf>,
    pub golangci_lint: Option<PathBuf>,
}

impl ToolchainPaths {
    pub fn discover(toolchains_dir: &Path, path_var: Option<&str>) -> Self {
        let find = |tool: &str| locate(tool, toolchains_dir, path_var);
        Self {
            gitleaks: find("gitleaks"),
            osv_scanner: find("osv-scanner"),
            govulncheck: find("govulncheck"),
            golangci_lint: find("golangci-lint"),
        }
    }
}

fn locate(tool: &str, toolchains_dir: &Path, path_var: Option<&str>) -> Option<PathBuf> {
    let installed = toolchains_dir.join(tool).join(BIN_DIR).join(tool);
    if is_executable(&installed) {
        return Some(installed);
    }
    std::env::split_paths(path_var?).map(|dir| dir.join(tool)).find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn install(dir: &Path, relative: &str, mode: u32) -> PathBuf {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn finds_toolchain_installs_then_path_and_ignores_non_executables() {
        let tools = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let gitleaks = install(tools.path(), "gitleaks/current/bin/gitleaks", 0o755);
        install(tools.path(), "osv-scanner/current/bin/osv-scanner", 0o644);
        let on_path = install(bin.path(), "govulncheck", 0o755);
        let path_var = std::env::join_paths([bin.path()]).unwrap();

        let paths = ToolchainPaths::discover(tools.path(), path_var.to_str());

        assert_eq!(paths.gitleaks, Some(gitleaks));
        assert_eq!(paths.osv_scanner, None, "not executable");
        assert_eq!(paths.govulncheck, Some(on_path));
        assert_eq!(paths.golangci_lint, None);
        assert_eq!(ToolchainPaths::discover(tools.path(), None).govulncheck, None);
    }
}
