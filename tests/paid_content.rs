//! Integration tests for `wavedash paid-content`, driving the real binary.
//!
//! Two tiers, because they can be trusted at different times:
//!
//! * **Surface tests** run everywhere. They cover argument parsing, aliases, and
//!   validation — everything clap settles before a request is ever built — so they
//!   are meaningful today, while the API routes are still unimplemented.
//!
//! * **Lifecycle tests** need a live API and are `#[ignore]`d. Run them with
//!   `cargo test --test paid_content -- --ignored` once the backend ships, with
//!   `WAVEDASH_TOKEN` and `WAVEDASH_GAME_ID` set. They deliberately panic rather
//!   than skip when those are missing: a test that silently no-ops is worse than
//!   one that never ran, because it reports success either way.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_wavedash");

fn command(args: &[&str]) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env_remove("WAVEDASH_TOKEN")
        .env_remove("WAVEDASH_GAME_ID")
        .env_remove("WAVEDASH_UPLOAD_DIR")
        .env_remove("CI")
        .env("HOME", env!("CARGO_TARGET_TMPDIR"))
        .env("USERPROFILE", env!("CARGO_TARGET_TMPDIR"));
    cmd
}

fn run(args: &[&str]) -> Output {
    command(args)
        .output()
        .expect("the wavedash binary should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn the_command_lists_all_five_subcommands() {
    let out = run(&["paid-content", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let help = stdout(&out);
    for subcommand in ["list", "create", "update", "deactivate", "resolve"] {
        assert!(
            help.contains(subcommand),
            "`{subcommand}` missing from help:\n{help}"
        );
    }
}

#[test]
fn the_unlockable_alias_reaches_the_same_command() {
    let out = run(&["unlockable", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("paywall"), "{}", stdout(&out));
}

#[test]
fn deactivate_is_reachable_as_delete_and_del() {
    for alias in ["delete", "del"] {
        let out = run(&["paid-content", alias, "--help"]);
        assert!(
            out.status.success(),
            "`{alias}` should resolve: {}",
            stderr(&out)
        );
        assert!(stdout(&out).contains("buyers keep access"), "{alias}");
    }
}

#[test]
fn the_listing_needs_no_flag_to_appear() {
    let help = stdout(&run(&["paid-content", "resolve", "--help"]));
    assert!(help.contains("--all-files"), "{help}");
    assert!(
        !help.contains("--show-files"),
        "the listing prints by default; there is nothing to switch on:\n{help}"
    );
}

#[test]
fn the_short_subcommand_aliases_reach_the_same_commands() {
    for (alias, canonical) in [("ls", "List paid content"), ("add", "Create a paywall")] {
        let out = run(&["paid-content", alias, "--help"]);
        assert!(
            out.status.success(),
            "`{alias}` should resolve: {}",
            stderr(&out)
        );
        assert!(
            stdout(&out).contains(canonical),
            "`{alias}` should reach the same command: {}",
            stdout(&out)
        );
    }
}

#[test]
fn the_concise_long_aliases_parse_the_same_as_their_canonical_forms() {
    let out = run(&[
        "paid-content",
        "create",
        "--game-id",
        "g",
        "full-version",
        "--glob",
        "levels/**",
        "--price",
        "4.99",
        "--title",
        "Unlock",
        "--feature",
        "All peaks",
        "--visible",
        "live",
        "--all-files",
    ]);
    let message = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        !message.contains("unexpected argument") && !message.contains("invalid value"),
        "the long aliases should parse cleanly: {message}"
    );
}

#[test]
fn paid_content_adds_no_new_single_letter_flags() {
    let help = stdout(&run(&["paid-content", "create", "--help"]));
    for line in help.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('-') {
            let short = rest.chars().next().unwrap_or(' ');
            assert!(
                short == '-' || matches!(short, 'c' | 'h'),
                "unexpected short flag in `create`: {line}"
            );
        }
    }
}

#[test]
fn the_identifier_is_positional_on_the_write_commands() {
    for args in [
        vec![
            "paid-content",
            "update",
            "full-version",
            "--game-id",
            "g",
            "--price",
            "1.99",
        ],
        vec![
            "paid-content",
            "deactivate",
            "full-version",
            "--game-id",
            "g",
            "--yes",
        ],
    ] {
        let sub = args[1];
        let out = run(&args);
        let message = format!("{}{}", stdout(&out), stderr(&out));
        assert!(
            !message.contains("unexpected argument") && !message.contains("required"),
            "`{sub} <CONTENT_IDENTIFIER>` should parse: {message}"
        );
    }
}

#[test]
fn the_document_id_is_the_alternative_not_an_addition() {
    let out = run(&[
        "paid-content",
        "update",
        "full-version",
        "--id",
        "jh349ta9xdfafxxx",
        "--game-id",
        "g",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("cannot be used with"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn naming_no_entry_at_all_is_refused() {
    let out = run(&[
        "paid-content",
        "update",
        "--game-id",
        "g",
        "--price",
        "1.99",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("CONTENT_IDENTIFIER") || stderr(&out).contains("--id"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn resolves_positional_names_an_entry_not_a_glob() {
    let out = run(&["paid-content", "resolve", "--game-id", "g", "full-version"]);
    let message = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        !message.contains("unexpected argument") && !message.contains("required"),
        "the positional should name an entry, as it does on update and deactivate: {message}"
    );
}

#[test]
fn allow_no_matches_is_offered_wherever_a_match_report_is_made() {
    for sub in ["create", "update", "resolve"] {
        let help = stdout(&run(&["paid-content", sub, "--help"]));
        assert!(help.contains("--allow-no-matches"), "{sub}:\n{help}");
    }
    let help = stdout(&run(&["paid-content", "deactivate", "--help"]));
    assert!(!help.contains("--allow-no-matches"), "{help}");
}

#[test]
fn a_blank_pattern_is_rejected_before_any_request() {
    let cases: [&[&str]; 3] = [
        &[
            "paid-content",
            "create",
            "--game-id",
            "g",
            "x",
            "--price",
            "1",
            "--title",
            "t",
            "--feature",
            "f",
            "--pattern",
            "",
        ],
        &[
            "paid-content",
            "update",
            "--game-id",
            "g",
            "x",
            "--pattern",
            "",
        ],
        &["paid-content", "resolve", "--game-id", "g", "--pattern", ""],
    ];
    for args in cases {
        let out = run(args);
        assert!(!out.status.success(), "{}: {}", args[1], stdout(&out));
        assert!(
            stderr(&out).contains("cannot be empty"),
            "{}: {}",
            args[1],
            stderr(&out)
        );
    }
}

#[test]
fn no_patterns_is_the_explicit_way_to_gate_nothing() {
    for sub in ["create", "update"] {
        let help = stdout(&run(&["paid-content", sub, "--help"]));
        assert!(help.contains("--no-patterns"), "{sub}:\n{help}");
    }
    for sub in ["resolve", "deactivate", "list"] {
        let help = stdout(&run(&["paid-content", sub, "--help"]));
        assert!(!help.contains("--no-patterns"), "{sub}:\n{help}");
    }

    let out = run(&[
        "paid-content",
        "update",
        "--game-id",
        "g",
        "x",
        "--no-patterns",
        "--pattern",
        "levels/**",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("cannot be used with"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn resolve_is_strict_by_default_and_offers_the_opt_out() {
    let help = stdout(&run(&["paid-content", "resolve", "--help"]));
    assert!(help.contains("--allow-no-matches"), "{help}");
    assert!(
        !help.contains("--strict"),
        "--strict was replaced by its inverse:\n{help}"
    );
}

#[test]
fn a_price_that_is_not_whole_cents_is_rejected_before_any_request() {
    let out = run(&[
        "paid-content",
        "create",
        "--game-id",
        "g",
        "full-version",
        "--pattern",
        "levels/**",
        "--price",
        "0.999",
        "--title",
        "t",
        "--feature",
        "f",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("at most 2 decimal places"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_price_outside_the_platform_range_is_left_to_the_server() {
    let out = run(&[
        "paid-content",
        "create",
        "--game-id",
        "g",
        "full-version",
        "--pattern",
        "levels/**",
        "--price",
        "500.00",
        "--title",
        "t",
        "--feature",
        "f",
    ]);
    assert!(
        !stderr(&out).contains("at most 2 decimal places"),
        "the price should have parsed: {}",
        stderr(&out)
    );
}

#[test]
fn create_requires_a_feature_and_a_gating_choice() {
    let base = [
        "paid-content",
        "create",
        "--game-id",
        "g",
        "full-version",
        "--price",
        "4.99",
        "--title",
        "t",
        "--pattern",
        "levels/**",
        "--feature",
        "f",
    ];

    let mut without_feature = base.to_vec();
    let at = without_feature
        .iter()
        .position(|a| *a == "--feature")
        .unwrap();
    without_feature.drain(at..at + 2);
    let out = run(&without_feature);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--feature"), "{}", stderr(&out));

    let mut without_pattern = base.to_vec();
    let at = without_pattern
        .iter()
        .position(|a| *a == "--pattern")
        .unwrap();
    without_pattern.drain(at..at + 2);
    let out = run(&without_pattern);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("--pattern") && err.contains("--no-patterns"),
        "create must ask for one of the two gating flags up front: {err}"
    );

    let mut with_no_patterns = without_pattern.clone();
    with_no_patterns.push("--no-patterns");
    let message = {
        let out = run(&with_no_patterns);
        format!("{}{}", stdout(&out), stderr(&out))
    };
    assert!(
        !message.contains("required") && !message.contains("unexpected"),
        "paid content that gates no files is a supported state via --no-patterns: {message}"
    );

    let mut twice = base.to_vec();
    twice.extend(["--pattern", "audio/**"]);
    let message = {
        let out = run(&twice);
        format!("{}{}", stdout(&out), stderr(&out))
    };
    assert!(
        !message.contains("cannot be used") && !message.contains("unexpected"),
        "repeating --pattern must still be allowed: {message}"
    );
}

#[test]
fn resolve_requires_something_to_resolve() {
    let out = run(&["paid-content", "resolve", "--game-id", "g"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("CONTENT_IDENTIFIER") || stderr(&out).contains("--pattern"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn resolve_refuses_a_pattern_and_an_entry_together() {
    let out = run(&[
        "paid-content",
        "resolve",
        "--game-id",
        "g",
        "full-version",
        "--glob",
        "levels/**",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("cannot be used with") || stderr(&out).contains("conflict"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn visibility_only_accepts_the_settable_values() {
    let out = run(&[
        "paid-content",
        "create",
        "--game-id",
        "g",
        "full-version",
        "--pattern",
        "levels/**",
        "--price",
        "4.99",
        "--title",
        "t",
        "--feature",
        "f",
        "--visibility",
        "inactive",
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("playtest") && stderr(&out).contains("live"),
        "the error should list the accepted values: {}",
        stderr(&out)
    );
}

#[test]
fn deactivate_refuses_to_guess_when_it_cannot_prompt() {
    let out = command(&[
        "paid-content",
        "deactivate",
        "--game-id",
        "g",
        "--id",
        "pc_1",
    ])
    .env("WAVEDASH_TOKEN", "not-a-real-key")
    .env("CI", "1")
    .output()
    .expect("the wavedash binary should run");
    assert!(!out.status.success());
    let message = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        message.contains("--yes") || message.contains("confirmation"),
        "a destructive command must not proceed unprompted: {message}"
    );
}

fn live_env() -> (String, String) {
    let token = std::env::var("WAVEDASH_TOKEN").unwrap_or_default();
    let game_id = std::env::var("WAVEDASH_GAME_ID").unwrap_or_default();
    assert!(
        !token.is_empty() && !game_id.is_empty(),
        "lifecycle tests need WAVEDASH_TOKEN and WAVEDASH_GAME_ID set"
    );
    (token, game_id)
}

fn run_live(token: &str, game_id: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("WAVEDASH_TOKEN", token)
        .env("WAVEDASH_GAME_ID", game_id)
        .env("CI", "1")
        .output()
        .expect("the wavedash binary should run")
}

fn unique_identifier() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be after the epoch")
        .as_nanos();
    format!("it-{}", nanos % 1_000_000_000)
}

#[test]
#[ignore = "needs a live API: cargo test --test paid_content -- --ignored"]
fn create_list_resolve_update_deactivate_round_trip() {
    let (token, game_id) = live_env();
    let identifier = unique_identifier();

    let created = run_live(
        &token,
        &game_id,
        &[
            "paid-content",
            "create",
            &identifier,
            "--pattern",
            "levels/**",
            "--price",
            "4.99",
            "--title",
            "Unlock all content",
            "--feature",
            "All twelve alpine peaks",
            "--visibility",
            "playtest",
            "--allow-no-matches",
        ],
    );
    assert!(
        created.status.success(),
        "create failed: {}",
        stderr(&created)
    );
    let created_out = stdout(&created);
    assert!(created_out.contains(&identifier), "{created_out}");
    assert!(
        created_out.contains("gated") || created_out.contains("No completed build"),
        "create should print the match report: {created_out}"
    );

    let listed = run_live(&token, &game_id, &["paid-content", "list", "--json"]);
    assert!(listed.status.success(), "list failed: {}", stderr(&listed));
    let rows: serde_json::Value =
        serde_json::from_str(&stdout(&listed)).expect("--json should emit valid JSON");
    let row = rows
        .as_array()
        .expect("the listing should be an array")
        .iter()
        .find(|r| r["contentIdentifier"] == identifier.as_str())
        .expect("the created entry should appear in the listing");
    let id = row["_id"].as_str().expect("_id should be a string");
    assert_eq!(row["priceCents"], 499);
    assert_eq!(row["visibility"], "playtest");

    let matched = run_live(
        &token,
        &game_id,
        &[
            "paid-content",
            "resolve",
            "--id",
            id,
            "--all-files",
            "--allow-no-matches",
        ],
    );
    assert!(
        matched.status.success(),
        "match failed: {}",
        stderr(&matched)
    );
    assert!(
        stdout(&matched).contains("\"levels/**\""),
        "match must echo the stored pattern verbatim: {}",
        stdout(&matched)
    );

    let updated = run_live(
        &token,
        &game_id,
        &[
            "paid-content",
            "update",
            &identifier,
            "--title",
            "Unlock everything",
        ],
    );
    assert!(
        updated.status.success(),
        "update with only --title should not require --visibility: {}",
        stderr(&updated)
    );

    let cleared = run_live(
        &token,
        &game_id,
        &["paid-content", "update", &identifier, "--no-patterns"],
    );
    assert!(
        cleared.status.success(),
        "--no-patterns should clear the list without --allow-no-matches: {}",
        stderr(&cleared)
    );

    let deactivated = run_live(
        &token,
        &game_id,
        &["paid-content", "deactivate", &identifier, "--yes"],
    );
    assert!(
        deactivated.status.success(),
        "deactivate failed: {}",
        stderr(&deactivated)
    );

    let after = run_live(&token, &game_id, &["paid-content", "list", "--json"]);
    let rows: serde_json::Value =
        serde_json::from_str(&stdout(&after)).expect("--json should emit valid JSON");
    let row = rows
        .as_array()
        .expect("the listing should be an array")
        .iter()
        .find(|r| r["_id"] == id)
        .expect("a deactivated entry is retired, not deleted, so it still lists");
    assert_eq!(
        row["visibility"], "inactive",
        "deactivate should leave the row inactive rather than removing it"
    );
}

#[test]
#[ignore = "needs a live API: cargo test --test paid_content -- --ignored"]
fn resolve_reports_a_pattern_that_gates_nothing() {
    let (token, game_id) = live_env();

    let out = run_live(
        &token,
        &game_id,
        &[
            "paid-content",
            "resolve",
            "--glob",
            "definitely/not/a/real/path/*.xyz",
            "--allow-no-matches",
        ],
    );
    assert!(out.status.success(), "match failed: {}", stderr(&out));
    assert!(
        stdout(&out).contains("matches nothing"),
        "a zero-match pattern should be called out: {}",
        stdout(&out)
    );

    let strict = run_live(
        &token,
        &game_id,
        &[
            "paid-content",
            "resolve",
            "--glob",
            "definitely/not/a/real/path/*.xyz",
        ],
    );
    assert!(
        !strict.status.success(),
        "a zero-match pattern is fatal unless --allow-no-matches is passed"
    );
}

#[test]
#[ignore = "needs a live API: cargo test --test paid_content -- --ignored"]
fn the_server_owns_the_price_range() {
    let (token, game_id) = live_env();
    let identifier = unique_identifier();

    let out = run_live(
        &token,
        &game_id,
        &[
            "paid-content",
            "create",
            &identifier,
            "--pattern",
            "levels/**",
            "--price",
            "5000.00",
            "--title",
            "Too expensive",
            "--feature",
            "f",
            "--allow-no-matches",
        ],
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).to_lowercase().contains("price"),
        "the server's own price error should reach the user: {}",
        stderr(&out)
    );
}
