use crossbeam::deque::{Injector, Stealer, Worker};
use reqwest::{
    blocking::{Client, RequestBuilder},
    header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT},
    Url,
};

use std::io;
use std::{error::Error, io::Write, process::Command};
use std::{
    fs,
    sync::{Arc, Mutex},
    thread,
};
use tempfile::TempDir;

fn get_repo_page(token: &str, username: &str, page: u16) -> Result<RequestBuilder, Box<dyn Error>> {
    let api_url = Url::parse(&format!(
        "https://api.github.com/users/{username}/repos?per_page=20&page={}",
        page
    ))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static("2022-11-28"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static("KauMah"));

    return Ok(Client::new()
        .get(api_url)
        .headers(headers)
        .bearer_auth(token.trim_end()));
}

// Use a struct for deserializing rather than `serde_json::Value`
#[derive(Debug, serde::Deserialize)]
struct User {
    email: String,
    name: String,
}

// 1. Don't return a `Vec` with 2 elements. Use a tuple instead: `-> Result<(String, String)>`
// 2. Even better: return a struct with named fields if you're deserializing json.
// 3. Make the function fail if GitHub's response was incomplete/incorrect/empty/whatever instead of
//    returning an empty Vec.
// 4. You can deserialize the response directly with `.json()`
fn get_user(token: &str, username: &str) -> Result<User, Box<dyn Error>> {
    // not _that_ important if your URL is essentially static, but good practice to validate as much
    // as possible
    let api_url = Url::parse(&format!("https://api.github.com/users/{username}"))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static("2022-11-28"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static("KauMah"));

    Ok(Client::new()
        .get(api_url)
        .headers(headers)
        .bearer_auth(token.trim_end())
        .send()?
        .json()?)
}

#[derive(Debug, serde::Deserialize)]
struct Job {
    git_url: String,
    name: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("Initializing - Github Analyzer...");
    let token = fs::read_to_string("./token").expect("Could not read token form ./token");

    let mut username = String::new();
    println!("Enter a Github Username:\n");
    io::stdin()
        .read_line(&mut username)
        .expect("Something went wrong reading username from stdin");

    // clear terminal
    println!("{}[2J", 27 as char);
    print!("{esc}[2J{esc}[1;1H", esc = 27 as char);
    // end clear terminal

    let mut git_repo_jobs = Vec::new();

    for page in 1.. {
        let page = get_repo_page(&token, &username, page)?;
        let jobs = page.send()?.json::<Vec<Job>>()?;

        if jobs.is_empty() {
            break;
        }

        git_repo_jobs.extend(jobs);
    }

    let user = get_user(&token, &username).expect("No identifiers found for user for parsing repos, consider using option --searchName [<names>...]");

    let user = Arc::new(user);

    let output_file = Arc::new(Mutex::new(std::fs::File::create(format!(
        "{}.csv",
        username.trim()
    ))?));

    let dir_path = Arc::new(TempDir::new()?);

    let identifier_str = format!("--author={}|{}", user.name, user.email);

    // --------------------------------------------------------------------------------------------- Start Job

    let job_queue: Arc<Injector<Job>> = Arc::default();
    for repo in git_repo_jobs {
        job_queue.push(repo);
    }

    let stealers: Arc<Mutex<Vec<Stealer<Job>>>> = Arc::default();

    let mut handles = Vec::new();

    for _ in 0..4 {
        // It's convention that you use fully qualified syntax when cloning an `Arc` or similar
        // containers so it's obvious that you're only doing a shallow clone. This also avoids
        // potentially conflicting `Clone` implementations.
        let job_queue = Arc::clone(&job_queue);
        let stealers = Arc::clone(&stealers);
        let dir_path = Arc::clone(&dir_path);
        let identifier_str = identifier_str.clone();
        let output_file = Arc::clone(&output_file);

        let handle = thread::spawn(move || {
            let worker: Worker<Job> = Worker::new_fifo();

            {
                stealers.lock().unwrap().push(worker.stealer());
            }

            _ = job_queue.steal_batch(&worker);

            loop {
                if !job_queue.is_empty() {
                    _ = job_queue.steal_batch(&worker);
                    continue;
                }

                while let Some(job) = worker.pop() {
                    let path = dir_path.path().join(job.name.clone());

                    // Clone repo
                    Command::new("git")
                        .args(["clone", &job.git_url])
                        .current_dir(&*dir_path)
                        .status()
                        .expect("Failed to clone repo");

                    let output = Command::new("git")
                        .args([
                            "--no-pager",
                            "log",
                            "--pretty=format:\"%H%x09%ad%x09\"",
                            "--perl-regexp",
                            "--invert-grep",
                            identifier_str.as_str(),
                            "--no-merges",
                            "--date=unix",
                            "--numstat",
                        ])
                        .current_dir(&*path)
                        .output()
                        .expect("Failed to show git log");

                    let output = String::from_utf8_lossy(&output.stdout);

                    let mut num_files = 0;
                    let mut num_lines_added = 0;
                    let mut num_lines_deleted = 0;
                    let mut timestamp: &str = "";
                    let mut hash: &str = "";

                    for line in output.lines() {
                        let words = line
                            .split_whitespace()
                            .map(|st| st.trim_matches('"'))
                            .collect::<Vec<_>>();

                        match words.get(0) {
                            Some(term) if term.len() == 40 => {
                                hash = term;
                                timestamp = words.get(1).expect("Failed to get timestamp.");
                            }
                            Some(term) => {
                                num_files += 1;
                                num_lines_added += term.parse().unwrap_or(0);
                                num_lines_deleted += term.parse().unwrap_or(0);
                            }
                            None => {
                                output_file
                                    .lock()
                                    .unwrap()
                                    .write_all(
                                        format!(
                                            "{},{},{},{},{}\n",
                                            hash,
                                            timestamp,
                                            num_files,
                                            num_lines_added,
                                            num_lines_deleted
                                        )
                                        .as_bytes(),
                                    )
                                    .expect("Failed to write to file");
                                num_files = 0;
                                num_lines_added = 0;
                                num_lines_deleted = 0;
                            }
                        };
                    }

                    Command::new("rm").args(["-rf", path.to_str().unwrap()]);

                    output_file
                        .lock()
                        .unwrap()
                        .write_all(
                            format!(
                                "{},{},{},{},{}\n",
                                hash, timestamp, num_files, num_lines_added, num_lines_deleted
                            )
                            .as_bytes(),
                        )
                        .expect("Failed to write to file");
                }

                let mut has_stolen = false;

                for stealer in stealers.lock().expect("Mutex got poisoned.").iter() {
                    if !stealer.is_empty() {
                        _ = stealer.steal_batch(&worker);
                        has_stolen = true;
                        break;
                    }
                }

                if !has_stolen {
                    break;
                }
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // --------------------------------------------------------------------------------------------- End Job

    Ok(())
}
