use crate::{Database, jira};
use keyring::Entry;
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tauri::{Emitter, State};

const CREDENTIAL_SERVICE: &str = "com.agentcontrolplane.desktop.github-issue";
const MAX_RESPONSE: usize = 256 * 1024;
const MAX_ISSUE_TEXT: usize = 30_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GithubDraft {
    title: String,
    description: String,
    source_url: String,
    source_key: String,
}

#[derive(Debug, Deserialize)]
struct RawIssue {
    number: u64,
    title: String,
    body: Option<String>,
    state: String,
    labels: Vec<IssueLabel>,
    #[serde(default)]
    pull_request: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct IssueLabel {
    name: String,
}

#[derive(Debug)]
struct NormalizedIssue {
    title: String,
    body: String,
    state: String,
    labels: Vec<String>,
}

fn host_settings_key(host: &str) -> String {
    format!("github_issue_host:{host}")
}

fn validate_host(host: &str) -> Result<String, String> {
    let host = host.trim().to_ascii_lowercase();
    let location = parse_issue_url(&format!("https://{host}/owner/repo/issues/1"))?;
    if location.host != host || host.parse::<std::net::IpAddr>().is_ok() {
        return Err("Enter a GitHub hostname without a scheme, port, or path.".to_owned());
    }
    Ok(host)
}

#[tauri::command]
pub(crate) fn configure_github_issue_host(
    host: String,
    method: String,
    token: String,
    database: State<'_, Database>,
) -> Result<(), String> {
    let host = validate_host(&host)?;
    if !matches!(method.as_str(), "cli" | "token") {
        return Err("Choose GitHub CLI or a personal access token.".to_owned());
    }
    if token.len() > 4096 || token.contains(['\r', '\n']) {
        return Err("The GitHub token is invalid.".to_owned());
    }
    if method == "token" || !token.is_empty() {
        let entry = Entry::new(CREDENTIAL_SERVICE, &host)
            .map_err(|_| "Unable to access the OS credential vault.".to_owned())?;
        if !token.is_empty() {
            entry.set_password(&token).map_err(|_| {
                "Unable to save the GitHub token in the OS credential vault.".to_owned()
            })?;
        }
        if method == "token" {
            match entry.get_password() {
                Ok(_) => {}
                Err(keyring::Error::NoEntry) => {
                    return Err("Enter a personal access token for this host.".to_owned());
                }
                Err(_) => return Err("Unable to access the OS credential vault.".to_owned()),
            }
        }
    }
    let connection = database
        .0
        .lock()
        .map_err(|_| "Settings database is unavailable.".to_owned())?;
    connection.execute(
        "INSERT INTO app_settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![host_settings_key(&host), method],
    ).map_err(|_| "Unable to save GitHub host settings.".to_owned())?;
    Ok(())
}

fn auth_method(connection: &rusqlite::Connection, host: &str) -> Result<String, String> {
    let configured = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            [host_settings_key(host)],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|_| "Unable to read GitHub host settings.".to_owned())?;
    match configured.as_deref() {
        Some("cli") => Ok("cli".to_owned()),
        Some("token") => Ok("token".to_owned()),
        Some(_) => Err("Invalid GitHub host authentication configuration.".to_owned()),
        None if host == "github.com" => Ok("cli".to_owned()),
        None => Err(
            "Configure this GitHub Enterprise host in Settings → Integrations before importing."
                .to_owned(),
        ),
    }
}

#[derive(Serialize)]
pub(crate) struct GithubIssueHost {
    host: String,
    method: String,
}

#[tauri::command]
pub(crate) fn list_github_issue_hosts(
    database: State<'_, Database>,
) -> Result<Vec<GithubIssueHost>, String> {
    let connection = database
        .0
        .lock()
        .map_err(|_| "Settings database is unavailable.".to_owned())?;
    let mut statement = connection.prepare("SELECT substr(key, 19), value FROM app_settings WHERE substr(key, 1, 18) = 'github_issue_host:' ORDER BY key")
        .map_err(|_| "Unable to read GitHub host settings.".to_owned())?;
    let mut hosts = statement
        .query_map([], |row| {
            Ok(GithubIssueHost {
                host: row.get(0)?,
                method: row.get(1)?,
            })
        })
        .map_err(|_| "Unable to read GitHub host settings.".to_owned())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Unable to read GitHub host settings.".to_owned())?;
    if !hosts.iter().any(|entry| entry.host == "github.com") {
        hosts.insert(
            0,
            GithubIssueHost {
                host: "github.com".to_owned(),
                method: "cli".to_owned(),
            },
        );
    }
    Ok(hosts)
}

fn normalized_issue(
    raw: &[u8],
    location: &IssueLocation,
    secret: Option<&str>,
) -> Result<NormalizedIssue, String> {
    let issue: RawIssue = serde_json::from_slice(raw)
        .map_err(|_| "GitHub returned an unreadable issue response.".to_owned())?;
    if issue.number != location.number
        || issue.pull_request.is_some()
        || issue.title.trim().is_empty()
    {
        return Err("GitHub did not return the requested issue.".to_owned());
    }
    let result = NormalizedIssue {
        title: issue.title,
        body: issue.body.unwrap_or_default(),
        state: issue.state,
        labels: issue.labels.into_iter().map(|label| label.name).collect(),
    };
    let text = issue_text(&result)?;
    if secret.is_some_and(|token| !token.is_empty() && text.contains(token))
        || contains_github_token(&text)
    {
        return Err(
            "The issue contains credential-like content and cannot be imported safely.".to_owned(),
        );
    }
    Ok(result)
}

fn contains_github_token(text: &str) -> bool {
    ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"]
        .iter()
        .any(|prefix| {
            text.match_indices(prefix).any(|(i, _)| {
                text[i + prefix.len()..]
                    .bytes()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
                    .count()
                    >= 16
            })
        })
}

fn issue_text(issue: &NormalizedIssue) -> Result<String, String> {
    let text = format!(
        "Title: {}\nState: {}\nLabels: {}\n\nDescription:\n{}",
        issue.title,
        issue.state,
        issue.labels.join(", "),
        issue.body
    );
    if text.chars().count() > MAX_ISSUE_TEXT {
        return Err(format!(
            "The GitHub issue is too large to import (maximum {MAX_ISSUE_TEXT} characters)."
        ));
    }
    Ok(text)
}

fn read_limited<R: Read>(reader: R) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_RESPONSE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Unable to read GitHub CLI output.".to_owned())?;
    if bytes.len() > MAX_RESPONSE {
        return Err("GitHub returned an issue response that is too large.".to_owned());
    }
    Ok(bytes)
}

fn run_gh(args: &[&str]) -> Result<(bool, Vec<u8>, Vec<u8>), String> {
    let mut child = Command::new(if cfg!(windows) { "gh.exe" } else { "gh" })
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            "Unable to start GitHub CLI. Install gh and ensure it is on PATH.".to_owned()
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Unable to read GitHub CLI output.".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Unable to read GitHub CLI errors.".to_owned())?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let (sender, receiver) = mpsc::channel();
    let out_sender = sender.clone();
    thread::spawn(move || {
        let _ = out_sender.send((true, read_limited(stdout)));
    });
    thread::spawn(move || {
        let _ = sender.send((false, read_limited(stderr)));
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("GitHub CLI timed out.".to_owned());
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("Unable to wait for GitHub CLI.".to_owned());
            }
        }
    };
    let status = status?;
    let mut stdout = None;
    let mut stderr = None;
    for _ in 0..2 {
        let (is_stdout, output) = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "GitHub CLI output timed out or could not be read.".to_owned())?;
        if is_stdout {
            stdout = Some(output?);
        } else {
            stderr = Some(output?);
        }
    }
    Ok((
        status.success(),
        stdout.ok_or_else(|| "Unable to read GitHub CLI output.".to_owned())?,
        stderr.ok_or_else(|| "Unable to read GitHub CLI errors.".to_owned())?,
    ))
}

fn cli_issue(location: &IssueLocation) -> Result<NormalizedIssue, String> {
    normalized_issue(&cli_issue_with(location, run_gh)?, location, None)
}

fn cli_issue_with(
    location: &IssueLocation,
    mut run: impl FnMut(&[&str]) -> Result<(bool, Vec<u8>, Vec<u8>), String>,
) -> Result<Vec<u8>, String> {
    let (authenticated, _, _) = run(&["auth", "status", "--hostname", &location.host])?;
    if !authenticated {
        return Err("GitHub CLI is not authenticated for this host. Run gh auth login --hostname for the host.".to_owned());
    }
    let repo = format!(
        "{}/{}/{}",
        location.host, location.owner, location.repository
    );
    let number = location.number.to_string();
    let (success, output, error) = run(&[
        "issue",
        "view",
        &number,
        "--repo",
        &repo,
        "--json",
        "number,title,body,state,labels",
    ])?;
    if !success {
        let detail = String::from_utf8_lossy(&error).to_ascii_lowercase();
        return Err(if detail.contains("rate limit") {
            "GitHub rate limit reached. Try again later."
        } else if detail.contains("not found")
            || detail.contains("could not resolve")
            || detail.contains("403")
            || detail.contains("404")
        {
            "The issue was not found or this account lacks repository access."
        } else {
            "GitHub CLI could not retrieve the issue. Check the host and repository access."
        }
        .to_owned());
    }
    Ok(output)
}

fn api_error(status: StatusCode, rate_remaining: Option<&str>, retry_after: bool) -> String {
    if status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN && (rate_remaining == Some("0") || retry_after))
    {
        "GitHub rate limit reached. Try again later.".to_owned()
    } else if status == StatusCode::UNAUTHORIZED {
        "GitHub rejected the saved token. Check this host's credentials.".to_owned()
    } else if status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND {
        "The issue was not found or this token lacks repository access.".to_owned()
    } else {
        format!(
            "GitHub returned HTTP {} while retrieving the issue.",
            status.as_u16()
        )
    }
}

fn issue_api_url(location: &IssueLocation) -> String {
    let base = if location.host == "github.com" {
        "https://api.github.com".to_owned()
    } else {
        format!("https://{}/api/v3", location.host)
    };
    format!(
        "{base}/repos/{}/{}/issues/{}",
        location.owner, location.repository, location.number
    )
}

fn github_client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(Policy::none())
}

async fn token_issue(location: &IssueLocation, token: &str) -> Result<NormalizedIssue, String> {
    let client = github_client_builder()
        .build()
        .map_err(|_| "Unable to prepare the GitHub connection.".to_owned())?;
    token_issue_at(&client, &issue_api_url(location), location, token).await
}

async fn token_issue_at(
    client: &Client,
    url: &str,
    location: &IssueLocation,
    token: &str,
) -> Result<NormalizedIssue, String> {
    let mut response = client
        .get(url)
        .header("User-Agent", "Agent-Control-Plane")
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "Unable to connect to GitHub. Check the host and try again.".to_owned())?;
    let status = response.status();
    if !status.is_success() {
        let remaining = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut retry_after = response.headers().contains_key("retry-after");
        if status == StatusCode::FORBIDDEN && remaining.as_deref() != Some("0") && !retry_after {
            let body = response_bytes(&mut response).await?;
            retry_after = String::from_utf8_lossy(&body)
                .to_ascii_lowercase()
                .contains("rate limit");
        }
        return Err(api_error(status, remaining.as_deref(), retry_after));
    }
    let bytes = response_bytes(&mut response).await?;
    normalized_issue(&bytes, location, Some(token))
}

async fn response_bytes(response: &mut reqwest::Response) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Unable to read the GitHub issue response.".to_owned())?
    {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err("GitHub returned an issue response that is too large.".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn reject_duplicate(
    connection: &rusqlite::Connection,
    project_id: &str,
    source_key: &str,
) -> Result<(), String> {
    let duplicate = connection
        .query_row(
            "SELECT 1 FROM project_tasks WHERE project_id = ?1 AND source_key = ?2",
            params![project_id, source_key],
            |_| Ok(()),
        )
        .optional()
        .map_err(|_| "Unable to check for duplicate tasks.".to_owned())?
        .is_some();
    if duplicate {
        return Err("A task imported from this issue already exists in this project.".to_owned());
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn import_github_issue(
    url: String,
    project_id: String,
    request_id: String,
    app: tauri::AppHandle,
    database: State<'_, Database>,
) -> Result<GithubDraft, String> {
    let location = parse_issue_url(&url)?;
    let (method, provider) = {
        let connection = database
            .0
            .lock()
            .map_err(|_| "Project database is unavailable.".to_owned())?;
        let project_exists = connection
            .query_row(
                "SELECT 1 FROM projects WHERE id = ?1",
                [&project_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|_| "Unable to check the project.".to_owned())?
            .is_some();
        if !project_exists {
            return Err("The selected project is no longer registered.".to_owned());
        }
        reject_duplicate(&connection, &project_id, &location.source_key)?;
        let method = auth_method(&connection, &location.host)?;
        let provider = connection
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'default_provider'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| "Unable to read AI agent settings.".to_owned())?
            .unwrap_or_else(|| "claudeCode".to_owned());
        let tool = match provider.as_str() {
            "copilot" => "copilot",
            "claudeCode" => "claude_code",
            _ => return Err("Choose GitHub Copilot CLI or Claude Code CLI as the default agent to import GitHub issues.".to_owned()),
        };
        let enabled = connection
            .query_row(
                "SELECT enabled FROM tool_settings WHERE provider = ?1",
                [tool],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map_err(|_| "Unable to read AI agent settings.".to_owned())?
            .unwrap_or(false);
        if !enabled {
            return Err(
                "Enable the default AI agent in Settings → Integrations before importing."
                    .to_owned(),
            );
        }
        (method, provider)
    };
    let (issue, token) = if method == "token" {
        let token = Entry::new(CREDENTIAL_SERVICE, &location.host)
            .and_then(|entry| entry.get_password())
            .map_err(|error| match error {
                keyring::Error::NoEntry => {
                    "No personal access token is configured for this GitHub host.".to_owned()
                }
                _ => "Unable to access the OS credential vault.".to_owned(),
            })?;
        if !token.is_empty() && url.contains(&token) {
            return Err(
                "The issue URL contains credential-like content and cannot be imported safely."
                    .to_owned(),
            );
        }
        let issue = token_issue(&location, &token).await?;
        (issue, Some(token))
    } else {
        let cli_location = IssueLocation {
            host: location.host.clone(),
            owner: location.owner.clone(),
            repository: location.repository.clone(),
            number: location.number,
            url: location.url.clone(),
            source_key: location.source_key.clone(),
        };
        let issue = tauri::async_runtime::spawn_blocking(move || cli_issue(&cli_location))
            .await
            .map_err(|_| "Unable to run GitHub CLI.".to_owned())??;
        (issue, None)
    };
    let text = issue_text(&issue)?;
    app.emit("github-issue-import-stage", &request_id)
        .map_err(|_| "Unable to report import progress.".to_owned())?;
    let draft = tauri::async_runtime::spawn_blocking(move || {
        let draft = jira::generate_github_task_draft(&provider, &text)?;
        if token.as_deref().is_some_and(|secret| !secret.is_empty() && (draft.title.contains(secret) || draft.description.contains(secret)))
            || contains_github_token(&draft.title) || contains_github_token(&draft.description) {
            return Err("The generated draft contains credential-like content and cannot be imported safely.".to_owned());
        }
        Ok(draft)
    }).await.map_err(|_| "Unable to start AI task generation.".to_owned())??;
    Ok(GithubDraft {
        title: draft.title,
        description: draft.description,
        source_url: url.trim().to_owned(),
        source_key: location.source_key,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct IssueLocation {
    pub(crate) host: String,
    pub(crate) owner: String,
    pub(crate) repository: String,
    pub(crate) number: u64,
    pub(crate) url: String,
    pub(crate) source_key: String,
}

pub(crate) fn parse_issue_url(input: &str) -> Result<IssueLocation, String> {
    let input = input.trim();
    if input.len() > 2048 || contains_github_token(input) {
        return Err("The issue URL is too long or contains credential-like content.".to_owned());
    }
    let url = Url::parse(input).map_err(|_| "Enter a valid GitHub issue URL.".to_owned())?;
    let rest = input
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or_else(|| "Enter a valid GitHub issue URL.".to_owned())?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let raw_path = rest
        .get(authority.len()..)
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    if url.scheme() != "https"
        || authority.contains('@')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || raw_path != url.path()
        || url.query_pairs().any(|(key, _)| {
            let key = key.to_ascii_lowercase();
            key.contains("token") || key.contains("password") || key.contains("secret")
        })
    {
        return Err(
            "Use an HTTPS GitHub issue URL without embedded credentials or a port.".to_owned(),
        );
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "Enter a valid GitHub issue host.".to_owned())?
        .to_ascii_lowercase();
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    let parts: Vec<_> = path.split('/').collect();
    let valid_name = |name: &str| {
        !name.is_empty()
            && name.len() <= 100
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
            && name != "."
            && name != ".."
    };
    if parts.len() != 5
        || parts[0] != ""
        || !valid_name(parts[1])
        || !valid_name(parts[2])
        || parts[3] != "issues"
        || parts[4].is_empty()
        || !parts[4].bytes().all(|c| c.is_ascii_digit())
    {
        return Err(
            "Enter a GitHub issue URL ending in /owner/repository/issues/number.".to_owned(),
        );
    }
    let number = parts[4]
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| "The GitHub issue number is invalid.".to_owned())?;
    let owner = parts[1].to_owned();
    let repository = parts[2].to_owned();
    let canonical = format!("https://{host}/{owner}/{repository}/issues/{number}");
    let source_key = format!(
        "github:{}/{}/{}/{}",
        host,
        owner.to_ascii_lowercase(),
        repository.to_ascii_lowercase(),
        number
    );
    Ok(IssueLocation {
        host,
        owner,
        repository,
        number,
        url: canonical,
        source_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener};

    fn serve_once(response: String) -> (std::net::SocketAddr, thread::JoinHandle<Option<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local mock server");
        let address = listener.local_addr().expect("local address");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return None;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("mock server accept failed: {error}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read timeout");
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.ends_with(b"\r\n\r\n") && request.len() < 16 * 1024 {
                let count = socket.read(&mut buffer).expect("read mock request");
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            let _ = socket.write_all(response.as_bytes());
            Some(String::from_utf8(request).expect("HTTP request"))
        });
        (address, server)
    }

    fn mock_response(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn canonicalizes_issue_identity() {
        let issue = parse_issue_url("https://GitHub.com/Owner/Repo/issues/0042?x=1#issuecomment-1")
            .unwrap();
        assert_eq!(issue.url, "https://github.com/Owner/Repo/issues/42");
        assert_eq!(issue.source_key, "github:github.com/owner/repo/42");
        assert_eq!(
            parse_issue_url("https://github.company.com/team/repo/issues/1")
                .unwrap()
                .host,
            "github.company.com"
        );
        assert_eq!(
            parse_issue_url("https://github.com/owner/repo/issues/42/")
                .unwrap()
                .source_key,
            issue.source_key
        );
    }

    #[test]
    fn rejects_invalid_issue_urls() {
        for input in [
            "http://github.com/a/b/issues/1",
            "https://user:password@github.com/a/b/issues/1",
            "https://@github.com/a/b/issues/1",
            "https://github.com/a/b/pull/1",
            "https://github.com/a/b/issues/0",
            "https://github.com/a/b/issues/1/extra",
            "https://github.com/a/b/issues/not-a-number",
            "https://github.com/a%2Fb/c/issues/1",
            "https://github.com:8443/a/b/issues/1",
            "https://github.com/a/../a/b/issues/1",
            "https://github.com/a/b/issues/1//",
            "https://github.com/a/b/issues/1?access_token=secret",
        ] {
            assert!(parse_issue_url(input).is_err(), "{input}");
        }
    }

    #[test]
    fn selects_only_the_configured_host_authentication() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO app_settings VALUES ('github_issue_host:git.example.com', 'token');
            INSERT INTO app_settings VALUES ('github_issue_host:other.example.com', 'cli');",
        )
        .unwrap();
        assert_eq!(auth_method(&db, "github.com").unwrap(), "cli");
        assert_eq!(auth_method(&db, "git.example.com").unwrap(), "token");
        assert_eq!(auth_method(&db, "other.example.com").unwrap(), "cli");
        assert!(
            auth_method(&db, "unknown.example.com")
                .unwrap_err()
                .contains("Configure")
        );
        assert!(validate_host("https://git.example.com").is_err());
        assert!(validate_host("127.0.0.1").is_err());
        assert_eq!(validate_host("Git.Example.Com").unwrap(), "git.example.com");
    }

    #[test]
    fn cli_checks_host_and_fetches_structured_issue_without_token_command() {
        let issue = parse_issue_url("https://git.example.com/team/repo/issues/12").unwrap();
        let mut calls: Vec<Vec<String>> = Vec::new();
        let result = cli_issue_with(&issue, |args| {
            calls.push(args.iter().map(|arg| (*arg).to_owned()).collect());
            Ok((true, b"{}".to_vec(), Vec::new()))
        })
        .unwrap();
        assert_eq!(result, b"{}");
        assert_eq!(
            calls[0],
            ["auth", "status", "--hostname", "git.example.com"]
        );
        assert_eq!(
            calls[1],
            [
                "issue",
                "view",
                "12",
                "--repo",
                "git.example.com/team/repo",
                "--json",
                "number,title,body,state,labels"
            ]
        );
        let mut count = 0;
        let error = cli_issue_with(&issue, |_| {
            count += 1;
            Ok((false, Vec::new(), b"private credential".to_vec()))
        })
        .unwrap_err();
        assert_eq!(count, 1);
        assert!(error.contains("not authenticated"));
        assert!(!error.contains("private credential"));
    }

    #[test]
    #[ignore = "requires GitHub CLI authentication and network access"]
    fn authenticated_cli_fetches_public_issue() {
        let location = parse_issue_url("https://github.com/cli/cli/issues/1").unwrap();
        let issue = cli_issue(&location).expect("fetch public GitHub issue with gh");
        assert!(!issue.title.trim().is_empty());
    }

    #[test]
    fn normalizes_responses_and_rejects_unsafe_or_incorrect_data() {
        let issue = parse_issue_url("https://github.com/team/repo/issues/12").unwrap();
        let json = br#"{"number":12,"title":"Fix login","body":"Reproduce error","state":"OPEN","labels":[{"name":"bug"}]}"#;
        let normalized = normalized_issue(json, &issue, None).unwrap();
        assert!(issue_text(&normalized).unwrap().contains("Labels: bug"));
        assert!(normalized_issue(json, &issue, Some("Reproduce error")).is_err());
        assert!(
            normalized_issue(
                br#"{"number":13,"title":"Wrong","body":null,"state":"open","labels":[]}"#,
                &issue,
                None
            )
            .is_err()
        );
        assert!(normalized_issue(br#"{"number":12,"title":"PR","body":null,"state":"open","labels":[],"pull_request":{}}"#, &issue, None).is_err());
        let too_large = format!(
            "{{\"number\":12,\"title\":\"x\",\"body\":\"{}\",\"state\":\"open\",\"labels\":[]}}",
            "x".repeat(MAX_ISSUE_TEXT)
        );
        assert!(normalized_issue(too_large.as_bytes(), &issue, None).is_err());
        let secret = format!(
            "{{\"number\":12,\"title\":\"ghp_{}\",\"body\":null,\"state\":\"open\",\"labels\":[]}}",
            "a".repeat(36)
        );
        assert!(normalized_issue(secret.as_bytes(), &issue, None).is_err());
    }

    #[test]
    fn api_url_errors_and_duplicate_checks_are_explicit() {
        let public = parse_issue_url("https://github.com/team/repo/issues/12").unwrap();
        let enterprise = parse_issue_url("https://git.example.com/team/repo/issues/12").unwrap();
        assert_eq!(
            issue_api_url(&public),
            "https://api.github.com/repos/team/repo/issues/12"
        );
        assert_eq!(
            issue_api_url(&enterprise),
            "https://git.example.com/api/v3/repos/team/repo/issues/12"
        );
        assert!(api_error(StatusCode::UNAUTHORIZED, None, false).contains("rejected"));
        assert!(api_error(StatusCode::FORBIDDEN, Some("0"), false).contains("rate limit"));
        assert!(api_error(StatusCode::FORBIDDEN, None, true).contains("rate limit"));
        assert!(api_error(StatusCode::NOT_FOUND, None, false).contains("access"));
        assert!(api_error(StatusCode::BAD_GATEWAY, None, false).contains("502"));
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE project_tasks (project_id TEXT, source_key TEXT);
            INSERT INTO project_tasks VALUES ('project-a', 'github:github.com/team/repo/12');",
        )
        .unwrap();
        assert!(reject_duplicate(&db, "project-a", &public.source_key).is_err());
        assert!(reject_duplicate(&db, "project-b", &public.source_key).is_ok());
    }

    #[test]
    fn enterprise_token_adapter_sends_authorization_and_normalizes_the_response() {
        let location = parse_issue_url("https://git.example.com/team/repo/issues/12").unwrap();
        let body = r#"{"number":12,"title":"Fix login","body":"Steps","state":"open","labels":[{"name":"bug"}],"comments":[{"body":"not imported"}]}"#;
        let (address, server) = serve_once(mock_response("200 OK", "", body));
        let path = Url::parse(&issue_api_url(&location))
            .unwrap()
            .path()
            .to_owned();
        let url = format!("http://{address}{path}");
        let client = github_client_builder().no_proxy().build().unwrap();
        let issue = tauri::async_runtime::block_on(token_issue_at(
            &client,
            &url,
            &location,
            "test-fixture-token",
        ))
        .expect("fetch issue");
        let request = server.join().unwrap().expect("request reached mock");
        assert!(request.starts_with("GET /api/v3/repos/team/repo/issues/12 HTTP/1.1\r\n"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-fixture-token\r\n")
        );
        let text = issue_text(&issue).unwrap();
        assert!(text.contains("Fix login") && text.contains("Labels: bug"));
        assert!(!text.contains("not imported"));
    }

    #[test]
    fn token_adapter_does_not_follow_redirects_or_expose_error_bodies() {
        let location = parse_issue_url("https://git.example.com/team/repo/issues/12").unwrap();
        let client = github_client_builder().no_proxy().build().unwrap();
        let (target, target_server) = serve_once(mock_response("200 OK", "", "{}"));
        let (address, redirect_server) = serve_once(mock_response(
            "302 Found",
            &format!("Location: http://{target}/leak\r\n"),
            "",
        ));
        let error = tauri::async_runtime::block_on(token_issue_at(
            &client,
            &format!("http://{address}/api/v3/repos/team/repo/issues/12"),
            &location,
            "test-fixture-token",
        ))
        .unwrap_err();
        assert!(error.contains("302"));
        assert!(!error.contains("test-fixture-token"));
        assert!(redirect_server.join().unwrap().is_some());
        assert!(target_server.join().unwrap().is_none());

        let body = r#"{"message":"secondary rate limit; test-fixture-token"}"#;
        let (address, server) = serve_once(mock_response("403 Forbidden", "", body));
        let error = tauri::async_runtime::block_on(token_issue_at(
            &client,
            &format!("http://{address}/api/v3/repos/team/repo/issues/12"),
            &location,
            "test-fixture-token",
        ))
        .unwrap_err();
        assert!(error.contains("rate limit"));
        assert!(!error.contains("test-fixture-token"));
        assert!(server.join().unwrap().is_some());
    }

    #[test]
    fn token_adapter_reports_authentication_and_response_size_errors() {
        let location = parse_issue_url("https://git.example.com/team/repo/issues/12").unwrap();
        let client = github_client_builder().no_proxy().build().unwrap();
        for (status, body, expected) in [
            (
                "401 Unauthorized",
                "test-fixture-token".to_owned(),
                "rejected",
            ),
            ("429 Too Many Requests", String::new(), "rate limit"),
            ("200 OK", "x".repeat(MAX_RESPONSE + 1), "too large"),
        ] {
            let (address, server) = serve_once(mock_response(status, "", &body));
            let error = tauri::async_runtime::block_on(token_issue_at(
                &client,
                &format!("http://{address}/api/v3/repos/team/repo/issues/12"),
                &location,
                "test-fixture-token",
            ))
            .unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("test-fixture-token"));
            assert!(server.join().unwrap().is_some());
        }
    }

    #[test]
    #[ignore = "requires an issue URL and configured host authentication"]
    fn authenticated_configured_issue_import() {
        let url = std::env::var("ACP_ISSUE_URL")
            .expect("Set ACP_ISSUE_URL to an accessible GitHub issue URL");
        let location = parse_issue_url(&url).expect("valid GitHub issue URL");
        let method = std::env::var("ACP_AUTH_METHOD").expect("Set ACP_AUTH_METHOD to cli or token");
        let issue = match method.as_str() {
            "cli" => cli_issue(&location).expect("GitHub CLI issue retrieval"),
            "token" => {
                let source =
                    std::env::var("ACP_TOKEN_SOURCE").unwrap_or_else(|_| "issue-import".to_owned());
                let (service, account) = match source.as_str() {
                    "issue-import" => (CREDENTIAL_SERVICE, location.host.as_str()),
                    "github-connection" if location.host == "github.com" => (
                        crate::GITHUB_CREDENTIAL_SERVICE,
                        crate::GITHUB_CREDENTIAL_USER,
                    ),
                    _ => panic!("Unsupported token source for this GitHub host"),
                };
                let token = Entry::new(service, account)
                    .map_err(|_| "Unable to initialize the credential vault")
                    .and_then(|entry| {
                        entry.get_password().map_err(|error| match error {
                            keyring::Error::NoEntry => {
                                "No token is saved for the selected credential source"
                            }
                            _ => "The credential vault is unavailable or locked",
                        })
                    })
                    .expect("GitHub token lookup");
                tauri::async_runtime::block_on(token_issue(&location, &token))
                    .expect("GitHub token issue retrieval")
            }
            _ => panic!("ACP_AUTH_METHOD must be cli or token"),
        };
        assert!(!issue.title.trim().is_empty());
    }
}
