//! `COMPLETE=<shell> tisvlay` must emit each shell's registration script --
//! the bootstrap that packaging ships into the autoload dirs. Asserting the
//! per-shell marker here catches a `clap_complete` bump that changes the
//! template in CI, without needing a live shell.

use std::process::Command;

fn registration(shell: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_tisvlay"))
        .env("COMPLETE", shell)
        .output()
        .expect("run tisvlay");
    assert!(
        output.status.success(),
        "COMPLETE={shell} tisvlay exited with {:?}",
        output.status
    );
    String::from_utf8(output.stdout).expect("registration is utf-8")
}

#[test]
fn emits_bash_registration() {
    let script = registration("bash");
    assert!(
        script.contains("-F _clap_complete_tisvlay tisvlay"),
        "bash registration marker missing:\n{script}"
    );
}

#[test]
fn emits_zsh_registration() {
    let script = registration("zsh");
    assert!(
        script.contains("#compdef tisvlay"),
        "zsh registration marker missing:\n{script}"
    );
}

#[test]
fn emits_fish_registration() {
    let script = registration("fish");
    assert!(
        script.contains("--command tisvlay"),
        "fish registration marker missing:\n{script}"
    );
}
