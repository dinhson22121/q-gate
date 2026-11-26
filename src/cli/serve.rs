use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::sync::watch;

use super::agents::{redteam_service, rulebook_service};
use super::login::{dashboard_secret, login_url};
use super::pipeline::{AppContext, ReviewPipeline, ReviewRequest, load_review_config};
use super::policy::policy_service;
use crate::access::api::{AccessState, protect};
use crate::cli::pipeline::HISTORY_DB_FILE;
use crate::conventions::{RulebookApiState, router as rulebook_router};
use crate::feedback::application::FeedbackService;
use crate::feedback::infrastructure::SqliteFeedbackStore;
use crate::feedback::{FeedbackApiState, router as feedback_router};
use crate::history::api::{DashboardState, router_guarded, serve_router};
use crate::history::application::{RunRepository, Trigger};
use crate::policy::{PolicyApiState, router as policy_router};
use crate::redteam::api::{RedteamApiState, redteam_router};
use crate::review::application::NoProgress;
use crate::review::infrastructure::local_git::DiffTarget;
use crate::review::infrastructure::snapshot::sweep_stale_snapshots;
use crate::settings::api::{SettingsApiState, router as settings_router};
use crate::settings::domain::ReposConfig;
use crate::tickets::infrastructure::JiraOauthClient;
use crate::watch::api::{ReposApiState, router as repos_router};
use crate::watch::application::ports::{
    CommitEvent, CommitReviewer, CommitVerdict, ManualScanner, Notifier, WatchError,
};
use crate::watch::application::watch_commits::{WatchCommits, WatchConfig};
use crate::watch::infrastructure::composite_registry::CompositeRegistry;
use crate::watch::infrastructure::fs_browser::repos_root_from_env;
use crate::watch::infrastructure::git_inspector::GitRepoInspector;
use crate::watch::infrastructure::instance_guard::InstanceGuard;
use crate::watch::infrastructure::log_notifier::{LogNotifier, TeeNotifier};
use crate::watch::infrastructure::scan_registry::ScanRepoRegistry;
use crate::watch::infrastructure::toml_registry::TomlRepoRegistry;
use crate::watch::infrastructure::webhook_notifier::WebhookNotifier;
use crate::web::ApiToken;

const LOCK_FILE: &str = "watch.lock";
pub const REPOS_FILE: &str = "repos.toml";

pub async fn serve(context: Arc<AppContext>) -> Result<()> {
    harden_process();
    let _guard = InstanceGuard::acquire(&context.paths.data_dir.join(LOCK_FILE))
        .context("another qgate service is already running")?;
    let history = context.open_history()?;
    sweep_stale_snapshots(&context.work_dir());
    let server = context.server();
    let addr = server.dashboard.bind_addr()?;
    let secret = dashboard_secret(&context)?;
    let login_link = login_url(&context, &secret)?;
    let listener = TcpListener::bind(addr).await.with_context(|| format!("cannot listen on {addr}"))?;

    let stop_tx = Arc::new(watch::channel(false).0);
    let stop_rx = stop_tx.subscribe();
    let signal_tx = stop_tx.clone();
    tokio::spawn(async move {
        wait_for_termination().await;
        let _ = signal_tx.send(true);
    });

    let hosts = server.dashboard.allowed_hosts.clone();
    let settings_deps = context.settings_deps();
    let rulebooks = rulebook_service(context.clone())?;
    let policy = policy_service(context.clone(), settings_deps.clone(), rulebooks.clone(), history.clone())?;
    let redteam = redteam_service(context.clone())?;
    let scanner: Arc<dyn ManualScanner> =
        Arc::new(PipelineScanner { context: context.clone(), history: history.clone() });
    let token = ApiToken::generate().context("cannot generate the dashboard API token")?;
    let state = DashboardState::with_allowed_hosts(history.clone(), server.dashboard.allowed_hosts.clone())
        .with_token(token.clone());
    let feedback = Arc::new(FeedbackService::new(Arc::new(SqliteFeedbackStore::open(
        context.paths.data_dir.join(HISTORY_DB_FILE),
    )?)));
    let settings_page = settings_router(SettingsApiState::new(
        settings_deps,
        hosts.clone(),
        token.clone(),
        Arc::new(JiraOauthClient::atlassian()),
    ))
    .merge(rulebook_router(RulebookApiState::new(rulebooks, hosts.clone(), token.clone())))
    .merge(repos_router(
        ReposApiState::new(
            TomlRepoRegistry::new(context.paths.config_dir.join(REPOS_FILE)),
            repos_root_from_env(),
            hosts.clone(),
            token.clone(),
        )
        .with_scanner(scanner),
    ))
    .merge(redteam_router(RedteamApiState::new(redteam, hosts.clone(), token.clone())))
    .merge(feedback_router(FeedbackApiState::new(feedback, history.clone(), hosts.clone(), token.clone())))
    .merge(policy_router(PolicyApiState::new(policy, hosts, token)));
    let dashboard_stop = stop_tx.clone();
    let dashboard_shutdown = stopped(stop_rx.clone());
    let access = AccessState::new(secret);
    let app = router_guarded(state, settings_page, |routes| protect(routes, access));
    let dashboard = async move {
        let result = serve_router(listener, app, dashboard_shutdown).await;
        let _ = dashboard_stop.send(true);
        result
    };
    let watcher = WatchCommits::new(
        GitRepoInspector::new(),
        PipelineReviewer { pipeline: ReviewPipeline::new(context.clone(), Some(history)) },
        build_notifier(&server.notify.webhook_url)?,
        repo_source(&context),
        WatchConfig {
            poll_interval: Duration::from_secs(server.watch.poll_interval_secs),
            max_concurrent_reviews: server.watch.max_concurrent_reviews,
            remote: server.watch.remote.clone(),
            base_branch: server.watch.base_branch.clone(),
            ..WatchConfig::default()
        },
    );
    println!(
        "qgate: dashboard on http://{addr} · settings on http://{addr}/settings · watching {}",
        watched_description(&server.repos)
    );
    println!("Dashboard: {login_link}");
    let (dashboard_result, report) = tokio::join!(dashboard, watcher.run(stopped(stop_rx)));
    println!("qgate: stopped after {} review(s), {} aborted", report.reviews_completed, report.reviews_aborted);
    dashboard_result.with_context(|| format!("dashboard failed on {addr}"))
}

pub fn repo_source(context: &AppContext) -> CompositeRegistry<TomlRepoRegistry, ScanRepoRegistry> {
    imported_repo_source(&context.paths.config_dir.join(REPOS_FILE), &context.server().repos)
}

fn imported_repo_source(
    imported_file: &Path,
    repos: &ReposConfig,
) -> CompositeRegistry<TomlRepoRegistry, ScanRepoRegistry> {
    let scanned_roots = repos.scanned_roots().iter().map(PathBuf::from).collect();
    CompositeRegistry::new(
        TomlRepoRegistry::new(imported_file.to_path_buf()),
        ScanRepoRegistry::new(scanned_roots).with_max_depth(repos.scan_depth as usize),
    )
}

fn watched_description(repos: &ReposConfig) -> String {
    if repos.auto_scan {
        format!("imported repositories and every repository under {}", repos.roots.join(", "))
    } else {
        "imported repositories only".into()
    }
}

struct PipelineReviewer {
    pipeline: ReviewPipeline,
}

impl CommitReviewer for PipelineReviewer {
    async fn review_commit(&self, event: &CommitEvent) -> Result<CommitVerdict, WatchError> {
        let enabled = load_review_config(&event.repo.path, &review_target(event))
            .await
            .map(|c| c.watch.enabled && c.watch.review_on_push)
            .map_err(|e| WatchError::Config(format!("{e:#}")))?;
        if !enabled {
            return Ok(CommitVerdict {
                passed: true,
                blocking_issues: 0,
                summary: "push review disabled in .qgate.toml".into(),
                run_id: None,
            });
        }
        let request = ReviewRequest {
            repo_root: event.repo.path.clone(),
            target: review_target(event),
            trigger: Trigger::Push,
            explicit_ticket: None,
            branch: event.branch.clone(),
            ticket_in_progress: false,
        };
        let outcome =
            self.pipeline.run(&request, &NoProgress).await.map_err(|e| WatchError::Review(format!("{e:#}")))?;
        let gate = &outcome.report.gate;
        let blocking_issues = gate.blocking.len() + gate.unmet_criteria.len() + gate.failed_checks.len();
        Ok(CommitVerdict {
            passed: gate.passed,
            blocking_issues,
            summary: summarize(&outcome.report, event.note.as_deref()),
            run_id: outcome.run_id,
        })
    }
}

struct PipelineScanner {
    context: Arc<AppContext>,
    history: Arc<dyn RunRepository>,
}

impl ManualScanner for PipelineScanner {
    fn scan(&self, repo_root: PathBuf) {
        let context = self.context.clone();
        let history = self.history.clone();
        tokio::spawn(async move {
            let target = resolve_base_target(&repo_root, &context).await;
            let request = ReviewRequest {
                repo_root,
                target,
                trigger: Trigger::Manual,
                explicit_ticket: None,
                branch: None,
                ticket_in_progress: false,
            };
            if let Err(e) = ReviewPipeline::new(context, Some(history)).run(&request, &NoProgress).await {
                eprintln!("qgate: manual scan failed: {e:#}");
            }
        });
    }
}

pub(super) async fn resolve_base_target(root: &Path, context: &AppContext) -> DiffTarget {
    let head = git_rev(root, "HEAD").await;
    let configured = context.server().watch.base_branch;
    for candidate in [configured.as_str(), "origin/main", "main", "origin/master", "master"] {
        if candidate.is_empty() {
            continue;
        }
        if let Some(base) = git_rev(root, candidate).await
            && head.as_deref() != Some(base.as_str())
        {
            return DiffTarget::Base(candidate.to_string());
        }
    }
    DiffTarget::Commit(head.unwrap_or_else(|| "HEAD".into()))
}

async fn git_rev(root: &Path, rev: &str) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--quiet", rev])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

fn review_target(event: &CommitEvent) -> DiffTarget {
    match &event.base {
        Some(base) => DiffTarget::Range { base: base.clone(), head: event.sha.clone() },
        None => DiffTarget::Commit(event.sha.clone()),
    }
}

fn summarize(report: &crate::review::application::ReviewReport, note: Option<&str>) -> String {
    let prefix = note.map(|n| format!("{n}; ")).unwrap_or_default();
    format!("{prefix}{}", summarize_report(report))
}

fn summarize_report(report: &crate::review::application::ReviewReport) -> String {
    let criteria = report
        .requirements
        .as_ref()
        .map(|r| format!(", AC {}/{} met", r.met_count(), r.assessments.len()))
        .unwrap_or_default();
    format!("{} finding(s){criteria}, {} check(s)", report.findings.len(), report.checks.len())
}

enum AnyNotifier {
    Log(LogNotifier),
    LogAndWebhook(TeeNotifier<LogNotifier, WebhookNotifier>),
}

impl Notifier for AnyNotifier {
    async fn notify(&self, title: &str, body: &str) {
        match self {
            Self::Log(n) => n.notify(title, body).await,
            Self::LogAndWebhook(n) => n.notify(title, body).await,
        }
    }
}

fn build_notifier(webhook_url: &str) -> Result<AnyNotifier> {
    if webhook_url.trim().is_empty() {
        return Ok(AnyNotifier::Log(LogNotifier::new()));
    }
    let webhook = WebhookNotifier::new(webhook_url).context("invalid notify.webhook_url")?;
    Ok(AnyNotifier::LogAndWebhook(TeeNotifier::new(LogNotifier::new(), webhook)))
}

async fn stopped(mut stop: watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopped| *stopped).await;
}

fn harden_process() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl(PR_SET_DUMPABLE, 0) only changes this process's dumpable flag.
        let result = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
        if result != 0 {
            eprintln!("qgate: warning: could not mark the process non-dumpable");
        }
    }
}

pub(super) async fn wait_for_termination() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::application::ports::{RepoRegistry, RepoSource};

    #[test]
    fn notifier_uses_webhook_only_when_configured() {
        assert!(matches!(build_notifier("").unwrap(), AnyNotifier::Log(_)));
        assert!(matches!(build_notifier("https://hooks.example.com/x").unwrap(), AnyNotifier::LogAndWebhook(_)));
        assert!(build_notifier("ftp://nope").is_err());
    }

    #[tokio::test]
    async fn stopped_resolves_after_signal() {
        let (tx, rx) = watch::channel(false);
        let waiter = tokio::spawn(stopped(rx));
        tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), waiter).await.unwrap().unwrap();
    }

    #[test]
    fn review_target_covers_the_whole_pushed_branch() {
        use crate::watch::application::ports::WatchedRepo;
        let mut event = CommitEvent {
            repo: WatchedRepo::from_path(PathBuf::from("/repos/a")),
            sha: "c4".into(),
            base: Some("c1".into()),
            branch: Some("feature".into()),
            note: None,
        };
        assert_eq!(review_target(&event), DiffTarget::Range { base: "c1".into(), head: "c4".into() });
        event.base = None;
        assert_eq!(review_target(&event), DiffTarget::Commit("c4".into()));
    }

    #[test]
    fn summarize_prefixes_a_note_when_present() {
        let report = crate::review::application::ReviewReport::default();
        assert!(summarize(&report, Some("no merge-base; reviewing whole branch")).starts_with("no merge-base;"));
        assert!(!summarize(&report, None).starts_with(';'));
    }

    fn init_git_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        let status = std::process::Command::new("git").args(["init", "-q"]).current_dir(path).status().unwrap();
        assert!(status.success());
    }

    struct Folder {
        _dir: tempfile::TempDir,
        root: PathBuf,
        imported_file: PathBuf,
    }

    fn folder_with_imported_and_sibling_repo() -> Folder {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("repos");
        init_git_repo(&root.join("imported"));
        init_git_repo(&root.join("sibling"));
        let imported_file = dir.path().join("config").join(REPOS_FILE);
        Folder { _dir: dir, root, imported_file }
    }

    async fn watched_names(folder: &Folder, auto_scan: bool) -> Vec<String> {
        let registry = TomlRepoRegistry::new(folder.imported_file.clone());
        registry.add(&folder.root.join("imported")).await.unwrap();
        let repos = ReposConfig { roots: vec![folder.root.display().to_string()], auto_scan, ..ReposConfig::default() };
        let mut names: Vec<String> = imported_repo_source(&folder.imported_file, &repos)
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|repo| repo.name)
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn a_repository_that_was_not_imported_is_never_watched_by_default() {
        let folder = folder_with_imported_and_sibling_repo();

        assert_eq!(watched_names(&folder, false).await, vec!["imported".to_string()]);
    }

    #[tokio::test]
    async fn auto_scan_also_watches_every_repository_under_the_roots() {
        let folder = folder_with_imported_and_sibling_repo();

        assert_eq!(watched_names(&folder, true).await, vec!["imported".to_string(), "sibling".to_string()]);
    }

    #[test]
    fn watched_description_says_what_is_watched() {
        let imported_only = ReposConfig::default();
        let scanning = ReposConfig { auto_scan: true, ..ReposConfig::default() };

        assert_eq!(watched_description(&imported_only), "imported repositories only");
        assert!(watched_description(&scanning).contains("/repos"));
    }
}
