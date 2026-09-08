use std::io;
use std::process::Command;

#[test]
fn no_connection_setup_is_required_but_an_api_key_still_is() -> io::Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_jio"))
        .arg("usage")
        .env_remove("JIO_ENDPOINT")
        .env_remove("JIO_HOST")
        .env_remove("JIO_CA_CERT")
        .env_remove("JIO_API_KEY")
        .output()?;
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "jio: JIO_API_KEY is required\n"
    );
    assert!(output.stdout.is_empty());
    Ok(())
}
