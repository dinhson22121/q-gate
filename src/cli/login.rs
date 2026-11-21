//! Dashboard login wiring: the secret in effect and the login link (`qgate serve` prints it at startup,
//! `qgate login-url` on demand). The link is the only place the secret is ever printed.

use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;

use anyhow::{Context, Result};

use super::EXIT_OK;
use super::pipeline::AppContext;
use crate::access::api::LOGIN_PATH;
use crate::access::application::{ENV_DASHBOARD_TOKEN, ResolveDashboardSecret};
use crate::access::domain::DashboardSecret;
use crate::access::infrastructure::FileSecretStore;

/// Host port of the dashboard in Docker (`127.0.0.1:${QGATE_DASHBOARD_PORT}:8080`).
const ENV_DASHBOARD_PORT: &str = "QGATE_DASHBOARD_PORT";
const LOOPBACK_HOST: &str = "localhost";

/// `QGATE_DASHBOARD_TOKEN`, else `<data_dir>/dashboard-token` (created on first use).
pub fn dashboard_secret(context: &AppContext) -> Result<DashboardSecret> {
    let store = FileSecretStore::in_data_dir(&context.paths.data_dir);
    let env_value = (context.env)(ENV_DASHBOARD_TOKEN);
    ResolveDashboardSecret::new(&store).execute(env_value.as_deref()).context("cannot set up dashboard login")
}

/// `http://<host>:<port>/login?token=<secret>` for the configured `dashboard.bind`.
pub fn login_url(context: &AppContext, secret: &DashboardSecret) -> Result<String> {
    let bind = context.server().dashboard.bind_addr()?;
    let authority = public_authority(bind, (context.env)(ENV_DASHBOARD_PORT).as_deref());
    Ok(format!("http://{authority}{LOGIN_PATH}?token={}", secret.expose()))
}

/// `qgate login-url`.
pub fn print_login_url(context: &AppContext) -> Result<ExitCode> {
    let secret = dashboard_secret(context)?;
    println!("{}", login_url(context, &secret)?);
    Ok(ExitCode::from(EXIT_OK))
}

/// Where a browser on this machine reaches the dashboard: `localhost` for wildcard/loopback binds (the
/// default host the dashboard accepts), else the bound IP; the published port when known (Docker maps
/// it), else the bound one.
fn public_authority(bind: SocketAddr, published_port: Option<&str>) -> String {
    let port = published_port.and_then(|p| p.trim().parse::<u16>().ok()).filter(|p| *p != 0).unwrap_or(bind.port());
    let ip = bind.ip();
    let host = match ip {
        _ if ip.is_unspecified() || ip.is_loopback() => LOOPBACK_HOST.to_string(),
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    };
    format!("{host}:{port}")
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, RwLock};

    use super::*;
    use crate::settings::application::ServerConfig;
    use crate::settings::infrastructure::{AppPaths, FileStore, SqliteServerConfigStore};

    const ENV_SECRET: &str = "env-secret-0123456789-0123456789-abc";

    fn context(dir: &Path, vars: &'static [(&'static str, &'static str)]) -> AppContext {
        AppContext {
            paths: AppPaths::with_dirs(dir.join("config"), dir.join("data")),
            server: Arc::new(RwLock::new(ServerConfig::default())),
            store: Arc::new(FileStore::new(dir.join("config/credentials.toml"))),
            settings_store: Arc::new(SqliteServerConfigStore::open(dir.join("data/qgate.db")).unwrap()),
            env: Arc::new(move |name: &str| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())),
        }
    }

    #[test]
    fn generated_secret_is_persisted_and_reused_in_the_login_url() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(dir.path(), &[]);
        let secret = dashboard_secret(&context).unwrap();
        assert_eq!(dashboard_secret(&context).unwrap(), secret);
        assert!(dir.path().join("data/dashboard-token").is_file());
        let url = login_url(&context, &secret).unwrap();
        assert_eq!(url, format!("http://localhost:8080/login?token={}", secret.expose()));
        assert_eq!(print_login_url(&context).unwrap(), ExitCode::from(EXIT_OK));
    }

    #[test]
    fn env_secret_and_published_port_win() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(dir.path(), &[(ENV_DASHBOARD_TOKEN, ENV_SECRET), (ENV_DASHBOARD_PORT, "18080")]);
        let secret = dashboard_secret(&context).unwrap();
        assert_eq!(secret.expose(), ENV_SECRET);
        assert!(!dir.path().join("data/dashboard-token").exists(), "env secret must not create the file");
        assert_eq!(login_url(&context, &secret).unwrap(), format!("http://localhost:18080/login?token={ENV_SECRET}"));
    }

    #[test]
    fn invalid_env_secret_fails_without_printing_it() {
        let dir = tempfile::tempdir().unwrap();
        let context = context(dir.path(), &[(ENV_DASHBOARD_TOKEN, "short-leaky-value")]);
        let message = format!("{:#}", dashboard_secret(&context).unwrap_err());
        assert!(message.contains(ENV_DASHBOARD_TOKEN), "{message}");
        assert!(!message.contains("short-leaky-value"), "{message}");
    }

    fn addr(raw: &str) -> SocketAddr {
        raw.parse().unwrap()
    }

    #[test]
    fn wildcard_and_loopback_binds_use_localhost() {
        assert_eq!(public_authority(addr("0.0.0.0:8080"), None), "localhost:8080");
        assert_eq!(public_authority(addr("127.0.0.1:9000"), None), "localhost:9000");
        assert_eq!(public_authority(addr("[::]:8080"), None), "localhost:8080");
    }

    #[test]
    fn other_ips_are_used_as_is() {
        assert_eq!(public_authority(addr("192.168.1.5:8080"), None), "192.168.1.5:8080");
        assert_eq!(public_authority(addr("[fd00::1]:8080"), None), "[fd00::1]:8080");
    }

    #[test]
    fn published_port_wins_when_valid() {
        assert_eq!(public_authority(addr("0.0.0.0:8080"), Some(" 18080 ")), "localhost:18080");
        assert_eq!(public_authority(addr("0.0.0.0:8080"), Some("0")), "localhost:8080");
        assert_eq!(public_authority(addr("0.0.0.0:8080"), Some("http")), "localhost:8080");
    }
}
