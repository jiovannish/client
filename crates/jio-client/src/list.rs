use crate::{
    Connection, DEFAULT_REQUEST_TIMEOUT, SessionClient, decode, invalid, other, valid_session_id,
};
use serde::Deserialize;
use std::{io, time::Instant};

/// An account VM's configured resources, including when compute is stopped.
#[derive(Debug)]
pub struct VmSummary {
    pub id: String,
    pub vcpu_count: u8,
    pub memory_mib: u64,
}

#[derive(Deserialize)]
struct Page {
    sessions: Vec<Entry>,
    next_after: Option<String>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    state: String,
    release: Resources,
}

#[derive(Deserialize)]
struct Resources {
    vcpu: u8,
    memory_mib: u64,
}

impl SessionClient {
    /// Lists non-destroyed VMs belonging to the authenticated account.
    /// Local SSH keys are not required. Standalone Core does not offer this route.
    pub fn list(&self) -> io::Result<Vec<VmSummary>> {
        let connection = Connection::open(&self.endpoint, &self.api_key, DEFAULT_REQUEST_TIMEOUT)?;
        let started = Instant::now();
        let mut after = String::new();
        let mut summaries = Vec::new();
        // Bound pagination even if a faulty endpoint keeps returning new IDs.
        for _ in 0..100 {
            let timeout = DEFAULT_REQUEST_TIMEOUT
                .checked_sub(started.elapsed())
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "VM listing timed out"))?;
            let response = connection
                .client
                .get(format!("{}/v1/sessions", connection.base_url))
                .query(&[("after", &after)])
                .bearer_auth(&connection.api_key)
                .timeout(timeout)
                .send()
                .map_err(other)?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "jio list requires account VM listing support",
                ));
            }
            let page: Page = decode(response)?;
            if page.sessions.len() > 100 {
                return Err(invalid("VM listing page exceeds limit"));
            }
            let mut last = after.as_str();
            for entry in &page.sessions {
                if !valid_session_id(&entry.id)
                    || entry.id.as_str() <= last
                    || entry.release.vcpu == 0
                    || !(1..=16_777_216).contains(&entry.release.memory_mib)
                    || entry.state.is_empty()
                    || entry.state.len() > 32
                    || !entry
                        .state
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c == b'_')
                {
                    return Err(invalid("endpoint returned invalid VM listing"));
                }
                last = &entry.id;
            }
            if page
                .next_after
                .as_deref()
                .is_some_and(|next| next != last || next <= after.as_str())
            {
                return Err(invalid("endpoint returned invalid VM listing cursor"));
            }
            summaries.extend(
                page.sessions
                    .into_iter()
                    .filter(|entry| entry.state != "destroyed")
                    .map(|entry| VmSummary {
                        id: entry.id,
                        vcpu_count: entry.release.vcpu,
                        memory_mib: entry.release.memory_mib,
                    }),
            );
            match page.next_after {
                Some(next) => after = next,
                None => return Ok(summaries),
            }
        }
        Err(invalid(
            "VM listing exceeds 100 pages; no partial list returned",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn lists_authenticated_pages_and_rejects_bad_responses() -> io::Result<()> {
        let a = "a".repeat(32);
        let b = "b".repeat(32);
        let entry = |id: &str, state: &str| {
            serde_json::json!({"id":id,"state":state,
            "release":{"vcpu":4,"memory_mib":8192}})
        };
        for (pages, valid) in [
            (
                vec![
                    (
                        200,
                        serde_json::json!({"sessions":[entry(&a,"destroyed")],"next_after":a}),
                    ),
                    (
                        200,
                        serde_json::json!({"sessions":[entry(&b,"stopped")],"next_after":null}),
                    ),
                ],
                true,
            ),
            (
                vec![(200, serde_json::json!({"sessions":[],"next_after":null}))],
                true,
            ),
            (
                vec![(
                    200,
                    serde_json::json!({"sessions":[entry("bad\u{1b}id","ready")],"next_after":null}),
                )],
                false,
            ),
            (
                vec![(
                    200,
                    serde_json::json!({"sessions":[entry(&a,"ready")],"next_after":""}),
                )],
                false,
            ),
            (
                vec![(
                    200,
                    serde_json::json!({"sessions":[entry(&a,"ready"),entry(&a,"ready")],"next_after":null}),
                )],
                false,
            ),
            (
                vec![(401, serde_json::json!({"error":"invalid API key"}))],
                false,
            ),
            (vec![(404, serde_json::json!({"error":"not found"}))], false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let endpoint = format!("http://{}", listener.local_addr()?);
            let expected_count = usize::from(pages.len() > 1);
            let server = thread::spawn(move || -> io::Result<()> {
                for (index, (status, body)) in pages.into_iter().enumerate() {
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
                    let cursor = if index == 0 {
                        String::new()
                    } else {
                        "a".repeat(32)
                    };
                    assert!(
                        headers
                            .starts_with(&format!("GET /v1/sessions?after={cursor} HTTP/1.1\r\n"))
                    );
                    assert!(
                        headers
                            .to_ascii_lowercase()
                            .contains(&format!("authorization: bearer {}", "a".repeat(64)))
                    );
                    let body = body.to_string();
                    write!(
                        stream,
                        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )?;
                }
                Ok(())
            });
            let result = SessionClient::new(endpoint, "a".repeat(64))?.list();
            server
                .join()
                .map_err(|_| io::Error::other("test endpoint failed"))??;
            if valid {
                let vms = result?;
                assert_eq!(vms.len(), expected_count);
                if let Some(vm) = vms.first() {
                    assert_eq!((&vm.id, vm.vcpu_count, vm.memory_mib), (&b, 4, 8192));
                }
            } else {
                assert!(result.is_err());
            }
        }
        Ok(())
    }
}
