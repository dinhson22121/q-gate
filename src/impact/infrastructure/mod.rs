//! Go adapter: `go list` / `go test -json` run through the sandbox backend.

mod go_cli;
mod go_list;
mod test_json;

pub use go_cli::{GoCli, GoLimits};
pub use go_list::parse_package_lines;
pub use test_json::parse_test_json;
