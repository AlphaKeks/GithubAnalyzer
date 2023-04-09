use crossbeam::deque::{Injector, Stealer, Worker};
use reqwest::{
	blocking::{Client, RequestBuilder},
	header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT},
	Url,
};
use serde_json::Value;
use std::io;
use std::{error::Error, io::Write, process::Command};
use std::{
	fs,
	sync::{Arc, Mutex},
	thread,
};
use tempfile::TempDir;

struct Job {
	url: String,
	name: String,
}

fn get_repo_page(token: &str, username: &str, page: u16) -> Result<RequestBuilder, Box<dyn Error>> {
	let api_url = Url::parse(&format!(
		"https://api.github.com/users/{username}/repos?per_page=20&page={}",
		page
	))?;

	let mut headers = HeaderMap::new();
	headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.github+json"));
	headers.insert("X-GitHub-Api-Version", HeaderValue::from_static("2022-11-28"));
	headers.insert(USER_AGENT, HeaderValue::from_static("KauMah"));

	return Ok(Client::new()
		.get(api_url)
		.headers(headers)
		.bearer_auth(token.trim_end()));
}

fn get_user_identifiers(token: &str, username: &str) -> Result<Vec<String>, Box<dyn Error>> {
	let mut out: Vec<String> = Vec::new();
	let api_url = format!("https://api.github.com/users/{username}");
	let mut headers = HeaderMap::new();
	headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.github+json"));
	headers.insert("X-GitHub-Api-Version", HeaderValue::from_static("2022-11-28"));
	headers.insert(USER_AGENT, HeaderValue::from_static("KauMah"));
	let res = Client::new()
		.get(&api_url)
		.headers(headers)
		.bearer_auth(token.trim_end())
		.send()
		.expect("Request to /users/<username> failed")
		.text()
		.expect("Conversion to text failed for user");

	let val: Value = serde_json::from_str(&res)
		.expect("Failed to parse JSON from response for GET users/{username}");
	// let pretty = serde_json::to_string_pretty(&val).expect("pls");
	// println!("{}", pretty);
	let email = val
		.get("email")
		.expect("Failed to parse string <email> from JSON")
		.to_string();
	let name = val
		.get("name")
		.expect("Failed to parse string <name> from JSON")
		.to_string();
	if name.ne("null") {
		out.push(name.trim_matches('"').to_owned())
	};
	if email.ne("null") {
		out.push(email.trim_matches('"').to_owned())
	};

	return Ok(out);
}

fn get_git_urls(rb: RequestBuilder) -> Result<Vec<Job>, Box<dyn Error>> {
	let mut urls = Vec::new();
	let res = rb
		.send()
		.expect("request to /{user}/repos failed")
		.text()
		.expect("Conversion to String failed for respositories");
	let json: Vec<Value> = serde_json::from_str(&res).expect("Failed to convert response to JSON");
	for repo in json.iter() {
		let url = repo
			.get("git_url")
			.expect("Failed to get 'git_url' from JSON object")
			.to_string();
		let name = repo
			.get("name")
			.expect("Failed to get name from JSON resoponse")
			.to_string();
		// println!("{}", &url[7..url.len() - 1]);
		urls.push(Job {
			url: url
				.trim_matches('"')
				.replace("git:", "http:")
				.to_owned(),
			name: name.trim_matches('"').to_owned(),
		});
	}

	Ok(urls)
}

fn main() -> Result<(), reqwest::Error> {
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

	let mut git_repo_jobs: Vec<Job> = Vec::new();
	let mut page: u16 = 1;
	loop {
		let req =
			get_repo_page(&token, &username, page).expect("Something went wrong building the URL");
		let new_urls = get_git_urls(req).expect("msg");
		if new_urls.len() == 0 {
			break;
		}
		git_repo_jobs.extend(new_urls);
		page = page + 1;
	}

	// git_urls
	//     .into_iter()
	//     .map(|job| println!("Name: {}\tUrl: {}", job.name, job.url))
	//     .collect::<Vec<_>>();
	let identifiers =
		Arc::new(get_user_identifiers(&token, &username).expect("function get_user_email failed"));
	identifiers
		.iter()
		.for_each(|id| println!("{}", id));
	if identifiers.len() < 1 {
		panic!("No identifiers found for user for parsing repos, consider using option --searchName [<names>...]");
		// TODO: add the ability to use --searchName when running. This is such a later problem
	}

	let output_file = Arc::new(Mutex::new(
		std::fs::File::create(format!("{}.csv", username.trim()))
			.expect("Failed to create output.csv"),
	));
	let dir_path = Arc::new(TempDir::new().expect("Failed to create a temporary directory"));
	let identifier_str = format!("--author={}", identifiers.join("|"));
	// --------------------------------------------------------------------------------------------- Start Job

	let job_queue: Arc<Injector<Job>> = Arc::new(Injector::new());
	for repo in git_repo_jobs {
		job_queue.push(repo);
	}
	let stealers: Arc<Mutex<Vec<Stealer<Job>>>> = Arc::new(Mutex::new(Vec::new()));
	let mut handles = Vec::new();
	for i in 0..4 {
		let job_queue = job_queue.clone();
		let stealers = stealers.clone();
		let dir_path = dir_path.clone();
		let identifier_str = identifier_str.clone();
		let output_file = output_file.clone();
		let handle = thread::spawn(move || {
			let worker: Worker<Job> = Worker::new_fifo();
			{
				stealers
					.lock()
					.unwrap()
					.push(worker.stealer());
			}
			let _ = job_queue.steal_batch(&worker);
			loop {
				if !worker.is_empty() {
					let job = worker.pop().unwrap();
					let path = dir_path.path().join(job.name.clone());

					let clone = Command::new("git")
						.args(["clone", job.url.as_str()])
						.current_dir(dir_path.path().to_str().unwrap())
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
						.current_dir(path.clone().to_str().unwrap())
						.output()
						.expect("Failed to show git log");

					let output = String::from_utf8_lossy(&output.stdout);
					let lines = output.lines();
					let mut num_files = 0;
					let mut num_lines_added = 0;
					let mut num_lines_deleted = 0;
					let mut timestamp: &str = "";
					let mut hash: &str = "";
					for line in lines {
						let words = line
							.split_whitespace()
							.collect::<Vec<_>>();
						let words = words
							.into_iter()
							.map(|st| st.trim_matches('"'))
							.collect::<Vec<_>>();
						let first = words.get(0);
						match first {
							Some(term) => {
								if term.len() == 40 {
									hash = term;
									timestamp = words.get(1).unwrap();
								// println!("Hash: {}, ts: {}", hash, timestamp);
								} else {
									num_files += 1;
									num_lines_added += match term.parse::<u32>() {
										Ok(val) => val,
										Err(_) => 0,
									};
									num_lines_deleted += match words.get(1).unwrap().parse::<u32>()
									{
										Ok(val) => val,
										Err(_) => 0,
									}
								}
							}
							None => {
								// println!(
								//     "Files: {}, added: {}, removed: {}",
								//     num_files, num_lines_added, num_lines_deleted
								// );
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
					let _del = Command::new("rm").args(["-rf", path.to_str().unwrap()]);
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
				} else if !job_queue.is_empty() {
					let _ = job_queue.steal_batch(&worker);
					continue;
				} else {
					let mut has_stolen = false;
					for stealer in stealers
						.lock()
						.unwrap()
						.to_owned()
						.into_iter()
					{
						if !stealer.is_empty() {
							let _ = stealer.steal_batch(&worker);
							has_stolen = true;
							break;
						}
					}
					if !has_stolen {
						break;
					}
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
