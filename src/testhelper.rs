#[cfg(test)]
use crate::command;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use tempfile::{tempdir, TempDir};

pub struct TestHelper {
    // While we never access this field we need to hold onto the tempdir or
    // else the directory it references will be deleted.
    _tempdir: TempDir,
    root: PathBuf,
    paths: Vec<PathBuf>,
    root_gitignore_file: PathBuf,
    tests_data_gitignore_file: PathBuf,
}

impl TestHelper {
    const PATHS: &'static [&'static str] = &[
        "README.md",
        "can_ignore.x",
        "src/can_ignore.rs",
        "src/bar.rs",
        "src/main.rs",
        "src/module.rs",
        "merge-conflict-file",
        "tests/data/foo.txt",
        "tests/data/bar.txt",
        "tests/data/generated.txt",
    ];

    pub fn new() -> Result<Self> {
        let temp = tempdir()?;
        let root = maybe_canonicalize(temp.path())?;
        let helper = TestHelper {
            _tempdir: temp,
            root,
            paths: Self::PATHS.iter().map(PathBuf::from).collect(),
            root_gitignore_file: PathBuf::from(".gitignore"),
            tests_data_gitignore_file: PathBuf::from("tests/data/.gitignore"),
        };
        Ok(helper)
    }

    pub fn with_git_repo(self) -> Result<Self> {
        self.create_git_repo()?;
        Ok(self)
    }

    pub fn with_config_file(self, file_name: &str, content: &str) -> Result<Self> {
        if cfg!(windows) {
            self.write_file(&self.config_file(file_name), &content.replace('\n', "\r\n"))?;
        } else {
            self.write_file(&self.config_file(file_name), content)?;
        }
        Ok(self)
    }

    pub fn pushd_to_root(&self) -> Result<Pushd> {
        Pushd::new(self.root.clone())
    }

    fn create_git_repo(&self) -> Result<()> {
        for p in self.paths.iter() {
            self.write_file(p, "some content")?;
        }

        self.run_git(&["init", "--initial-branch", "master"])?;

        // If the tests are run in a totally clean environment they will blow
        // up if this isnt't set. This fixes
        // https://github.com/houseabsolute/precious/issues/15.
        self.run_git(&["config", "user.email", "precious@example.com"])?;
        // With this on I get line ending warnings from git on Windows if I
        // don't write out files with CRLF. Disabling this simplifies things
        // greatly.
        self.run_git(&["config", "core.autocrlf", "false"])?;

        self.stage_all()?;
        self.run_git(&["commit", "-m", "initial commit"])?;

        Ok(())
    }

    pub fn root(&self) -> PathBuf {
        self.root.clone()
    }

    pub fn config_file(&self, file_name: &str) -> PathBuf {
        let mut path = self.root.clone();
        path.push(file_name);
        path
    }

    pub fn all_files(&self) -> Vec<PathBuf> {
        self.paths.to_vec()
    }

    pub fn stage_all(&self) -> Result<()> {
        self.run_git(&["add", "."])
    }

    pub fn commit_all(&self) -> Result<()> {
        self.run_git(&["commit", "-a", "-m", "committed"])
    }

    const ROOT_GITIGNORE: &'static str = "
/**/bar.*
can_ignore.*
";

    const TESTS_DATA_GITIGNORE: &'static str = "
generated.*
";

    pub fn non_ignored_files() -> Vec<PathBuf> {
        Self::PATHS
            .iter()
            .filter_map(|&p| {
                if p.contains("can_ignore") || p.contains("bar.") || p.contains("generated.txt") {
                    None
                } else {
                    Some(PathBuf::from(p))
                }
            })
            .collect()
    }

    pub fn switch_to_branch(&self, branch: &str, exists: bool) -> Result<()> {
        let mut args: Vec<&str> = vec!["checkout", "--quiet"];
        if !exists {
            args.push("-b");
        }
        args.push(branch);
        command::run_command(
            "git".to_string(),
            args.iter().map(|a| a.to_string()).collect(),
            &HashMap::new(),
            &[0],
            false,
            Some(&self.root()),
        )?;
        Ok(())
    }

    pub fn merge_master(&self, expect_fail: bool) -> Result<()> {
        let mut expect_codes = [0].to_vec();
        if expect_fail {
            expect_codes.push(1);
        }

        command::run_command(
            "git".to_string(),
            ["merge", "--quiet", "--no-ff", "--no-commit", "master"]
                .iter()
                .map(|a| a.to_string())
                .collect(),
            &HashMap::new(),
            &expect_codes,
            true,
            Some(&self.root()),
        )?;
        Ok(())
    }

    pub fn add_gitignore_files(&self) -> Result<Vec<PathBuf>> {
        self.write_file(&self.root_gitignore_file, Self::ROOT_GITIGNORE)?;
        self.write_file(&self.tests_data_gitignore_file, Self::TESTS_DATA_GITIGNORE)?;

        Ok(vec![
            self.root_gitignore_file.clone(),
            self.tests_data_gitignore_file.clone(),
        ])
    }

    fn run_git(&self, args: &[&str]) -> Result<()> {
        command::run_command(
            "git".to_string(),
            args.iter().map(|a| a.to_string()).collect(),
            &HashMap::new(),
            &[0],
            false,
            Some(&self.root),
        )?;
        Ok(())
    }

    const TO_MODIFY: &'static [&'static str] = &["src/module.rs", "tests/data/foo.txt"];

    pub fn modify_files(&self) -> Result<Vec<PathBuf>> {
        let mut paths: Vec<PathBuf> = vec![];
        for p in Self::TO_MODIFY.iter().map(PathBuf::from) {
            self.write_file(&p, "new content")?;
            paths.push(p.clone());
        }
        Ok(paths)
    }

    pub fn write_file(&self, rel: &Path, content: &str) -> Result<()> {
        let mut full = self.root.clone();
        full.push(rel);
        fs::create_dir_all(full.parent().unwrap()).with_context(|| {
            format!(
                "Creating dir at {}",
                full.parent().unwrap().to_string_lossy(),
            )
        })?;
        let mut file = fs::File::create(full.clone())
            .context(format!("Creating file at {}", full.to_string_lossy()))?;
        file.write_all(content.as_bytes())
            .context(format!("Writing to file at {}", full.to_string_lossy()))?;

        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    pub fn read_file(&self, rel: &Path) -> Result<String> {
        let mut full = self.root.clone();
        full.push(rel);
        let content = fs::read_to_string(full.clone())
            .context(format!("Reading file at {}", full.to_string_lossy()))?;

        Ok(content)
    }
}

pub struct Pushd(PathBuf);

impl Pushd {
    pub fn new(path: PathBuf) -> Result<Pushd> {
        let cwd = env::current_dir()?;
        env::set_current_dir(path)?;
        Ok(Pushd(cwd))
    }
}

impl Drop for Pushd {
    fn drop(&mut self) {
        // If the original path was a tempdir it may be gone now.
        if !self.0.exists() {
            return;
        }

        let res = env::set_current_dir(&self.0);
        if let Err(e) = res {
            panic!(
                "Could not return to original dir, {}: {}",
                self.0.to_string_lossy(),
                e,
            );
        }
    }
}

// The temp directory on macOS in GitHub Actions appears to be a symlink, but
// canonicalizing on Windows breaks tests for some reason.
pub fn maybe_canonicalize(path: &Path) -> Result<PathBuf> {
    if cfg!(windows) {
        return Ok(path.to_owned());
    }

    Ok(fs::canonicalize(path)?)
}
