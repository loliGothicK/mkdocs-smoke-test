use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::*;
use boolinator::Boolinator;
use clap::{App, Arg};
use itertools::Itertools;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
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
    let path = &path.into();
    let mut header: [String; 4] = ["".into(), "".into(), "".into(), "".into()];
    let mut codes = Vec::new();
    let mut buffer = Vec::new();
    let mut line_start = 0usize;
    let mut in_code_block = false;
    let mut in_test_code_block = false;
    let mut is_specified_lang = false;
    let lang_code = format!(r#"```{}"#, language);

    for (num, res) in BufReader::new(File::open(path)?).lines().enumerate() {
        match res {
            Ok(line) => {
                if line.starts_with(r#"```"#) {
                    if in_code_block {
                        if in_test_code_block {
                            codes.push(TestCase {
                                path: path.to_string_lossy().to_string(),
                                header: header.iter().filter(|h| !h.is_empty()).join("#"),
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
            Err(err) => {
                return Err(err).with_context(|| "ERROR: fail to read file");
            }
        }
    }
    Ok(codes)
}

#[derive(Debug)]
struct Report {
    test_case: TestCase,
    compiler: String,
    info: String,
}

type TestResult = Result<String, Report>;

async fn run_tests(test_case: TestCase, settings: &Settings) -> Vec<TestResult> {
    use std::process::Command;
    settings
        .compilers
        .iter()
        .map(|compiler| {
            let echo = Command::new("echo")
                .arg(&test_case.code)
                .stdout(Stdio::piped())
                .spawn();
            Command::new(compiler)
                .args(settings.compiler_options.clone())
                .arg("-xc++")
                .arg("-")
                .stdin(echo.unwrap().stdout.unwrap())
                .output()
                .map_err(|err| Report {
                    test_case: test_case.clone(),
                    compiler: compiler.clone(),
                    info: err.to_string(),
                })
                .and_then(|output| {
                    output
                        .status
                        .success()
                        .as_result_from(
                            || {
                                Command::new("./a.out").output().map_err(|err| Report {
                                    test_case: test_case.clone(),
                                    compiler: compiler.clone(),
                                    info: err.to_string(),
                                })
                            },
                            || Report {
                                test_case: test_case.clone(),
                                compiler: compiler.clone(),
                                info: String::from_utf8(output.stderr).unwrap(),
                            },
                        )
                        .and_then(std::convert::identity)
                })
                .map(|_| {
                    format! {
                        "Passed: {file} ({header} [line: {begin}-{end}])",
                        file = test_case.path,
                        header = test_case.header,
                        begin = test_case.start,
                        end = test_case.end,
                    }
                })
        })
        .collect()
}

/// Returns clap app
///
/// # App config
///
fn create_my_app() -> clap::App<'static, 'static> {
    App::new("mkdocs-smoke-test")
        .version("1.0")
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

#[async_std::main]
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
    for entry in WalkDir::new(directory)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.into_path();

        if path.to_string_lossy().ends_with(".md") {
            let jobs = read_the_docs(
                &path,
                &Settings::global().language,
                &Settings::global().dogear,
            )
                .map(|codes| {
                    codes.into_iter().map(|code| {
                        std::thread::spawn(move || async move {
                            let res = run_tests(code, Settings::global());
                            res.await
                        })
                    })
                })
                .with_context(|| anyhow!("ERROR: fail to read the docs"))?
                .collect::<Vec<_>>();

            for job in jobs {
                for res in job.join().unwrap().await {
                    match res {
                        Ok(msg) => {
                            println!("{}", msg);
                        }
                        Err(report) => {
                            println! {
                                "ERROR: {file} line {start}-{end}, compiler = {cxx}:\n\t{info}",
                                file = report.test_case.path,
                                start = report.test_case.start,
                                end = report.test_case.end,
                                cxx = report.compiler,
                                info = report.info,
                            };
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
