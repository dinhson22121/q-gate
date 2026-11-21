use chrono::{DateTime, SecondsFormat, Utc};

use crate::watch::application::ports::Notifier;

#[derive(Debug, Clone, Copy, Default)]
pub struct LogNotifier;

impl LogNotifier {
    pub fn new() -> Self {
        Self
    }
}

impl Notifier for LogNotifier {
    async fn notify(&self, title: &str, body: &str) {
        println!("{}", log_line(Utc::now(), title, body));
    }
}

fn log_line(now: DateTime<Utc>, title: &str, body: &str) -> String {
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let time = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    if body.is_empty() { format!("{time} {title}") } else { format!("{time} {title} | {body}") }
}

#[derive(Debug, Clone)]
pub struct TeeNotifier<A, B> {
    first: A,
    second: B,
}

impl<A: Notifier, B: Notifier> TeeNotifier<A, B> {
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

impl<A: Notifier, B: Notifier> Notifier for TeeNotifier<A, B> {
    async fn notify(&self, title: &str, body: &str) {
        self.first.notify(title, body).await;
        self.second.notify(title, body).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::TimeZone;

    use super::*;

    #[test]
    fn formats_single_line() {
        let now = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(
            log_line(now, "✅ qgate: api abc passed", "line one\n  line two"),
            "2026-01-02T03:04:05Z ✅ qgate: api abc passed | line one line two"
        );
        assert_eq!(log_line(now, "title", " \n"), "2026-01-02T03:04:05Z title");
    }

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl Notifier for Recorder {
        async fn notify(&self, title: &str, _body: &str) {
            self.0.lock().unwrap().push(title.to_owned());
        }
    }

    #[tokio::test]
    async fn tee_delivers_to_both() {
        let (a, b) = (Recorder::default(), Recorder::default());
        TeeNotifier::new(a.clone(), b.clone()).notify("t", "b").await;
        LogNotifier::new().notify("t", "b").await;
        assert_eq!(*a.0.lock().unwrap(), vec!["t"]);
        assert_eq!(*b.0.lock().unwrap(), vec!["t"]);
    }
}
