use jio_client::{CommandResult, Session, VmClient};
use std::env;
use std::ffi::OsString;
use std::io;
use std::time::Duration;

pub fn create(host: String) -> io::Result<Session> {
    let client = client(host)?;
    let session = client.create()?.session();
    client.set_current_session_id(&session.session_id)?;
    Ok(session)
}

pub fn connect(host: String, id: Option<&str>) -> io::Result<()> {
    let client = client(host)?;
    let id = resolve_session_id(&client, id)?;
    client.attach(&id)?.connect()?;
    client.set_current_session_id(&id)
}

pub fn exec(host: String, id: &str, command: &str, timeout: Duration) -> io::Result<CommandResult> {
    client(host)?.attach(id)?.exec(command, timeout)
}

pub fn destroy(host: String, id: &str) -> io::Result<()> {
    let client = client(host)?;
    client.destroy(id)?;
    client.clear_current_session_id(id)
}

pub fn target_session_id(host: &str, id: Option<&str>) -> io::Result<String> {
    let client = client(host.to_owned())?;
    resolve_session_id(&client, id)
}

pub fn stop(host: String, id: &str) -> io::Result<Session> {
    client(host)?.stop(id)
}

pub fn start(host: String, id: &str) -> io::Result<Session> {
    client(host)?.start(id)
}

fn client(host: String) -> io::Result<VmClient> {
    let api_key = env::var("JIO_API_KEY")
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "JIO_API_KEY is required"))?;
    VmClient::new(host, api_key)
}

fn resolve_session_id(client: &VmClient, id: Option<&str>) -> io::Result<String> {
    if let Some(id) = id {
        if !jio_client::valid_session_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session ID is invalid",
            ));
        }
        return Ok(id.to_owned());
    }
    client.current_session_id()?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no current session; run jio create or jio connect <session-id>",
        )
    })
}

pub fn host(value: Option<OsString>) -> io::Result<String> {
    value
        .map(|value| {
            value
                .into_string()
                .map_err(|_| invalid("host is not UTF-8"))
        })
        .transpose()?
        .or_else(|| env::var("JIO_ENDPOINT").ok())
        .or_else(|| env::var("JIO_HOST").ok())
        .ok_or_else(|| invalid("missing --host <host> (or JIO_ENDPOINT/JIO_HOST)"))
}

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}
