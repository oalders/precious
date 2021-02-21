use crate::basepaths;
use crate::chars;
use crate::config;
use crate::filter;
use crate::vcs;
use anyhow::{Error, Result};
use clap::{App, Arg, ArgGroup, ArgMatches, SubCommand};
use fern::colors::{Color, ColoredLevelConfig};
use fern::Dispatch;
use log::{debug, error};
use rayon::{prelude::*, ThreadPool, ThreadPoolBuilder};
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
enum PreciousError {
    #[error(r#"Could not parse {arg:} argument, "{val:}", as an integer"#)]
    InvalidIntegerArgument { arg: String, val: String },

    #[error("Could not find a VCS checkout root starting from {cwd:}")]
    CannotFindRoot { cwd: String },

    #[error("No {what:} filters defined in your config")]
    NoFilters { what: String },
}

#[derive(Debug)]
struct Exit {
    status: i8,
    message: Option<String>,
    error: Option<String>,
}

impl From<Error> for Exit {
    fn from(err: Error) -> Exit {
        Exit {
            status: 1,
            message: None,
            error: Some(err.to_string()),
        }
    }
}

#[derive(Debug)]
struct ActionError {
    error: String,
    config_key: String,
    path: PathBuf,
}

#[derive(Debug)]
pub struct Precious<'a> {
    matches: &'a ArgMatches<'a>,
    config: Option<config::Config>,
    cwd: PathBuf,
    root: Option<PathBuf>,
    config_file: Option<PathBuf>,
    chars: chars::Chars,
    quiet: bool,
    basepaths: Option<basepaths::BasePaths>,
    thread_pool: ThreadPool,
}

pub fn app<'a>() -> App<'a, 'a> {
    App::new("precious")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Dave Rolsky <autarch@urth.org>")
        .about("One code quality tool to rule them all")
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .takes_value(true)
                .help("Path to config file"),
        )
        .arg(
            Arg::with_name("jobs")
                .short("j")
                .long("jobs")
                .takes_value(true)
                .help("Number of parallel jobs (threads) to run (defaults to one per core)"),
        )
        .arg(
            Arg::with_name("ascii")
                .long("ascii")
                .help("Replace super-fun Unicode symbols with terribly boring ASCII"),
        )
        .arg(
            Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .help("Enable verbose output"),
        )
        .arg(
            Arg::with_name("debug")
                .short("d")
                .long("debug")
                .help("Enable debugging output"),
        )
        .arg(
            Arg::with_name("trace")
                .short("t")
                .long("trace")
                .help("Enable tracing output (maximum logging)"),
        )
        .arg(
            Arg::with_name("quiet")
                .short("q")
                .long("quiet")
                .help("Suppresses most output"),
        )
        .group(ArgGroup::with_name("log-level").args(&["verbose", "debug", "trace", "quiet"]))
        .subcommand(common_subcommand(
            "tidy",
            "Tidies the specified files and/or directories",
        ))
        .subcommand(common_subcommand(
            "lint",
            "Lints the specified files and/or directories",
        ))
}

fn common_subcommand<'a>(name: &'a str, about: &'a str) -> App<'a, 'a> {
    SubCommand::with_name(name)
        .about(about)
        .arg(
            Arg::with_name("all")
                .short("a")
                .long("all")
                .help("Run against all files in the current directory and below"),
        )
        .arg(
            Arg::with_name("git")
                .short("g")
                .long("git")
                .help("Run against files that have been modified according to git"),
        )
        .arg(
            Arg::with_name("staged")
                .short("s")
                .long("staged")
                .help("Run against file content that is staged for a git commit"),
        )
        .arg(
            Arg::with_name("paths")
                .multiple(true)
                .takes_value(true)
                .help("A list of paths on which to operate"),
        )
        .group(
            ArgGroup::with_name("operate-on")
                .args(&["all", "git", "staged", "paths"])
                .required(true),
        )
}

pub fn init_logger(matches: &ArgMatches) -> Result<(), log::SetLoggerError> {
    let line_colors = ColoredLevelConfig::new()
        .error(Color::Red)
        .warn(Color::Yellow)
        .info(Color::BrightBlack)
        .debug(Color::BrightBlack)
        .trace(Color::BrightBlack);

    let level = if matches.is_present("trace") {
        log::LevelFilter::Trace
    } else if matches.is_present("debug") {
        log::LevelFilter::Debug
    } else if matches.is_present("verbose") {
        log::LevelFilter::Info
    } else {
        log::LevelFilter::Warn
    };

    let level_colors = line_colors.clone().info(Color::Green).debug(Color::Black);

    Dispatch::new()
        .format(move |out, message, record| {
            out.finish(format_args!(
                "{color_line}[{target}][{level}{color_line}] {message}\x1B[0m",
                color_line = format_args!(
                    "\x1B[{}m",
                    line_colors.get_color(&record.level()).to_fg_str()
                ),
                target = record.target(),
                level = level_colors.color(record.level()),
                message = message,
            ));
        })
        .level(level)
        .chain(std::io::stderr())
        .apply()
}

impl<'a> Precious<'a> {
    pub fn new(matches: &'a ArgMatches) -> Result<Precious<'a>> {
        let c = if matches.is_present("ascii") {
            chars::BORING_CHARS
        } else {
            chars::FUN_CHARS
        };

        let mut s = Precious {
            matches,
            config: None,
            cwd: env::current_dir()?,
            root: None,
            config_file: None,
            chars: c,
            quiet: matches.is_present("quiet"),
            basepaths: None,
            thread_pool: ThreadPoolBuilder::new()
                .num_threads(Self::jobs(matches)?)
                .build()?,
        };
        s.set_config()?;

        Ok(s)
    }

    fn jobs(matches: &'a ArgMatches) -> Result<usize> {
        match matches.value_of("jobs") {
            Some(j) => match j.parse::<usize>() {
                Ok(u) => Ok(u),
                Err(_) => Err(PreciousError::InvalidIntegerArgument {
                    arg: "--jobs".to_string(),
                    val: j.to_string(),
                }
                .into()),
            },
            None => Ok(0),
        }
    }

    fn set_config(&mut self) -> Result<()> {
        self.set_root()?;
        let file = if self.matches.is_present("config") {
            let conf_file = self.matches.value_of("config").unwrap();
            debug!("Loading config from {} (set via flag)", conf_file);
            PathBuf::from(conf_file)
        } else {
            let default = self.default_config_file();
            debug!(
                "Loading config from {} (default location)",
                default.to_string_lossy()
            );
            default
        };

        self.config_file = Some(file.clone());
        self.config = Some(config::Config::new(file)?);

        Ok(())
    }

    fn set_root(&mut self) -> Result<()> {
        let mut root = PathBuf::new();

        if Self::has_config_file(&self.cwd) {
            self.root = Some(self.cwd.clone());
            return Ok(());
        }

        for anc in self.cwd.ancestors() {
            if Self::is_checkout_root(&anc) {
                root.push(anc);
                self.root = Some(root);
                return Ok(());
            }
        }

        Err(PreciousError::CannotFindRoot {
            cwd: self.cwd.to_string_lossy().to_string(),
        }
        .into())
    }

    pub fn run(&mut self) -> i8 {
        match self.run_subcommand() {
            Ok(e) => {
                if let Some(err) = e.error {
                    print!("{}", err);
                }
                if let Some(msg) = e.message {
                    println!("{} {}", self.chars.empty, msg);
                }
                e.status
            }
            Err(e) => {
                error!("Failed to run precious: {}", e);
                1
            }
        }
    }

    fn run_subcommand(&mut self) -> Result<Exit> {
        if self.matches.subcommand_matches("tidy").is_some() {
            return self.tidy();
        } else if self.matches.subcommand_matches("lint").is_some() {
            return self.lint();
        }

        Ok(Exit {
            status: 1,
            message: None,
            error: Some(String::from(
                "You must run either the tidy or lint subcommand",
            )),
        })
    }

    fn tidy(&mut self) -> Result<Exit> {
        println!("{} Tidying {}", self.chars.ring, self.mode());

        let tidiers = self.config().tidy_filters(self.root_dir().as_path())?;
        self.run_all_filters("tidying", tidiers, |s, t| s.run_one_tidier(t))
    }

    fn lint(&mut self) -> Result<Exit> {
        println!("{} Linting {}", self.chars.ring, self.mode());

        let linters = self.config().lint_filters(self.root_dir().as_path())?;
        self.run_all_filters("linting", linters, |s, l| s.run_one_linter(l))
    }

    fn run_all_filters<R>(
        &mut self,
        action: &str,
        filters: Vec<filter::Filter>,
        run_filter: R,
    ) -> Result<Exit>
    where
        R: Fn(&mut Self, &filter::Filter) -> Result<Option<Vec<ActionError>>>,
    {
        if filters.is_empty() {
            return Err(PreciousError::NoFilters {
                what: action.into(),
            }
            .into());
        }

        if self.basepaths()?.paths()?.is_none() {
            return Ok(self.no_files_exit());
        }

        let mut all_errors: Vec<ActionError> = vec![];
        for f in filters {
            if let Some(mut errors) = run_filter(self, &f)? {
                all_errors.append(&mut errors);
            }
        }

        Ok(self.make_exit(all_errors, action))
    }

    fn run_one_tidier(&mut self, t: &filter::Filter) -> Result<Option<Vec<ActionError>>> {
        let runner = |s: &Self, p: &Path, paths: &basepaths::Paths| -> Option<ActionError> {
            match t.tidy(p, &paths.files) {
                Ok(Some(true)) => {
                    if !s.quiet {
                        println!(
                            "{} Tidied by {}:    {}",
                            s.chars.tidied,
                            t.name,
                            p.to_string_lossy()
                        );
                    }
                    None
                }
                Ok(Some(false)) => {
                    if !s.quiet {
                        println!(
                            "{} Unchanged by {}: {}",
                            s.chars.unchanged,
                            t.name,
                            p.to_string_lossy()
                        );
                    }
                    None
                }
                Ok(None) => None,
                Err(e) => {
                    println!(
                        "{} error {}: {}",
                        s.chars.execution_error,
                        t.name,
                        p.to_string_lossy()
                    );
                    Some(ActionError {
                        error: format!("{:#}", e),
                        config_key: t.config_key(),
                        path: p.to_owned(),
                    })
                }
            }
        };

        self.run_parallel(t, runner)
    }

    fn run_one_linter(&mut self, l: &filter::Filter) -> Result<Option<Vec<ActionError>>> {
        let runner = |s: &Self, p: &Path, paths: &basepaths::Paths| -> Option<ActionError> {
            match l.lint(p, &paths.files) {
                Ok(Some(r)) => {
                    if r.ok {
                        if !s.quiet {
                            println!(
                                "{} Passed {}: {}",
                                s.chars.lint_free,
                                l.name,
                                p.to_string_lossy()
                            );
                        }
                        None
                    } else {
                        println!(
                            "{} Failed {}: {}",
                            s.chars.lint_dirty,
                            l.name,
                            p.to_string_lossy()
                        );
                        if let Some(s) = r.stdout {
                            println!("{}", s);
                        }
                        if let Some(s) = r.stderr {
                            println!("{}", s);
                        }

                        Some(ActionError {
                            error: "linting failed".into(),
                            config_key: l.config_key(),
                            path: p.to_owned(),
                        })
                    }
                }
                Ok(None) => None,
                Err(e) => {
                    println!(
                        "{} error {}: {}",
                        s.chars.execution_error,
                        l.name,
                        p.to_string_lossy()
                    );
                    Some(ActionError {
                        error: format!("{:#}", e),
                        config_key: l.config_key(),
                        path: p.to_owned(),
                    })
                }
            }
        };

        self.run_parallel(l, runner)
    }

    fn run_parallel<R>(&mut self, f: &filter::Filter, runner: R) -> Result<Option<Vec<ActionError>>>
    where
        R: Fn(&Self, &Path, &basepaths::Paths) -> Option<ActionError> + Sync,
    {
        let map = self.path_map(f)?;

        let mut e: Vec<ActionError> = vec![];
        self.thread_pool.install(|| {
            e.append(
                &mut map
                    .par_iter()
                    .filter_map(|(p, paths)| runner(self, p, paths))
                    .collect::<Vec<ActionError>>(),
            );
        });

        if e.is_empty() {
            Ok(None)
        } else {
            Ok(Some(e))
        }
    }

    fn no_files_exit(&self) -> Exit {
        Exit {
            status: 0,
            message: Some(String::from("No files found")),
            error: None,
        }
    }

    fn make_exit(&self, errors: Vec<ActionError>, action: &str) -> Exit {
        let (status, error) = if errors.is_empty() {
            (0, None)
        } else {
            let red = format!("\x1B[{}m", Color::Red.to_fg_str());
            let ansi_off = "\x1B[0m";
            let plural = if errors.len() > 1 { 's' } else { '\0' };

            let error = format!(
                "{}Error{} when {} files:{}\n{}",
                red,
                plural,
                action,
                ansi_off,
                errors
                    .iter()
                    .map(|ae| format!(
                        "  {} {} [{}]\n    {}\n",
                        self.chars.bullet,
                        ae.path.to_string_lossy(),
                        ae.config_key,
                        ae.error,
                    ))
                    .collect::<Vec<String>>()
                    .join("")
            );
            (1, Some(error))
        };
        Exit {
            status,
            message: None,
            error,
        }
    }

    fn path_map(&mut self, f: &filter::Filter) -> Result<HashMap<PathBuf, basepaths::Paths>> {
        if f.run_mode_is(filter::RunMode::Root) {
            return self.root_as_paths();
        } else if f.run_mode_is(filter::RunMode::Dirs) {
            return self.dirs();
        }
        self.files()
    }

    fn root_as_paths(&mut self) -> Result<HashMap<PathBuf, basepaths::Paths>> {
        let mut root_map = HashMap::new();
        let paths = self.basepaths()?.paths()?;

        let mut all: Vec<PathBuf> = vec![];
        for p in paths.unwrap().iter_mut() {
            all.append(&mut p.files);
        }

        let root_paths = basepaths::Paths {
            dir: PathBuf::from("."),
            files: all,
        };
        root_map.insert(PathBuf::from("."), root_paths);
        Ok(root_map)
    }

    fn dirs(&mut self) -> Result<HashMap<PathBuf, basepaths::Paths>> {
        let mut map = HashMap::new();
        let paths = self.basepaths()?.paths()?;

        for p in paths.unwrap() {
            map.insert(p.dir.clone(), p);
        }
        Ok(map)
    }

    fn files(&mut self) -> Result<HashMap<PathBuf, basepaths::Paths>> {
        let mut map = HashMap::new();
        let paths = self.basepaths()?.paths()?;

        for p in paths.unwrap() {
            for f in p.files.iter() {
                map.insert(f.clone(), p.clone());
            }
        }
        Ok(map)
    }

    fn basepaths(&mut self) -> Result<&mut basepaths::BasePaths> {
        if self.basepaths.is_none() {
            let (mode, paths) = self.mode_and_paths_from_args();
            self.basepaths = Some(basepaths::BasePaths::new(
                mode,
                paths,
                self.cwd.clone(),
                self.config().exclude.clone(),
            )?);
        }
        Ok(self.basepaths.as_mut().unwrap())
    }

    fn mode(&self) -> basepaths::Mode {
        let (mode, _) = self.mode_and_paths_from_args();
        mode
    }

    fn mode_and_paths_from_args(&self) -> (basepaths::Mode, Vec<PathBuf>) {
        let subc_matches = self.matched_subcommand();

        let mut paths: Vec<PathBuf> = vec![];
        if subc_matches.is_present("all") {
            return (basepaths::Mode::All, paths);
        } else if subc_matches.is_present("git") {
            return (basepaths::Mode::GitModified, paths);
        } else if subc_matches.is_present("staged") {
            return (basepaths::Mode::GitStaged, paths);
        }

        if !subc_matches.is_present("paths") {
            panic!("No mode or paths were provided but clap did not return an error");
        }
        subc_matches.values_of("paths").unwrap().for_each(|p| {
            paths.push(PathBuf::from(p));
        });
        (basepaths::Mode::FromCli, paths)
    }

    fn matched_subcommand(&self) -> &ArgMatches<'a> {
        match self.matches.subcommand() {
            ("tidy", Some(m)) => m,
            ("lint", Some(m)) => m,
            _ => panic!("Somehow none of our subcommands matched and clap did not return an error"),
        }
    }

    fn is_checkout_root(path: &Path) -> bool {
        for dir in vcs::dirs() {
            let mut poss = PathBuf::from(path);
            poss.push(dir);
            if poss.exists() {
                return true;
            }
        }
        false
    }

    fn has_config_file(path: &Path) -> bool {
        let mut file = path.to_path_buf();
        file.push("precious.toml");
        file.exists()
    }

    fn default_config_file(&self) -> PathBuf {
        let mut file = self.root_dir();
        file.push("precious.toml");
        file
    }

    fn root_dir(&self) -> PathBuf {
        self.root.as_ref().unwrap().clone()
    }

    fn config(&self) -> &config::Config {
        self.config.as_ref().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testhelper;
    // Anything that does pushd must be run serially or else chaos ensues.
    use serial_test::serial;
    use spectral::prelude::*;

    const SIMPLE_CONFIG: &str = r#"
[commands.rustfmt]
type    = "both"
include = "**/*.rs"
cmd     = ["rustfmt"]
lint_flags = "--check"
ok_exit_codes = [0]
lint_failure_exit_codes = [1]
"#;

    #[test]
    #[serial]
    fn new() -> Result<()> {
        let helper = testhelper::TestHelper::new()?.with_config_file(SIMPLE_CONFIG)?;
        let _pushd = helper.pushd_to_root()?;

        let app = app();
        let matches = app.get_matches_from_safe(&["precious", "tidy", "--all"])?;

        let p = Precious::new(&matches)?;
        assert_that(&p.chars).is_equal_to(chars::FUN_CHARS);
        let mut expect_config_file = p.root_dir();
        expect_config_file.push("precious.toml");
        assert_that(&p.config_file.unwrap()).is_equal_to(expect_config_file);
        assert_that(&p.quiet).is_equal_to(false);

        Ok(())
    }

    #[test]
    #[serial]
    fn new_with_ascii_flag() -> Result<()> {
        let helper = testhelper::TestHelper::new()?.with_config_file(SIMPLE_CONFIG)?;
        let _pushd = helper.pushd_to_root()?;

        let app = app();
        let matches = app.get_matches_from_safe(&["precious", "--ascii", "tidy", "--all"])?;

        let p = Precious::new(&matches)?;
        assert_that(&p.chars).is_equal_to(chars::BORING_CHARS);

        Ok(())
    }

    #[test]
    #[serial]
    fn new_with_config_path() -> Result<()> {
        let helper = testhelper::TestHelper::new()?.with_config_file(SIMPLE_CONFIG)?;
        let _pushd = helper.pushd_to_root()?;

        let app = app();
        let matches = app.get_matches_from_safe(&[
            "precious",
            "--config",
            helper.config_file().to_str().unwrap(),
            "tidy",
            "--all",
        ])?;

        let p = Precious::new(&matches)?;
        assert_that(&p.config_file.unwrap()).is_equal_to(helper.config_file());

        Ok(())
    }

    #[test]
    #[serial]
    fn test_set_root_prefers_config_file() -> Result<()> {
        let helper = testhelper::TestHelper::new()?.with_git_repo()?;

        let mut src_dir = helper.root();
        src_dir.push("src");
        let mut subdir_config = src_dir.clone();
        subdir_config.push("precious.toml");
        helper.write_file(&subdir_config, SIMPLE_CONFIG)?;
        let _pushd = testhelper::Pushd::new(src_dir.clone())?;

        let app = app();
        let matches = app.get_matches_from_safe(&["precious", "--quiet", "tidy", "--all"])?;

        let p = Precious::new(&matches)?;
        assert_that(&p.root_dir()).is_equal_to(src_dir);

        Ok(())
    }

    #[test]
    #[serial]
    fn test_basepaths_uses_cwd() -> Result<()> {
        let helper = testhelper::TestHelper::new()?
            .with_config_file(SIMPLE_CONFIG)?
            .with_git_repo()?;

        let mut src_dir = helper.root();
        src_dir.push("src");
        let _pushd = testhelper::Pushd::new(src_dir)?;

        let app = app();
        let matches = app.get_matches_from_safe(&["precious", "--quiet", "tidy", "--all"])?;

        let mut p = Precious::new(&matches)?;
        let paths = p.basepaths()?;

        let expect = vec![basepaths::Paths {
            dir: PathBuf::from("."),
            files: ["bar.rs", "can_ignore.rs", "main.rs", "module.rs"]
                .iter()
                .map(PathBuf::from)
                .collect(),
        }];
        assert_that(&paths.paths()?).is_equal_to(Some(expect));

        Ok(())
    }

    #[test]
    #[serial]
    fn test_tidy_succeeds() -> Result<()> {
        let config = r#"
[commands.true]
type    = "tidy"
include = "**/*"
cmd     = ["true"]
ok_exit_codes = [0]
"#;
        let helper = testhelper::TestHelper::new()?.with_config_file(config)?;
        let _pushd = helper.pushd_to_root()?;

        let app = app();
        let matches = app.get_matches_from_safe(&["precious", "--quiet", "tidy", "--all"])?;

        let mut p = Precious::new(&matches)?;
        let status = p.run();

        assert_that(&status).is_equal_to(0);

        Ok(())
    }

    #[test]
    #[serial]
    fn test_tidy_fails() -> Result<()> {
        let config = r#"
[commands.false]
type    = "tidy"
include = "**/*"
cmd     = ["false"]
ok_exit_codes = [0]
"#;
        let helper = testhelper::TestHelper::new()?.with_config_file(config)?;
        let _pushd = helper.pushd_to_root()?;

        let app = app();
        let matches = app.get_matches_from_safe(&["precious", "--quiet", "tidy", "--all"])?;

        let mut p = Precious::new(&matches)?;
        let status = p.run();

        assert_that(&status).is_equal_to(1);

        Ok(())
    }

    #[test]
    #[serial]
    fn test_lint_succeeds() -> Result<()> {
        let config = r#"
[commands.true]
type    = "lint"
include = "**/*"
cmd     = ["true"]
ok_exit_codes = [0]
lint_failure_exit_codes = [1]
"#;
        let helper = testhelper::TestHelper::new()?.with_config_file(config)?;
        let _pushd = helper.pushd_to_root()?;

        let app = app();
        let matches = app.get_matches_from_safe(&["precious", "--quiet", "lint", "--all"])?;

        let mut p = Precious::new(&matches)?;
        let status = p.run();

        assert_that(&status).is_equal_to(0);

        Ok(())
    }

    #[test]
    #[serial]
    fn test_lint_fails() -> Result<()> {
        let config = r#"
[commands.false]
type    = "lint"
include = "**/*"
cmd     = ["false"]
ok_exit_codes = [0]
lint_failure_exit_codes = [1]
"#;
        let helper = testhelper::TestHelper::new()?.with_config_file(config)?;
        let _pushd = helper.pushd_to_root()?;

        let app = app();
        let matches = app.get_matches_from_safe(&["precious", "--quiet", "lint", "--all"])?;

        let mut p = Precious::new(&matches)?;
        let status = p.run();

        assert_that(&status).is_equal_to(1);

        Ok(())
    }
}
