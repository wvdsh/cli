use crate::auth::require_api_key;
use crate::builds::uploader::format_bytes;
use crate::config;
use anyhow::Result;
use colored::Colorize;
use comfy_table::modifiers::UTF8_ROUND_CORNERS;
use comfy_table::presets::UTF8_FULL;
use comfy_table::{Cell, ContentArrangement, Table};
use serde::{Deserialize, Serialize};
use serde_json::json;

const FILE_LISTING_LIMIT: usize = 50;

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
            PaidContentRef::Id(id) => (*id).to_string(),
            PaidContentRef::ContentIdentifier(identifier) => {
                format!("by-identifier/{}", identifier)
            }
        }
    }

    fn value(&self) -> &str {
        match self {
            PaidContentRef::Id(id) => id,
            PaidContentRef::ContentIdentifier(identifier) => identifier,
        }
    }
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
}

#[derive(Debug, Deserialize)]
struct PaidContentListResponse {
    #[serde(rename = "paidContent")]
    paid_content: Vec<PaidContent>,
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
    truncated: bool,
    #[serde(rename = "perPattern")]
    per_pattern: Vec<PatternMatch>,
    #[serde(rename = "zeroMatchPatterns")]
    zero_match_patterns: Vec<String>,
    matched: Vec<MatchedFile>,
}

#[derive(Debug, Deserialize)]
struct CreatedPaidContent {
    _id: String,
    #[serde(rename = "contentIdentifier")]
    content_identifier: String,
    #[serde(rename = "matchReport")]
    match_report: Option<MatchReport>,
}

#[derive(Debug, Deserialize)]
struct UpdatedPaidContent {
    #[serde(rename = "matchReport")]
    match_report: Option<MatchReport>,
}

/// Validates shape, not the platform's price range: a bound compiled in here
/// would reject prices the server accepts as soon as its limits move.
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

fn render_report(report: &MatchReport, show_files: bool, no_limit: bool) -> String {
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
        "\n  {} ({} files indexed{})\n\n",
        build_label.bold(),
        report.total_files,
        if report.truncated {
            "; indexing limit reached, counts are a floor"
        } else {
            ""
        }
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
            files_noun(report.gated_files)
        )
        .bold(),
        format_bytes(report.gated_size_bytes)
    ));

    if (show_files || no_limit) && !report.matched.is_empty() {
        out.push('\n');
        let limit = if no_limit {
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
        let omitted = report.matched.len() - limit;
        if omitted > 0 {
            out.push_str(&format!(
                "    {}\n",
                format!(
                    "… and {} more — pass --show-files-no-limit to list them",
                    omitted
                )
                .dimmed()
            ));
        }
    }

    out
}

fn print_report(report: Option<&MatchReport>, show_files: bool, no_limit: bool) {
    if let Some(report) = report {
        print!("{}", render_report(report, show_files, no_limit));
    }
}

pub async fn handle_paid_content_list(game_id: &str, json_output: bool) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/paid-content", api_host, game_id);

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let data: PaidContentListResponse = resp.json().await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&data.paid_content)?);
        return Ok(());
    }

    if data.paid_content.is_empty() {
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

    for entry in data.paid_content {
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
    pub show_files: bool,
    pub show_files_no_limit: bool,
}

pub async fn handle_paid_content_create(args: CreatePaidContentArgs<'_>) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!("{}/api/games/{}/paid-content", api_host, args.game_id);

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

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let created: CreatedPaidContent = resp.json().await?;

    println!(
        "✓ Created paid content \"{}\" (id: {})",
        created.content_identifier, created._id
    );
    print_report(
        created.match_report.as_ref(),
        args.show_files,
        args.show_files_no_limit,
    );
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
    pub show_files: bool,
    pub show_files_no_limit: bool,
}

pub async fn handle_paid_content_update(args: UpdatePaidContentArgs<'_>) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!(
        "{}/api/games/{}/paid-content/{}",
        api_host,
        args.game_id,
        args.reference.path_segment()
    );

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

    let resp = client
        .patch(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&serde_json::Value::Object(body))
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let updated: UpdatedPaidContent = resp.json().await?;

    println!("✓ Updated paid content {}", args.reference.value());
    print_report(
        updated.match_report.as_ref(),
        args.show_files,
        args.show_files_no_limit,
    );
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

        println!(
            "{} This stops offering paid content {} and cancels any price drop on it.\n\
             Players who already bought it keep access.",
            "Warning:".yellow().bold(),
            reference.value().bold()
        );
        let confirmed = cliclack::confirm("Are you sure you want to continue?")
            .initial_value(false)
            .interact()?;
        if !confirmed {
            println!("Aborted. Nothing was changed.");
            return Ok(());
        }
    }

    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!(
        "{}/api/games/{}/paid-content/{}",
        api_host,
        game_id,
        reference.path_segment()
    );

    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    config::check_api_response(resp).await?;
    println!("✓ Deactivated paid content {}", reference.value());
    Ok(())
}

pub struct ResolvePaidContentArgs<'a> {
    pub game_id: &'a str,
    pub patterns: &'a [String],
    pub reference: Option<PaidContentRef<'a>>,
    pub show_files: bool,
    pub show_files_no_limit: bool,
    pub json_output: bool,
    pub strict: bool,
}

pub async fn handle_paid_content_resolve(args: ResolvePaidContentArgs<'_>) -> Result<()> {
    let api_key = require_api_key()?;
    let client = config::create_http_client()?;
    let api_host = config::get("api_host")?;
    let url = format!(
        "{}/api/games/{}/paid-content/match-preview",
        api_host, args.game_id
    );

    let body = match args.reference {
        Some(PaidContentRef::Id(id)) => json!({ "paidContentId": id }),
        Some(PaidContentRef::ContentIdentifier(identifier)) => {
            json!({ "contentIdentifier": identifier })
        }
        None => json!({ "pathPatterns": args.patterns }),
    };

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await?;

    let resp = config::check_api_response(resp).await?;
    let report: MatchReport = resp.json().await?;

    if args.json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!(
            "{}",
            render_report(&report, args.show_files, args.show_files_no_limit)
        );
    }

    if args.strict && !report.zero_match_patterns.is_empty() {
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
            truncated: false,
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
    fn a_content_identifier_is_addressed_through_the_by_identifier_path() {
        assert_eq!(PaidContentRef::Id("pc_1").path_segment(), "pc_1");
        assert_eq!(
            PaidContentRef::ContentIdentifier("full-version").path_segment(),
            "by-identifier/full-version"
        );
    }

    #[test]
    fn report_echoes_every_pattern_verbatim_even_without_the_listing() {
        let report = report_with(4, vec![("levels/bonus/**", 4)]);
        let rendered = render_report(&report, false, false);
        assert!(
            rendered.contains("\"levels/bonus/**\""),
            "the pattern must be echoed back so shell expansion is visible: {rendered}"
        );
        assert!(rendered.contains("4 of 13 files gated"));
        assert!(!rendered.contains("levels/bonus/file0.dat"));
    }

    #[test]
    fn report_flags_a_pattern_that_matches_nothing() {
        let report = report_with(0, vec![("art/cutscenes/*.webm", 0)]);
        let rendered = render_report(&report, false, false);
        assert!(rendered.contains("matches nothing"), "{rendered}");
    }

    #[test]
    fn file_listing_truncates_and_says_how_many_it_omitted() {
        let report = report_with(60, vec![("levels/**", 60)]);
        let rendered = render_report(&report, true, false);
        assert!(rendered.contains("levels/bonus/file0.dat"));
        assert!(!rendered.contains("levels/bonus/file59.dat"));
        assert!(
            rendered.contains("… and 10 more"),
            "a cap that hides itself reads as a complete listing: {rendered}"
        );
        assert!(rendered.contains("--show-files-no-limit"));
    }

    #[test]
    fn no_limit_lists_every_matched_path() {
        let report = report_with(60, vec![("levels/**", 60)]);
        let rendered = render_report(&report, true, true);
        assert!(rendered.contains("levels/bonus/file59.dat"));
        assert!(!rendered.contains("… and"));
    }

    #[test]
    fn no_limit_alone_still_lists_the_paths() {
        let report = report_with(60, vec![("levels/**", 60)]);
        let rendered = render_report(&report, false, true);
        assert!(
            rendered.contains("levels/bonus/file59.dat"),
            "--show-files-no-limit implies --show-files: {rendered}"
        );
    }

    #[test]
    fn report_handles_a_game_with_no_completed_build() {
        let mut report = report_with(0, vec![("levels/**", 0)]);
        report.build = None;
        let rendered = render_report(&report, true, false);
        assert!(rendered.contains("No completed build yet"), "{rendered}");
    }

    #[test]
    fn truncated_index_is_reported_rather_than_hidden() {
        let mut report = report_with(4, vec![("levels/**", 4)]);
        report.truncated = true;
        let rendered = render_report(&report, false, false);
        assert!(rendered.contains("counts are a floor"), "{rendered}");
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
        };

        let json = serde_json::to_value(&entry).expect("the row should serialize");
        assert_eq!(json["_id"], "paid-content-id");
        assert_eq!(json["contentIdentifier"], "full-version");
        assert_eq!(json["pathPatterns"][0], "levels/bonus/**");
        assert_eq!(json["priceCents"], 499);
        assert_eq!(json["visibility"], "playtest");
        assert_eq!(json["title"], "Unlock all content");

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
                "contentIdentifier",
                "pathPatterns",
                "priceCents",
                "title",
                "visibility"
            ]
        );
    }
}
