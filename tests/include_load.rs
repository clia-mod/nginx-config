use std::fs;
extern crate tempfile;
extern crate clia_nginx_config;
use tempfile::tempdir;

use clia_nginx_config::parse_main_from_file;

#[test]
fn include_files_are_loaded() {
    let dir = tempdir().unwrap();
    let inc1 = dir.path().join("a.conf");
    fs::write(&inc1, "daemon off;\n").unwrap();
    let inc2 = dir.path().join("b.conf");
    fs::write(&inc2, "server { listen 8080; }\n").unwrap();

    let main = dir.path().join("main.conf");
    fs::write(&main, "include *.conf;\n").unwrap();

    let m = parse_main_from_file(&main).unwrap();
    // should have two directives from the included files
    let names: Vec<_> = m.directives.iter().map(|d| d.item.directive_name()).collect();
    assert!(names.contains(&"daemon"));
    assert!(names.contains(&"server"));
}

#[test]
fn include_with_variable_in_path() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("inc");
    std::fs::create_dir_all(&sub).unwrap();
    let inc1 = sub.join("a.conf");
    fs::write(&inc1, "daemon off;\n").unwrap();

    let main = dir.path().join("main.conf");
    fs::write(&main, "set $subdir inc; include $subdir/*.conf;\n").unwrap();

    let m = parse_main_from_file(&main).unwrap();
    let names: Vec<_> = m.directives.iter().map(|d| d.item.directive_name()).collect();
    assert!(names.contains(&"daemon"));
}

#[test]
fn include_repo_fixture_is_loaded() {
    use std::path::Path;
    let path = Path::new("tests/include/nginx.conf");
    let m = parse_main_from_file(&path).unwrap();
    // find http block and assert it contains a server directive from the included file
    let http = m.directives.iter().find(|d| d.item.directive_name() == "http").expect("http block");
    let children = http.item.children().unwrap();
    let has_server = children.iter().any(|c| c.item.directive_name() == "server");
    assert!(has_server);
}

#[test]
fn mime_types_directive_present() {
    use std::path::Path;
    let path = Path::new("tests/include/nginx.conf");
    let m = parse_main_from_file(&path).unwrap();
    let http = m.directives.iter().find(|d| d.item.directive_name() == "http").expect("http block");
    let children = http.item.children().unwrap();
    assert!(children.iter().any(|c| c.item.directive_name() == "types"));
}

#[test]
fn parse_main_expands_include_using_cwd() {
    use std::fs::File;
    use std::io::Read;
    use std::path::Path;

    // Read fixture file into a string and replace the relative include with an
    // absolute path pattern so the expansion doesn't depend on global cwd.
    let mut buf = String::new();
    File::open("tests/include/nginx.conf").unwrap().read_to_string(&mut buf).unwrap();
    let base = Path::new("tests/include").canonicalize().unwrap();
    let pat = format!("{}/conf/*.conf", base.to_string_lossy());
    let buf = buf.replace("include conf/*.conf;", &format!("include {} ;", pat));

    let m = clia_nginx_config::parse_main(&buf).unwrap();

    let http = m.directives.iter().find(|d| d.item.directive_name() == "http").expect("http block");
    let children = http.item.children().unwrap();
    assert!(children.iter().any(|c| c.item.directive_name() == "server"));
}

