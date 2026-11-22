use tokio::process::Command;

pub(super) const CHILD_ENV_ALLOWLIST: [&str; 15] = [
    "HOME",
    "PATH",
    "USER",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "CLAUDE_CONFIG_DIR",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
];

pub(crate) fn scrub_child_env(command: &mut Command) -> &mut Command {
    let allowed = CHILD_ENV_ALLOWLIST.iter().filter_map(|name| std::env::var_os(name).map(|value| (*name, value)));
    command.env_clear().envs(allowed)
}
