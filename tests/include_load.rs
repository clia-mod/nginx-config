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

