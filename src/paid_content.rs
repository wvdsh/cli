use crate::auth::require_api_key;
use crate::builds::uploader::format_bytes;
use crate::config;
use anyhow::Result;
use colored::Colorize;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};
use reqwest::{Method, RequestBuilder};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

const FILE_LISTING_LIMIT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Visibility {
    Playtest,
    Live,
}

impl Visibility {
    fn as_wire(self) -> &'static str {
        match self {
            Visibility::Playtest => "playtest",
            Visibility::Live => "live",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PaidContentRef<'a> {
    Id(&'a str),
    ContentIdentifier(&'a str),
}

impl PaidContentRef<'_> {
    fn path_segment(&self) -> String {
        match self {
            PaidContentRef::Id(id) => format!("/{}", id),
            PaidContentRef::ContentIdentifier(identifier) => {
                format!("/by-identifier/{}", urlencoding::encode(identifier))
            }
        }
    }

    fn value(&self) -> &str {
        match self {
            PaidContentRef::Id(id) => id,
            PaidContentRef::ContentIdentifier(identifier) => identifier,
        }
    }

    fn matches(&self, entry: &PaidContent) -> bool {
        match self {
            PaidContentRef::Id(id) => entry._id == *id,
            PaidContentRef::ContentIdentifier(identifier) => {
                entry.content_identifier == *identifier
            }
        }
    }

    fn not_found_error(&self) -> anyhow::Error {
        match self {
            PaidContentRef::Id(id) => {
                anyhow::anyhow!("No paid content with id \"{}\" in this game", id)
            }
            PaidContentRef::ContentIdentifier(identifier) => anyhow::anyhow!(
                "No paid content with contentIdentifier \"{}\" in this game",
                identifier
            ),
        }
    }
}

fn request(api_key: &str, method: Method, game_id: &str, path: &str) -> Result<RequestBuilder> {
    let url = format!(
        "{}/api/games/{}/paid-content{}",
        config::get("api_host")?,
        game_id,
        path
    );
    Ok(config::create_http_client()?
        .request(method, url)
        .header("Authorization", format!("Bearer {}", api_key)))
}

#[derive(Debug, Deserialize, Serialize)]
struct PaidContent {
    _id: String,
    #[serde(rename = "contentIdentifier")]
    content_identifier: String,
    #[serde(rename = "pathPatterns")]
    path_patterns: Vec<String>,
    visibility: String,
    #[serde(rename = "priceCents")]
    price_cents: i64,
    title: String,
    #[serde(default, deserialize_with = "null_to_default")]
    message: String,
    #[serde(default, deserialize_with = "null_to_default")]
    features: Vec<String>,
    #[serde(default, rename = "buttonLabel", deserialize_with = "null_to_default")]
    button_label: String,
}

#[derive(Debug, Deserialize)]
struct PaidContentListResponse {
    #[serde(rename = "paidContent")]
    paid_content: Vec<PaidContent>,
}

#[derive(Debug, Default, Deserialize)]
struct DeactivateResponse {
    #[serde(rename = "wasActive", default)]
    was_active: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
struct BuildRef {
    #[serde(rename = "buildNumber")]
    build_number: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PatternMatch {
    pattern: String,
    #[serde(rename = "matchCount")]
    match_count: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct MatchedFile {
    path: String,
    #[serde(rename = "sizeBytes")]
    size_bytes: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct MatchReport {
    build: Option<BuildRef>,
    #[serde(rename = "totalFiles")]
    total_files: u64,
    #[serde(rename = "gatedFiles")]
    gated_files: u64,
    #[serde(rename = "gatedSizeBytes")]
    gated_size_bytes: u64,
    #[serde(rename = "perPattern", default, deserialize_with = "null_to_default")]
    per_pattern: Vec<PatternMatch>,
    #[serde(
        rename = "zeroMatchPatterns",
        default,
        deserialize_with = "null_to_default"
    )]
    zero_match_patterns: Vec<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    matched: Vec<MatchedFile>,
}

fn null_to_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::deserialize(d)?.unwrap_or_default())
}

/// Swallows a malformed report so it cannot fail a write that already landed.
fn report_or_none<'de, D: Deserializer<'de>>(d: D) -> Result<Option<MatchReport>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

#[derive(Debug, Deserialize)]
struct CreatedPaidContent {
    _id: String,
    #[serde(rename = "contentIdentifier")]
    content_identifier: String,
    #[serde(rename = "matchReport", default, deserialize_with = "report_or_none")]
    match_report: Option<MatchReport>,
}

#[derive(Debug, Deserialize)]
struct UpdatedPaidContent {
    #[serde(rename = "matchReport", default, deserialize_with = "report_or_none")]
    match_report: Option<MatchReport>,
}

/// Shape only: a range compiled in here would reject prices the server accepts
/// as soon as its limits move.
pub fn parse_price_dollars(input: &str) -> Result<i64, String> {
    let raw = input.trim().trim_start_matches('$').trim();
    let invalid = || {
        format!(
            "invalid price \"{}\" — expected a dollar amount with at most 2 decimal places, e.g. 4.99",
            input
        )
    };

    let (whole, frac) = match raw.split_once('.') {
        Some((whole, frac)) => (whole, frac),
        None => (raw, ""),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    if frac.len() > 2 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }

    let dollars: i64 = whole.parse().map_err(|_| invalid())?;
    let cents: i64 = match frac.len() {
        0 => 0,
        1 => frac.parse::<i64>().map_err(|_| invalid())? * 10,
        _ => frac.parse().map_err(|_| invalid())?,
    };
    dollars
        .checked_mul(100)
        .and_then(|d| d.checked_add(cents))
        .ok_or_else(invalid)
}

fn format_price(price_cents: i64) -> String {
    format!("${}.{:02}", price_cents / 100, price_cents % 100)
}

fn files_noun(count: u64) -> &'static str {
    if count == 1 {
        "file"
    } else {
        "files"
    }
}

fn render_report(report: &MatchReport, all_files: bool) -> String {
    let mut out = String::new();

    let Some(build) = &report.build else {
        out.push_str("  No completed build yet — push one to see what these patterns gate.\n");
        return out;
    };

    let build_label = match build.build_number {
        Some(number) => format!("Build #{}", number),
        None => "latest build".to_string(),
    };
    out.push_str(&format!(
        "\n  {} ({} {} indexed)\n\n",
        build_label.bold(),
        report.total_files,
        files_noun(report.total_files)
    ));

    let quoted: Vec<String> = report
        .per_pattern
        .iter()
        .map(|entry| format!("\"{}\"", entry.pattern))
        .collect();
    let width = quoted.iter().map(|q| q.chars().count()).max().unwrap_or(0);
    for (entry, quoted) in report.per_pattern.iter().zip(&quoted) {
        let warning = if entry.match_count == 0 {
            format!("   {}", "matches nothing".yellow())
        } else {
            String::new()
        };
        out.push_str(&format!(
            "    {:width$}  {} {}{}\n",
            quoted,
            entry.match_count,
            files_noun(entry.match_count),
            warning,
            width = width
        ));
    }

    out.push_str(&format!(
        "\n  {}, {}\n",
        format!(
            "{} of {} {} gated",
            report.gated_files,
            report.total_files,
            files_noun(report.total_files)
        )
        .bold(),
        format_bytes(report.gated_size_bytes)
    ));

    if !report.matched.is_empty() {
        out.push('\n');
        let limit = if all_files {
            report.matched.len()
        } else {
            FILE_LISTING_LIMIT.min(report.matched.len())
        };
        let listed = &report.matched[..limit];
        let path_width = listed
            .iter()
            .map(|file| file.path.chars().count())
            .max()
            .unwrap_or(0);
        for file in listed {
            out.push_str(&format!(
                "    {:path_width$}  {}\n",
                file.path,
                format_bytes(file.size_bytes),
                path_width = path_width
            ));
        }

        let capped = report.matched.len() - listed.len();
        if capped > 0 {
            out.push_str(&format!(
                "    {}\n",
                format!("… and {} more — pass --all-files to list them", capped).dimmed()
            ));
        }
    }

    out
}

fn print_report(report: Option<&MatchReport>, all_files: bool) {
    if let Some(report) = report {
        print!("{}", render_report(report, all_files));
    }
}

async fn fetch_paid_content(api_key: &str, game_id: &str) -> Result<Vec<PaidContent>> {
    let resp = request(api_key, Method::GET, game_id, "")?.send().await?;
    let resp = config::check_api_response(resp).await?;
    let data: PaidContentListResponse = resp.json().await?;
    Ok(data.paid_content)
}

pub async fn handle_paid_content_list(game_id: &str, json_output: bool) -> Result<()> {
    let api_key = require_api_key()?;
    let paid_content = fetch_paid_content(&api_key, game_id).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&paid_content)?);
        return Ok(());
    }

    if paid_content.is_empty() {
        println!("No paid content found.");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("ID"),
            Cell::new("Identifier"),
            Cell::new("Price"),
            Cell::new("Visibility"),
            Cell::new("Title"),
            Cell::new("Patterns"),
        ]);

    for entry in paid_content {
        table.add_row(vec![
            entry._id,
            entry.content_identifier,
            format_price(entry.price_cents),
            entry.visibility,
            entry.title,
            entry.path_patterns.join("\n"),
        ]);
    }

    println!("{table}");
    Ok(())
}

pub struct CreatePaidContentArgs<'a> {
    pub game_id: &'a str,
    pub content_identifier: &'a str,
    pub patterns: &'a [String],
    pub price_cents: i64,
    pub title: &'a str,
    pub message: Option<&'a str>,
    pub features: &'a [String],
    pub button_label: &'a str,
    pub visibility: Visibility,
    pub all_files: bool,
}

pub async fn handle_paid_content_create(args: CreatePaidContentArgs<'_>) -> Result<()> {
    let api_key = require_api_key()?;
    let body = json!({
        "contentIdentifier": args.content_identifier,
        "pathPatterns": args.patterns,
        "priceCents": args.price_cents,
        "title": args.title,
        "message": args.message.unwrap_or_default(),
        "features": args.features,
        "buttonLabel": args.button_label,
        "visibility": args.visibility.as_wire(),
    });

    let resp = request(&api_key, Method::POST, args.game_id, "")?
        .json(&body)
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let created: CreatedPaidContent = resp.json().await?;

    println!(
        "✓ Created paid content \"{}\" (id: {})",
        created.content_identifier, created._id
    );
    print_report(created.match_report.as_ref(), args.all_files);
    Ok(())
}

pub struct UpdatePaidContentArgs<'a> {
    pub game_id: &'a str,
    pub reference: PaidContentRef<'a>,
    pub patterns: Option<&'a [String]>,
    pub price_cents: Option<i64>,
    pub title: Option<&'a str>,
    pub message: Option<&'a str>,
    pub features: Option<&'a [String]>,
    pub button_label: Option<&'a str>,
    pub visibility: Option<Visibility>,
    pub all_files: bool,
}

pub async fn handle_paid_content_update(args: UpdatePaidContentArgs<'_>) -> Result<()> {
    let api_key = require_api_key()?;

    let mut body = serde_json::Map::new();
    if let Some(patterns) = args.patterns {
        body.insert("pathPatterns".into(), json!(patterns));
    }
    if let Some(price_cents) = args.price_cents {
        body.insert("priceCents".into(), json!(price_cents));
    }
    if let Some(title) = args.title {
        body.insert("title".into(), json!(title));
    }
    if let Some(message) = args.message {
        body.insert("message".into(), json!(message));
    }
    if let Some(features) = args.features {
        body.insert("features".into(), json!(features));
    }
    if let Some(button_label) = args.button_label {
        body.insert("buttonLabel".into(), json!(button_label));
    }
    if let Some(visibility) = args.visibility {
        body.insert("visibility".into(), json!(visibility.as_wire()));
    }

    if body.is_empty() {
        anyhow::bail!("No fields provided to update.");
    }

    let resp = request(
        &api_key,
        Method::PATCH,
        args.game_id,
        &args.reference.path_segment(),
    )?
    .json(&serde_json::Value::Object(body))
    .send()
    .await?;

    let resp = config::check_api_response(resp).await?;
    let updated: UpdatedPaidContent = resp.json().await?;

    println!("✓ Updated paid content {}", args.reference.value());
    print_report(updated.match_report.as_ref(), args.all_files);
    Ok(())
}

pub async fn handle_paid_content_deactivate(
    game_id: &str,
    reference: PaidContentRef<'_>,
    yes: bool,
) -> Result<()> {
    let api_key = require_api_key()?;

    if !yes {
        if crate::is_non_interactive() {
            anyhow::bail!(
                "Refusing to deactivate paid content without confirmation.\n\
                 Re-run with --yes (alias --force / -y) to proceed non-interactively."
            );
        }

        let entries = fetch_paid_content(&api_key, game_id).await?;
        let Some(entry) = entries.iter().find(|entry| reference.matches(entry)) else {
            return Err(reference.not_found_error());
        };
        if entry.visibility == INACTIVE {
            println!("{}", already_inactive_line(reference));
            return Ok(());
        }

        println!("{}", deactivate_warning(entry));
        let confirmed = cliclack::confirm("Are you sure you want to continue?")
            .initial_value(false)
            .interact()?;
        if !confirmed {
            println!("Aborted. Nothing was changed.");
            return Ok(());
        }
    }

    let resp = request(&api_key, Method::DELETE, game_id, &reference.path_segment())?
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let outcome: DeactivateResponse = resp.json().await.unwrap_or_default();
    println!("{}", deactivate_outcome_line(reference, outcome.was_active));
    Ok(())
}

const INACTIVE: &str = "inactive";

fn deactivate_warning(entry: &PaidContent) -> String {
    format!(
        "{} This stops offering paid content {} ({}, {}, currently {}) and cancels any price drop on it.\n\
         Players who already bought it keep access.",
        "Warning:".yellow().bold(),
        entry.content_identifier.bold(),
        entry.title,
        format_price(entry.price_cents),
        entry.visibility
    )
}

fn already_inactive_line(reference: PaidContentRef<'_>) -> String {
    format!(
        "Paid content {} is already inactive. Nothing changed.",
        reference.value()
    )
}

fn deactivate_outcome_line(reference: PaidContentRef<'_>, was_active: Option<bool>) -> String {
    match was_active {
        Some(false) => already_inactive_line(reference),
        _ => format!("✓ Deactivated paid content {}", reference.value()),
    }
}

pub struct ResolvePaidContentArgs<'a> {
    pub game_id: &'a str,
    pub patterns: &'a [String],
    pub reference: Option<PaidContentRef<'a>>,
    pub all_files: bool,
    pub json_output: bool,
    pub strict: bool,
}

pub async fn handle_paid_content_resolve(args: ResolvePaidContentArgs<'_>) -> Result<()> {
    let api_key = require_api_key()?;
    let body = match args.reference {
        Some(PaidContentRef::Id(id)) => json!({ "paidContentId": id }),
        Some(PaidContentRef::ContentIdentifier(identifier)) => {
            json!({ "contentIdentifier": identifier })
        }
        None => json!({ "pathPatterns": args.patterns }),
    };

    let resp = request(&api_key, Method::POST, args.game_id, "/match-preview")?
        .json(&body)
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let report: MatchReport = resp.json().await?;

    if args.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render_report(&report, args.all_files));
    }

    if args.strict {
        if report.build.is_none() {
            anyhow::bail!(
                "No completed build to check these patterns against. Push a build first."
            );
        }

        if !report.zero_match_patterns.is_empty() {
            anyhow::bail!(
                "{} pattern(s) matched no files: {}",
                report.zero_match_patterns.len(),
                report
                    .zero_match_patterns
                    .iter()
                    .map(|pattern| format!("\"{}\"", pattern))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with(matched: usize, per_pattern: Vec<(&str, u64)>) -> MatchReport {
        MatchReport {
            build: Some(BuildRef {
                build_number: Some(7),
            }),
            total_files: 13,
            gated_files: matched as u64,
            gated_size_bytes: 13_002_342,
            per_pattern: per_pattern
                .into_iter()
                .map(|(pattern, match_count)| PatternMatch {
                    pattern: pattern.to_string(),
                    match_count,
                })
                .collect(),
            zero_match_patterns: vec![],
            matched: (0..matched)
                .map(|i| MatchedFile {
                    path: format!("levels/bonus/file{}.dat", i),
                    size_bytes: 2_100_000,
                })
                .collect(),
        }
    }

    #[test]
    fn parses_plain_and_dollar_prefixed_prices() {
        assert_eq!(parse_price_dollars("0.99"), Ok(99));
        assert_eq!(parse_price_dollars("$0.99"), Ok(99));
        assert_eq!(parse_price_dollars(" 4.99 "), Ok(499));
        assert_eq!(parse_price_dollars("12"), Ok(1200));
        assert_eq!(parse_price_dollars("12.5"), Ok(1250));
        assert_eq!(parse_price_dollars("0"), Ok(0));
    }

    #[test]
    fn rejects_prices_that_are_not_whole_cents() {
        for bad in ["abc", "0.999", "1.2.3", "", "-1.00", "1,99", "1.9x"] {
            assert!(
                parse_price_dollars(bad).is_err(),
                "{bad} should not parse as a price"
            );
        }
    }

    #[test]
    fn accepts_prices_outside_the_platform_range() {
        assert_eq!(parse_price_dollars("0.01"), Ok(1));
        assert_eq!(parse_price_dollars("50000.00"), Ok(5_000_000));
    }

    #[test]
    fn formats_prices_with_two_decimal_places() {
        assert_eq!(format_price(99), "$0.99");
        assert_eq!(format_price(499), "$4.99");
        assert_eq!(format_price(1000), "$10.00");
    }

    #[test]
    fn visibility_goes_over_the_wire_as_a_readable_string() {
        assert_eq!(Visibility::Playtest.as_wire(), "playtest");
        assert_eq!(Visibility::Live.as_wire(), "live");
    }

    #[test]
    fn a_reference_owns_its_path_segment_and_escapes_the_identifier() {
        assert_eq!(PaidContentRef::Id("pc_1").path_segment(), "/pc_1");
        assert_eq!(
            PaidContentRef::ContentIdentifier("full-version").path_segment(),
            "/by-identifier/full-version"
        );
        assert_eq!(
            PaidContentRef::ContentIdentifier("dlc/level-2").path_segment(),
            "/by-identifier/dlc%2Flevel-2"
        );
        assert_eq!(
            PaidContentRef::ContentIdentifier("../clouds").path_segment(),
            "/by-identifier/..%2Fclouds"
        );
    }

    #[test]
    fn report_echoes_every_pattern_verbatim() {
        let report = report_with(4, vec![("levels/bonus/**", 4)]);
        let rendered = render_report(&report, false);
        assert!(
            rendered.contains("\"levels/bonus/**\""),
            "the pattern must be echoed back so shell expansion is visible: {rendered}"
        );
        assert!(rendered.contains("4 of 13 files gated"));
    }

    #[test]
    fn a_short_listing_prints_without_being_asked() {
        let report = report_with(4, vec![("levels/bonus/**", 4)]);
        let rendered = render_report(&report, false);
        assert!(rendered.contains("levels/bonus/file0.dat"), "{rendered}");
        assert!(rendered.contains("levels/bonus/file3.dat"), "{rendered}");
        assert!(!rendered.contains("… and"), "{rendered}");
    }

    #[test]
    fn a_long_listing_caps_and_points_at_the_flag_that_lifts_it() {
        let report = report_with(60, vec![("levels/**", 60)]);
        let rendered = render_report(&report, false);
        assert!(rendered.contains("levels/bonus/file9.dat"), "{rendered}");
        assert!(!rendered.contains("levels/bonus/file10.dat"), "{rendered}");
        assert!(
            rendered.contains("… and 50 more — pass --all-files to list them"),
            "{rendered}"
        );
    }

    #[test]
    fn report_flags_a_pattern_that_matches_nothing() {
        let report = report_with(0, vec![("art/cutscenes/*.webm", 0)]);
        let rendered = render_report(&report, false);
        assert!(rendered.contains("matches nothing"), "{rendered}");
    }

    #[test]
    fn every_matched_path_is_listed() {
        let report = report_with(60, vec![("levels/**", 60)]);
        let rendered = render_report(&report, true);
        assert!(rendered.contains("levels/bonus/file0.dat"));
        assert!(rendered.contains("levels/bonus/file59.dat"));
        assert!(!rendered.contains("… and"));
    }

    #[test]
    fn report_handles_a_game_with_no_completed_build() {
        let mut report = report_with(0, vec![("levels/**", 0)]);
        report.build = None;
        let rendered = render_report(&report, true);
        assert!(rendered.contains("No completed build yet"), "{rendered}");
    }

    fn entry(identifier: &str, visibility: &str) -> PaidContent {
        PaidContent {
            _id: format!("id-{identifier}"),
            content_identifier: identifier.to_string(),
            path_patterns: vec!["levels/**".to_string()],
            visibility: visibility.to_string(),
            price_cents: 499,
            title: "Unlock all content".to_string(),
            message: String::new(),
            features: vec![],
            button_label: "Unlock".to_string(),
        }
    }

    #[test]
    fn a_reference_finds_its_entry_by_identifier_or_id() {
        let entries = vec![entry("full-version", "live"), entry("bonus", "playtest")];
        let by_identifier = PaidContentRef::ContentIdentifier("bonus");
        let by_id = PaidContentRef::Id("id-full-version");
        let missing = PaidContentRef::ContentIdentifier("id-full-version");
        assert_eq!(
            entries
                .iter()
                .find(|e| by_identifier.matches(e))
                .map(|e| e._id.as_str()),
            Some("id-bonus")
        );
        assert_eq!(
            entries
                .iter()
                .find(|e| by_id.matches(e))
                .map(|e| e.content_identifier.as_str()),
            Some("full-version")
        );
        assert!(entries.iter().all(|e| !missing.matches(e)));
    }

    #[test]
    fn the_warning_names_what_is_about_to_be_deactivated() {
        let rendered = deactivate_warning(&entry("full-version", "live"));
        assert!(rendered.contains("full-version"), "{rendered}");
        assert!(rendered.contains("Unlock all content"), "{rendered}");
        assert!(rendered.contains("$4.99"), "{rendered}");
        assert!(rendered.contains("currently live"), "{rendered}");
        assert!(rendered.contains("keep access"), "{rendered}");
    }

    #[test]
    fn a_second_deactivation_is_reported_as_a_no_op() {
        let reference = PaidContentRef::ContentIdentifier("full-version");
        assert!(deactivate_outcome_line(reference, Some(false)).contains("already inactive"));
        assert!(deactivate_outcome_line(reference, Some(true)).contains("✓ Deactivated"));
    }

    #[test]
    fn an_older_server_that_says_nothing_about_was_active_still_reads_as_deactivated() {
        let reference = PaidContentRef::Id("pc_1");
        let outcome: DeactivateResponse =
            serde_json::from_value(json!({ "success": true })).expect("legacy body parses");
        assert_eq!(outcome.was_active, None);
        assert!(deactivate_outcome_line(reference, outcome.was_active).contains("✓ Deactivated"));

        let outcome: DeactivateResponse =
            serde_json::from_value(json!({ "success": true, "wasActive": false })).unwrap();
        assert_eq!(outcome.was_active, Some(false));
    }

    #[test]
    fn a_missing_reference_is_described_the_way_the_server_would() {
        let by_identifier = PaidContentRef::ContentIdentifier("ghost").not_found_error();
        assert!(by_identifier
            .to_string()
            .contains("contentIdentifier \"ghost\""));
        let by_id = PaidContentRef::Id("pc_9").not_found_error();
        assert!(by_id.to_string().contains("id \"pc_9\""));
    }

    #[test]
    fn parses_the_paid_content_list_response() {
        let response: PaidContentListResponse = serde_json::from_value(json!({
            "paidContent": [{
                "_id": "paid-content-id",
                "contentIdentifier": "full-version",
                "pathPatterns": ["levels/bonus/**"],
                "visibility": "playtest",
                "priceCents": 499,
                "title": "Unlock all content"
            }]
        }))
        .expect("the API response should deserialize");

        let entry = &response.paid_content[0];
        assert_eq!(entry.content_identifier, "full-version");
        assert_eq!(entry.visibility, "playtest");
        assert_eq!(entry.price_cents, 499);
    }

    #[test]
    fn a_null_or_absent_collection_does_not_lose_the_whole_listing() {
        let response: PaidContentListResponse = serde_json::from_value(json!({
            "paidContent": [{
                "_id": "paid-content-id",
                "contentIdentifier": "full-version",
                "pathPatterns": ["levels/**"],
                "visibility": "live",
                "priceCents": 499,
                "title": "Unlock",
                "message": null,
                "features": null
            }]
        }))
        .expect("a null field must not abort the whole listing");

        let entry = &response.paid_content[0];
        assert_eq!(entry.message, "");
        assert!(entry.features.is_empty());
        assert_eq!(entry.button_label, "");
    }

    #[test]
    fn an_empty_report_collection_may_be_omitted_or_null() {
        let report: MatchReport = serde_json::from_value(json!({
            "build": { "buildNumber": 7 },
            "totalFiles": 13,
            "gatedFiles": 0,
            "gatedSizeBytes": 0,
            "perPattern": [],
            "zeroMatchPatterns": null
        }))
        .expect("an omitted or null collection should not void the report");

        assert!(report.zero_match_patterns.is_empty());
        assert!(report.matched.is_empty());
    }

    #[test]
    fn a_report_that_will_not_parse_does_not_fail_a_create_that_landed() {
        let created: CreatedPaidContent = serde_json::from_value(json!({
            "_id": "paid-content-id",
            "contentIdentifier": "full-version",
            "matchReport": { "totalFiles": 13 }
        }))
        .expect("a trimmed report must not fail the response");

        assert_eq!(created.content_identifier, "full-version");
        assert!(created.match_report.is_none());
    }

    #[test]
    fn the_summary_pluralises_on_the_total_it_follows() {
        let mut report = report_with(1, vec![("levels/boss.dat", 1)]);
        report.total_files = 1;
        let rendered = render_report(&report, false);
        assert!(rendered.contains("1 file indexed"), "{rendered}");
        assert!(rendered.contains("1 of 1 file gated"), "{rendered}");
    }

    #[test]
    fn parses_a_create_response_whose_report_is_absent() {
        let created: CreatedPaidContent = serde_json::from_value(json!({
            "_id": "paid-content-id",
            "contentIdentifier": "full-version",
            "matchReport": null
        }))
        .expect("a create response without a report should deserialize");

        assert_eq!(created._id, "paid-content-id");
        assert!(created.match_report.is_none());
    }

    #[test]
    fn json_output_keeps_the_api_field_names() {
        let entry = PaidContent {
            _id: "paid-content-id".to_string(),
            content_identifier: "full-version".to_string(),
            path_patterns: vec!["levels/bonus/**".to_string()],
            visibility: "playtest".to_string(),
            price_cents: 499,
            title: "Unlock all content".to_string(),
            message: "The Ski Force is counting on you.".to_string(),
            features: vec!["All twelve alpine peaks".to_string()],
            button_label: "Unlock".to_string(),
        };

        let json = serde_json::to_value(&entry).expect("the row should serialize");
        assert_eq!(json["_id"], "paid-content-id");
        assert_eq!(json["contentIdentifier"], "full-version");
        assert_eq!(json["pathPatterns"][0], "levels/bonus/**");
        assert_eq!(json["priceCents"], 499);
        assert_eq!(json["visibility"], "playtest");
        assert_eq!(json["title"], "Unlock all content");
        assert_eq!(json["features"][0], "All twelve alpine peaks");
        assert_eq!(json["buttonLabel"], "Unlock");

        let mut keys: Vec<&str> = json
            .as_object()
            .expect("a row is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "_id",
                "buttonLabel",
                "contentIdentifier",
                "features",
                "message",
                "pathPatterns",
                "priceCents",
                "title",
                "visibility"
            ],
            "update replaces features wholesale, so --json has to carry them"
        );
    }
}
