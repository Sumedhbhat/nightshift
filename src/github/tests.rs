use super::*;
use std::io::{Read, Write};

fn run(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

fn mock(
    responses: Vec<(&'static str, &'static str, &'static str)>,
) -> (GitHub, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut requests = vec![];
        for (status, headers, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8(request).unwrap());
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    let mut api = GitHub::new().unwrap();
    api.base = base;
    (api, server)
}

fn mock_with_next_link(
    second_status: &'static str,
    second_body: &'static str,
) -> (GitHub, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let next = format!("{base}/page2?after=cursor-token");
    let server = std::thread::spawn(move || {
        let mut requests = vec![];
        for (status, headers, body) in [
            (
                "200 OK",
                format!("Link: <{next}>; rel=\"next\"\r\n"),
                r#"[{"number":1,"title":"One","labels":[{"name":"ready-for-agent"}]},{"number":2,"title":"PR","labels":[],"pull_request":{}}]"#,
            ),
            (second_status, String::new(), second_body),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8(request).unwrap());
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    let mut api = GitHub::new().unwrap();
    api.base = base;
    (api, server)
}

fn project() -> Project {
    Project {
        id: 1,
        checkout_path: "/tmp/repo".into(),
        github_host: "github.com".into(),
        repository_id: "R_1".into(),
        owner: "owner".into(),
        name: "repo".into(),
        remote: "origin".into(),
        setup_command: None,
    }
}

#[test]
fn github_remote_formats_and_invalid_hosts() {
    for url in [
        "git@github.com:owner/repo.git",
        "https://github.com/owner/repo",
        "ssh://git@github.com/owner/repo.git",
    ] {
        assert_eq!(parse_remote(url), Some(("owner".into(), "repo".into())));
    }
    for url in [
        "https://github.com.evil.test/owner/repo",
        "https://gitlab.com/owner/repo",
        "https://github.com/owner/repo/issues",
        "git@github.com:owner/../repo",
        "file:///tmp/repo",
    ] {
        assert_eq!(parse_remote(url), None);
    }
}

#[test]
fn follows_pages_excludes_prs_and_requests_readiness_filter() {
    run(async {
        let (api, server) =
            mock_with_next_link("200 OK", r#"[{"number":3,"title":"Three","labels":[]}]"#);
        let result = api.issues(&project(), true).await;
        assert!(result.error.is_none());
        assert_eq!(
            result.issues.iter().map(|i| i.number).collect::<Vec<_>>(),
            [1, 3]
        );
        let requests = server.join().unwrap();
        assert!(requests[0].contains("labels=ready-for-agent"));
        assert!(requests[1].contains("GET /page2?after=cursor-token HTTP/1.1"));
        assert!(!requests[0].to_lowercase().contains("authorization:"));
    });
}

#[test]
fn issue_page_caps_requests_at_ten_and_returns_the_cursor() {
    run(async {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let next = format!("{base}/page2?after=cursor-token");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let body = r#"[{"number":1,"title":"One","labels":[]},{"number":2,"title":"PR","labels":[],"pull_request":{}}]"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nLink: <{next}>; rel=\"next\"\r\n\r\n{body}", body.len()).unwrap();
            String::from_utf8(request).unwrap()
        });
        let mut api = GitHub::new().unwrap();
        api.base = base;
        let page = api.issue_page(&project(), false, None).await;
        assert!(page.error.is_none());
        assert_eq!(
            page.issues
                .iter()
                .map(|issue| issue.number)
                .collect::<Vec<_>>(),
            [1]
        );
        assert!(page.next.unwrap().contains("after=cursor-token"));
        let request = server.join().unwrap();
        assert!(request.contains("per_page=10"));
    });
}

#[test]
fn failed_page_keeps_partial_results() {
    run(async {
        let (api, server) = mock_with_next_link("403 Forbidden", "{}");
        let result = api.issues(&project(), false).await;
        assert_eq!(result.issues.len(), 1);
        assert!(result.error.unwrap().contains("rate limit"));
        server.join().unwrap();
    });
}

#[test]
fn reports_github_validation_message() {
    run(async {
        let (api, server) = mock(vec![(
            "422 Unprocessable Content",
            "",
            r#"{"message":"Use cursor based pagination"}"#,
        )]);
        let result = api.issues(&project(), false).await;
        let error = result.error.unwrap();
        assert!(error.contains("HTTP 422"));
        assert!(error.contains("Use cursor based pagination"));
        server.join().unwrap();
    });
}

#[test]
fn public_metadata_is_verified_and_private_repo_rejected() {
    run(async {
        let (api, server) = mock(vec![
            (
                "200 OK",
                "",
                r#"{"node_id":"R_123","name":"canonical","owner":{"login":"Owner"},"private":false}"#,
            ),
            (
                "200 OK",
                "",
                r#"{"node_id":"R_456","name":"secret","owner":{"login":"Owner"},"private":true}"#,
            ),
        ]);
        let remote = Remote {
            checkout: "/tmp/local".into(),
            name: "origin".into(),
            owner: "owner".into(),
            repo: "repo".into(),
        };
        let verified = api.verify(remote.clone()).await.unwrap();
        assert_eq!(verified.repository_id, "R_123");
        assert_eq!(verified.name, "canonical");
        assert!(api.verify(remote).await.is_err());
        server.join().unwrap();
    });
}

#[test]
fn checkout_inspection_validates_git_and_keeps_distinct_remotes() {
    run(async {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("/usr/bin/git")
                    .arg("-C")
                    .arg(dir.path())
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        assert!(inspect_checkout(dir.path().into()).await.is_err());
        git(&["init"]);
        assert!(inspect_checkout(dir.path().into()).await.is_err());
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ]);
        git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
        git(&["remote", "add", "mirror", "https://github.com/owner/repo"]);
        git(&["remote", "add", "upstream", "https://github.com/other/repo"]);
        let remotes = inspect_checkout(dir.path().into()).await.unwrap();
        assert_eq!(remotes.len(), 2);
        assert_eq!(remotes[0].name, "origin");
    });
}
