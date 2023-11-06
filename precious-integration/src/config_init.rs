use crate::shared::{compile_precious, precious_path};
use anyhow::Result;
use precious_helpers::exec::{self, ExecOutput};
use pushd::Pushd;
use regex::Regex;
use serial_test::serial;
#[cfg(target_family = "unix")]
use std::os::unix::fs::PermissionsExt;
use std::{collections::HashMap, fs::File, path::Path};
use tempfile::TempDir;

#[test]
#[serial]
fn init_go() -> Result<()> {
    compile_precious()?;
    let (_td, _pd) = chdir_to_tempdir()?;
    let output = init_with_components(&["go"], None)?;

    assert_eq!(output.exit_code, 0);
    assert!(output.stderr.is_none());

    assert_file_exists("precious.toml")?;
    assert_file_contains("precious.toml", &["golangci-lint", "check-go-mod.sh"])?;
    assert_file_exists("golangci-lint.yml")?;
    assert_file_contains(
        "golangci-lint.yml",
        &["gofumpt", "govet", "check-type-assertions"],
    )?;
    assert_file_exists("dev/bin/check-go-mod.sh")?;
    #[cfg(target_family = "unix")]
    assert_file_is_executable("dev/bin/check-go-mod.sh")?;

    let stdout = output.stdout.unwrap();
    assert!(stdout.contains("dev/bin/check-go-mod.sh"));
    assert!(stdout.contains("https://golangci-lint.run"));

    Ok(())
}

#[test]
#[serial]
fn init_rust() -> Result<()> {
    compile_precious()?;
    let (_td, _pd) = chdir_to_tempdir()?;
    let output = init_with_components(&["rust"], None)?;

    assert_eq!(output.exit_code, 0);
    assert!(output.stderr.is_none());

    assert_file_exists("precious.toml")?;
    assert_file_contains("precious.toml", &["clippy", "rustfmt"])?;

    let stdout = output.stdout.unwrap();
    assert!(stdout.contains("clippy"));

    Ok(())
}

#[test]
#[serial]
fn init_perl() -> Result<()> {
    compile_precious()?;
    let (_td, _pd) = chdir_to_tempdir()?;
    let output = init_with_components(&["perl"], None)?;

    assert_eq!(output.exit_code, 0);
    assert!(output.stderr.is_none());

    assert_file_exists("precious.toml")?;
    assert_file_contains("precious.toml", &["perlcritic", "perlimports", "perltidy"])?;

    let stdout = output.stdout.unwrap();
    assert!(stdout.contains("App-perlimports"));

    Ok(())
}

#[test]
#[serial]
fn init_does_not_overwrite_existing_file() -> Result<()> {
    compile_precious()?;
    let (_td, _pd) = chdir_to_tempdir()?;

    File::create("precious.toml")?;
    let output = init_with_components(&["rust"], None)?;

    assert_eq!(output.exit_code, 1);
    assert!(output.stderr.is_some());
    assert!(output
        .stderr
        .unwrap()
        .contains("A file already exists at the given path: precious.toml"));

    Ok(())
}

#[test]
#[serial]
fn init_does_not_overwrite_existing_file_with_nonstandard_name() -> Result<()> {
    compile_precious()?;
    let (_td, _pd) = chdir_to_tempdir()?;

    File::create("my-precious.toml")?;
    let output = init_with_components(&["rust"], Some("my-precious.toml"))?;

    assert_eq!(output.exit_code, 1);
    assert!(output.stderr.is_some());
    assert!(output
        .stderr
        .unwrap()
        .contains("A file already exists at the given path: my-precious.toml"));

    Ok(())
}

fn chdir_to_tempdir() -> Result<(TempDir, Pushd)> {
    let td = tempfile::Builder::new()
        .prefix("precious-integration-")
        .tempdir()?;
    let pd = Pushd::new(td.path())?;
    Ok((td, pd))
}

fn init_with_components(components: &[&str], init_path: Option<&str>) -> Result<ExecOutput> {
    let precious = precious_path()?;
    let env = HashMap::new();
    let mut args = vec!["config", "init"];
    for c in components {
        args.push("--component");
        args.push(c);
    }
    if let Some(p) = init_path {
        args.push("--path");
        args.push(p);
    }
    exec::run(
        &precious,
        &args,
        &env,
        &[0, 1],
        Some(&[Regex::new(".*")?]),
        None,
    )
}

fn assert_file_exists(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    assert!(path.exists(), "file {:?} does not exist", path);
    Ok(())
}

fn assert_file_contains(path: impl AsRef<Path>, contains: &[&str]) -> Result<()> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path)?;
    for c in contains {
        assert!(
            contents.contains(c),
            "file {:?} does not contain {:?}",
            path,
            c,
        );
    }
    Ok(())
}

#[cfg(target_family = "unix")]
fn assert_file_is_executable(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let perms = path.metadata()?.permissions();
    assert!(
        perms.mode() & 0o111 != 0,
        "file {:?} is not executable",
        path,
    );
    Ok(())
}
