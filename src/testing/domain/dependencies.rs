const GO_WORK: &str = "go.work";
const LOCAL_MARKERS: [&str; 17] = [
    "../",
    "=> ./",
    "=> /",
    "path =",
    "path=",
    "path:",
    "[workspace]",
    "\"file:",
    "\"link:",
    "\"workspace:",
    "\"workspaces\"",
    "project(",
    "includeBuild",
    "<relativePath>",
    "<module>",
    "-e ",
    "file://",
];

pub fn has_local_dependency(manifest_name: &str, text: &str) -> bool {
    manifest_name == GO_WORK || LOCAL_MARKERS.iter().any(|marker| text.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_local_path_dependencies_across_ecosystems() {
        for (name, text) in [
            ("go.mod", "module a\n\nreplace example.com/b => ../b\n"),
            ("go.mod", "replace example.com/b => ./vendor/b"),
            ("Cargo.toml", "[dependencies]\nb = { path = \"../b\" }\n"),
            ("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n"),
            ("package.json", "{\"dependencies\": {\"b\": \"file:../b\"}}"),
            ("package.json", "{\"workspaces\": [\"packages/*\"]}"),
            ("build.gradle", "implementation project(':lib')"),
            ("settings.gradle", "includeBuild '../x'"),
            ("pom.xml", "<parent><relativePath>../pom.xml</relativePath></parent>"),
            ("go.work", "go 1.22\nuse ./a\n"),
        ] {
            assert!(has_local_dependency(name, text), "{name}: {text}");
        }
    }

    #[test]
    fn plain_registry_dependencies_are_not_local() {
        assert!(!has_local_dependency("go.mod", "module a\n\nrequire example.com/b v1.2.3\n"));
        assert!(!has_local_dependency("package.json", "{\"dependencies\": {\"b\": \"^1.0.0\"}}"));
        assert!(!has_local_dependency("Cargo.toml", "[dependencies]\nserde = \"1\"\n"));
    }
}
