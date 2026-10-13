use std::{
    io::Write,
    process::{Command, Stdio},
};

fn helper(host: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mushy-askpass"));
    command.env("MUSHY_AUTH_HOST", host);
    command
}
struct Cleanup(String);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = helper(&self.0).arg("--forget").output();
    }
}
/// Requires a real, unlocked OS store. CI provisions one on each native runner.
#[test]
#[ignore = "requires an unlocked OS secure credential store"]
fn secure_credentials_and_askpass_roundtrip() {
    let host = format!("mushy-ci-{}", uuid::Uuid::new_v4());
    let _cleanup = Cleanup(host.clone());
    for _ in 0..2 {
        let password = format!("synthetic-test-password-{}", uuid::Uuid::new_v4());
        let mut writer = helper(&host)
            .arg("--store")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(password.as_bytes())
            .unwrap();
        let saved = writer.wait_with_output().unwrap();
        assert!(
            saved.status.success(),
            "OS secure store could not save credential"
        );
        assert!(saved.stdout.is_empty());
        assert!(saved.stderr.is_empty());
        let output = helper(&host).arg("SSH password:").output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, format!("{password}\n").as_bytes());
        assert!(output.stderr.is_empty());
    }
    let unsupported = helper(&host)
        .arg("Enter passphrase for key:")
        .output()
        .unwrap();
    assert!(!unsupported.status.success());
    assert!(unsupported.stdout.is_empty());
    let state = tempfile::tempdir().unwrap();
    let forgotten = Command::new(env!("CARGO_BIN_EXE_mushy"))
        .env("MUSHY_STATE_DIR", state.path())
        .args(["auth", "forget", &host])
        .output()
        .unwrap();
    assert!(forgotten.status.success());
    let missing = helper(&host).arg("SSH password:").output().unwrap();
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(missing.stderr.is_empty());
}
