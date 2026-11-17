//! Each pipeline's trigger mark starts that pipeline, and an item carrying
//! two waits until one is left.

use crate::labelled;
use crate::sandbox::Sandbox;

/// An `audit` pipeline: a worktree, then a command that succeeds.
const AUDIT: &str = r#"
[[pipeline.audit.step]]
id = "worktree"
uses = "git.worktree"

[[pipeline.audit.step]]
id = "audit"
uses = "atelier.command"
run = "true"
after = ["worktree"]
"#;

#[test]
fn an_atelier_audit_label_runs_the_audit_pipeline() {
    let sandbox = Sandbox::new(&labelled(&["atelier:audit"]));
    sandbox.config(AUDIT);

    let daemon = sandbox.daemon();
    sandbox.wait("the audit run", |sandbox| {
        sandbox.phase("12").as_deref() == Some("done")
    });
    drop(daemon);

    let log = sandbox.log();
    assert!(log.contains("audit github#12"), "{log}");
}

#[test]
fn an_item_with_two_triggers_is_logged_and_started_by_neither() {
    let sandbox = Sandbox::new(&labelled(&["atelier:build", "atelier:audit"]));
    sandbox.config(AUDIT);

    let daemon = sandbox.daemon();
    sandbox.wait("the ambiguity", |sandbox| {
        sandbox.log().contains(
            "github#12: carries the trigger marks of build, audit; \
             atelier starts none until one is left",
        )
    });
    drop(daemon);

    assert_eq!(sandbox.phase("12"), None);
}

#[test]
fn init_makes_a_label_for_each_pipeline_s_trigger() {
    let sandbox = Sandbox::new(&labelled(&[]));
    sandbox.config(AUDIT);

    let init = sandbox.atelier(&["init", "--path", &sandbox.checkout.to_string_lossy()]);

    let stdout = String::from_utf8(init.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&init.stderr);
    let made: Vec<String> = sandbox
        .gh_calls()
        .iter()
        .filter(|call| call[..2] == ["label", "create"])
        .map(|call| call[2].clone())
        .collect();
    assert!(init.status.success(), "{stderr}");
    assert_eq!(
        made,
        [
            "atelier:build",
            "atelier:audit",
            "atelier:working",
            "atelier:blocked",
            "improvement"
        ]
    );
    assert!(
        stdout.contains("labels atelier:build, atelier:audit, atelier:working"),
        "{stdout}"
    );
}

#[test]
fn a_pipeline_named_after_an_atelier_label_is_refused_by_status() {
    for name in ["working", "improvement"] {
        let sandbox = Sandbox::new(&labelled(&[]));
        sandbox.config(&format!("[pipeline.{name}]\n"));

        let status = sandbox.status();

        assert!(
            status.contains(&format!(
                ".atelier/config.toml: pipeline.{name}: {name} is atelier's own label; \
                 name the pipeline otherwise"
            )),
            "{name}: {status}"
        );
    }
}
