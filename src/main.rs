use std::borrow::Cow;
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::*;
use boolinator::Boolinator;
use clap::{App, Arg};
use colored::*;
use once_cell::sync::{Lazy, OnceCell};
use serde::{Deserialize, Serialize};
use tempdir::TempDir;
use walkdir::WalkDir;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Settings {
    language: String,
    compilers: Vec<String>,
    compiler_options: Vec<String>,
    dogear: String,
}

#[derive(Clone, Debug)]
struct TestCase {
    path: String,
    header: String,
    start: usize,
    end: usize,
    code: String,
}

fn read_the_docs(path: impl Into<PathBuf>, language: &str, dogear: &str) -> Result<Vec<TestCase>> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let path = &path.into();
    let mut header: [String; 4] = ["".into(), "".into(), "".into(), "".into()];
    let mut codes = Vec::new();
    let mut buffer = Vec::new();
    let mut line_start = 0usize;
    let mut in_code_block = false;
    let mut in_test_code_block = false;
    let mut is_specified_lang = false;
    let lang_code = format!(r#"```{}"#, language);

    for (num, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.starts_with(r#"```"#) {
            if in_code_block {
                if in_test_code_block {
                    codes.push(TestCase {
                        path: path.to_string_lossy().to_string(),
                        header: format!(
                            "{:?}",
                            header
                                .iter()
                                .filter(|h| !h.is_empty())
                                .map(|h| h.trim_matches(' '))
                                .collect::<Vec<_>>()
                        ),
                        start: line_start,
                        end: num,
                        code: buffer.join("\n"),
                    });
                    buffer.clear();
                }
                is_specified_lang = false;
                in_test_code_block = false;
            } else {
                is_specified_lang = line.starts_with(&lang_code);
                line_start = num;
            }
            in_code_block = !in_code_block;
        } else {
            // read a header if starts with `#`
            if !in_code_block && line.starts_with('#') {
                let len = line.len();
                let title = line.trim_start_matches('#').to_string();
                header[len - title.len() - 1] = title.to_string();
            }
            if in_code_block {
                if !in_test_code_block {
                    in_test_code_block = is_specified_lang && line == dogear;
                } else {
                    buffer.push(line.to_string());
                }
            }
        }
    }
    Ok(codes)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Report<'a> {
    filename: Cow<'a, str>,
    line: [usize; 2],
    compiler: Cow<'a, str>,
    info: Cow<'a, str>,
}

impl<'a> Report<'a> {
    fn from(case: &'a TestCase, compiler: impl Into<Cow<'a, str>>) -> Report<'a> {
        Report {
            filename: case.path.clone().into(),
            line: [case.start, case.end],
            compiler: compiler.into(),
            info: "".into(),
        }
    }
    fn with_info(self, info: impl Into<Cow<'a, str>>) -> Report<'a> {
        Report {
            filename: self.filename.to_owned(),
            line: self.line.to_owned(),
            compiler: self.compiler.to_owned(),
            info: info.into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Reports<'a>(Vec<Report<'a>>);

type TestResult<'a> = Result<String, Report<'a>>;

async fn run_tests<'a>(
    test_case: &'a TestCase,
    settings: &'static Settings,
    workspace: &'static TempDir,
    counter: usize,
) -> Vec<impl Future<Output = anyhow::Result<TestResult<'a>>> + 'a> {
    use std::time::Instant;
    use tokio::process::Command;

    settings
        .compilers
        .iter()
        .map(move |compiler| async move {
            let start = Instant::now();
            let exe = format!(
                "{}-{}.out",
                counter,
                std::path::Path::new(compiler)
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
            );
            let exe = workspace.path().join(exe);
            // piped echo
            let echo = std::process::Command::new("echo")
                .arg(&test_case.code)
                .stdout(Stdio::piped())
                .spawn()
                .expect("piped echo failed.");
            // compiles a test
            let compile_output = Command::new(compiler)
                .args(settings.compiler_options.clone())
                .args(&["-o", &exe.to_string_lossy()])
                .arg("-xc++")
                .arg("-")
                .stdin(echo.stdout.unwrap())
                .output()
                .await // compile
                .with_context(|| anyhow!("failed to execute compile process"))?;
            if compile_output.status.success() {
                let test_output = Command::new(&exe)
                    .output()
                    .await
                    .with_context(|| anyhow!("failed to execute test {}", exe.to_string_lossy()))?;
                Ok(test_output.status.success().as_result_from(
                    move || {
                        format! {
                            "Passed: {file} ({header} [line: {begin}-{end}], time: {elapsed} ms)",
                            file = test_case.path,
                            header = test_case.header,
                            begin = test_case.start,
                            end = test_case.end,
                            elapsed = start.elapsed().subsec_millis(),
                        }
                    },
                    move || {
                        Report::from(test_case, compiler)
                            .with_info(String::from_utf8(test_output.stderr).unwrap())
                    },
                ))
            } else {
                Ok(Err(Report::from(test_case, compiler).with_info(
                    String::from_utf8(compile_output.stderr).unwrap(),
                )))
            }
        })
        .collect::<Vec<_>>()
}

/// Returns clap app
///
/// # App config
///
fn create_my_app() -> clap::App<'static, 'static> {
    App::new("mkdocs-smoke-test")
        .version("0.2.0")
        .author("mitama <loligothick@gmail.com>")
        .about("Smoke test tool for MkDocs")
        .arg(
            Arg::with_name("directory")
                .short("d")
                .long("directory")
                .value_name("DIR")
                .help("Sets the path to docs directory")
                .takes_value(true)
                .required(true),
        )
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .value_name("CONFIG")
                .help("Sets the path to config.toml")
                .takes_value(true)
                .required(true),
        )
}

static INSTANCE: OnceCell<Settings> = OnceCell::new();
static WORKSPACE: Lazy<TempDir> =
    Lazy::new(|| TempDir::new("workspace").expect("failed to create workspace directory"));

impl Settings {
    pub fn global() -> &'static Settings {
        INSTANCE.get().expect("settings is not initialized")
    }

    fn init_from(path: impl Into<PathBuf>) -> Result<Settings> {
        let settings: Settings = toml::from_str(&std::fs::read_to_string(path.into())?)
            .with_context(|| anyhow!("ERROR: fail to parse config.toml"))?;
        Ok(settings)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Create App
    let matches = create_my_app().get_matches();

    // Check Args
    let config = matches.value_of("config").unwrap();
    let directory = matches.value_of("directory").unwrap();
    {
        // glob
        let settings = Settings::init_from(config)?;
        INSTANCE.set(settings).unwrap();
    }
    let mut reports = Vec::new();

    for entry in WalkDir::new(directory)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.into_path();
        if path.to_string_lossy().ends_with(".md") {
            let cases = read_the_docs(
                &path,
                &Settings::global().language,
                &Settings::global().dogear,
            )
            .with_context(|| anyhow!("ERROR: fail to read the docs"))?;

            let job_queue = cases
                .into_iter()
                .enumerate()
                .map(|(counter, code)| {
                    tokio::spawn(async move {
                        let mut reports = Vec::new();
                        for job in run_tests(&code, Settings::global(), &WORKSPACE, counter).await {
                            match job.await? {
                                Err(report) => {
                                    let err = serde_json::to_string(&report).unwrap();
                                    println!("{}", err.red());
                                    reports.push(err);
                                }
                                Ok(res) => {
                                    println!("{}", res);
                                }
                            }
                        }
                        anyhow::Result::<Option<Vec<String>>>::Ok(
                            (!reports.is_empty()).as_some(reports.clone()),
                        )
                    })
                })
                .collect::<Vec<_>>();
            for job in job_queue {
                if let Some(errors) = job.await?? {
                    reports.extend(errors);
                }
            }
        }
    }
    if !reports.is_empty() {
        let reports = reports
            .into_iter()
            .map(|report| serde_json::from_str(&report).unwrap())
            .collect::<Vec<_>>();
        anyhow::bail!(
            "{}",
            serde_json::to_string_pretty(&Reports(reports))
                .unwrap()
                .red()
        );
    }

    println!("{}", "All Tests Passed".green());
    std::fs::remove_dir_all(&*WORKSPACE)?;
    Ok(())
}
