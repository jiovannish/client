use super::*;
use std::{
    io::{BufRead, Read, Write},
    net::TcpListener,
    thread,
};

#[test]
fn names_are_bounded_and_ids_need_no_lookup() -> io::Result<()> {
    let api = SessionClient::new("http://127.0.0.1:1", "a".repeat(64))?;
    let id = "ab".repeat(16);
    assert_eq!(api.resolve_session_id(&id)?, id);
    for name in ["dev", "a", "my_vm-2", &"z".repeat(63)] {
        assert!(valid_vm_alias(name));
    }
    for name in [
        "",
        "Dev",
        "1dev",
        "-dev",
        "../dev",
        "dev/name",
        "dev name",
        "dev\n",
        "dév",
        &"z".repeat(64),
    ] {
        assert!(!valid_vm_alias(name));
        assert!(api.resolve_session_id(name).is_err());
        assert!(api.set_alias(&id, name).is_err());
    }
    assert!(!valid_vm_alias(&id));
    assert!(api.set_alias("dev", "name").is_err());
    Ok(())
}

#[test]
fn authenticated_bindings_are_checked_before_use() -> io::Result<()> {
    let id = "a".repeat(32);
    for method in ["GET", "PUT"] {
        for (status, body, valid) in [
            (
                200,
                serde_json::json!({"session_id":id,"alias":"dev"}),
                true,
            ),
            (
                200,
                serde_json::json!({"session_id":"../escape","alias":"dev"}),
                false,
            ),
            (
                200,
                serde_json::json!({"session_id":id,"alias":"other"}),
                false,
            ),
            (404, serde_json::json!({"error":"not found"}), false),
            (401, serde_json::json!({"error":"invalid API key"}), false),
            (
                409,
                serde_json::json!({"error":"alias is already assigned"}),
                false,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let endpoint = format!("http://{}", listener.local_addr()?);
            let path = if method == "GET" {
                "/v1/aliases/dev".to_owned()
            } else {
                format!("/v1/sessions/{id}/alias")
            };
            let server = thread::spawn(move || -> io::Result<()> {
                let (mut stream, _) = listener.accept()?;
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
                let mut reader = io::BufReader::new(&stream);
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line)? == 0 || line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                    assert!(headers.len() < 8192);
                }
                assert!(headers.starts_with(&format!("{method} {path} HTTP/1.1\r\n")));
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains(&format!("authorization: bearer {}", "a".repeat(64)))
                );
                if method == "PUT" {
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .ok_or_else(|| io::Error::other("missing length"))?
                        .parse()
                        .map_err(io::Error::other)?;
                    assert!(length < 1024);
                    let mut request = vec![0; length];
                    reader.read_exact(&mut request)?;
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&request)?,
                        serde_json::json!({"alias":"dev"})
                    );
                }
                let body = body.to_string();
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )?;
                Ok(())
            });
            let api = SessionClient::new(endpoint, "a".repeat(64))?;
            let result = if method == "GET" {
                api.resolve_session_id("dev")
                    .map(|resolved| assert_eq!(resolved, id))
            } else {
                api.set_alias(&id, "dev")
            };
            server
                .join()
                .map_err(|_| io::Error::other("test endpoint failed"))??;
            assert_eq!(result.is_ok(), valid, "{method} {status}");
        }
    }
    Ok(())
}
