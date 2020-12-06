use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::*;
use boolinator::Boolinator;
use clap::{App, Arg};
use walkdir::WalkDir;

#[derive(Clone, Debug)]
struct TestCode {
    path: String,
    header: Vec<String>,
    start: usize,
    end: usize,
    code: String,
}

fn read_the_docs(path: impl Into<PathBuf>, keys: (&str, &str)) -> Result<Vec<TestCode>> {
    let path = &path.into();
    let (begin, end) = keys;
    let mut header: [String; 4] = ["".into(), "".into(), "".into(), "".into()];
    let mut codes = Vec::new();
    let mut buffer = Vec::new();
    let mut in_code = false;
    let mut line_start = 0usize;
    let mut in_code_block = false;

    for (num, res) in BufReader::new(File::open(path)?).lines().enumerate() {
        if let Ok(line) = res {
            if line.starts_with(r#"```"#) {
                in_code_block = !in_code_block;
            }
            if !in_code_block && line.starts_with('#') {
                let len = line.len();
                let title = line.trim_start_matches('#').to_string();
                header[len - title.len() - 1] = title.to_string();
            }
            if line == end {
                in_code = false;
                codes.push(TestCode {
                    path: path.to_string_lossy().to_string(),
                    header: header.iter().cloned().map(String::from).collect(),
                    start: line_start,
                    end: num,
                    code: buffer.join("\n"),
                });
                buffer.clear();
                continue;
            }
            if in_code {
                buffer.push(line.to_string());
            }
            if line == begin {
                in_code = true;
                line_start = num;
            }
        }
    }
    Ok(codes)
}

fn main() -> Result<()> {
    let matches = App::new("mkdocs-smoke-test")
        .version("1.0")
        .author("mitama <loligothick@gmail.com>")
        .about("Smoke test tool for MkDocs")
        .arg(
            Arg::with_name("directory")
                .short("d")
                .long("directory")
                .value_name("PATH")
                .help("Sets a doc directory path")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("language")
                .short("l")
                .long("language")
                .value_name("LANG")
                .help("Sets a programming language to test")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("key")
                .short("k")
                .long("key")
                .value_name("KEY")
                .help("Sets a begin/end key words to search")
                .takes_value(true),
        )
        .get_matches();

    let directory = matches
        .value_of("directory")
        .with_context(|| "no directory specified")?;
    let language = matches
        .value_of("language")
        .with_context(|| "no language specified")?;
    let keys = {
        let keys = matches
            .value_of("key")
            .with_context(|| "no keys specified")?
            .split(',')
            .collect::<Vec<_>>();
        (keys.len() == 2)
            .as_some((keys[0], keys[1]))
            .ok_or_else(|| anyhow!("expected [begin,end] key pair"))?
    };

    println!("target: {}, lang: {}, keys={:?}", directory, language, keys);

    for entry in WalkDir::new(directory)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.into_path();

        if path.to_string_lossy().ends_with(".md") {
            let found = read_the_docs(&path, keys).map(|codes| {
                for code in codes.iter() {
                    println!("{:?}", code);
                }
                codes.len()
            })?;
            println!("{} codes found in {}", found, path.to_string_lossy());
        }
    }

    Ok(())
}
