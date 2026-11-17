//! Integration smoke test for the `soul-server` CLI binary.

use assert_cmd::Command;

use soul_server::domain::ports::UserStore;
use soul_server::store::FileUsers;

#[test]
fn user_add_creates_a_verifiable_user() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("u.json");
    let config_path = dir.path().join("soul-server.toml");
    std::fs::write(
        &config_path,
        format!(
            "bind = \"127.0.0.1:0\"\nroots = []\ndb = \"{}\"\n",
            db_path.display().to_string().replace('\\', "\\\\")
        ),
    )
    .unwrap();

    Command::cargo_bin("soul-server")
        .unwrap()
        .args([
            "user",
            "add",
            "bob",
            "--password",
            "pw",
            "--config",
            config_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let store = FileUsers::open(&db_path).unwrap();
    assert!(store.verify("bob", "pw").is_some());
}
