//! One parser per tool: its JSON report to [`QualityFinding`]s (`is_new` is decided later by the change index).
//! `None` means the output holds no report at all (the tool failed); an empty list means a clean report.

pub mod gitleaks;
pub mod golangci;
pub mod govulncheck;
pub mod osv;
