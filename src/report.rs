//! Offline session cost report; shares the exact trajectory estimator with the web UI.
use crate::savings::{Coverage, Scenario, Trajectory, savings_percent};
use anyhow::{Context, Result};
use clap::Parser;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

#[derive(Debug, Parser)]
pub struct ReportCli {
    #[command(subcommand)]
    command: ReportCommand,
}

#[derive(Debug, clap::Subcommand)]
enum ReportCommand {
    /// Estimate session costs compared with Pi-style compaction.
    Cost {
        /// Directory containing Carry session directories (defaults to ~/.carry/sessions).
        #[arg(long)]
        sessions: Option<PathBuf>,
        /// Destination HTML file (defaults to a persistent file in the system temp directory).
        #[arg(long)]
        output: Option<PathBuf>,
        /// Proxy-domain scenario window, not a verified provider model limit.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        scenario_context_window: Option<u64>,
    },
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn render(sessions: &Path) -> Result<String> {
    render_with_scenario(sessions, Scenario::default())
}

pub fn render_with_scenario(sessions: &Path, scenario: Scenario) -> Result<String> {
    let mut entries = fs::read_dir(sessions)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    let mut rows = String::new();
    let (mut count, mut no_trace, mut no_responses, mut eligible) = (0, 0, 0, 0);
    let (mut observed_total, mut carry_total, mut pi_total) = (0.0, 0.0, 0.0);
    let mut coverage = Coverage::default();
    for entry in entries {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("trace.jsonl");
        if !path.is_file() {
            no_trace += 1;
            continue;
        }
        count += 1;
        let mut trajectory = Trajectory::with_scenario(scenario);
        for line in BufReader::new(fs::File::open(&path)?).lines() {
            let line = line.with_context(|| format!("reading {}", path.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str(&line) {
                trajectory.record(&event);
            } else {
                trajectory.invalid_line();
            }
            // Interrupted or malformed records are counted, never silently priced.
        }
        let estimate = trajectory.snapshot();
        let c = &estimate.coverage;
        no_responses += usize::from(c.responses == 0);
        observed_total += estimate.observed_cost;
        coverage.requests += c.requests;
        coverage.responses += c.responses;
        coverage.unmatched_requests += c.unmatched_requests;
        coverage.missing_usage += c.missing_usage;
        coverage.orphan_responses += c.orphan_responses;
        coverage.unpriced_responses += c.unpriced_responses;
        coverage.retries += c.retries;
        coverage.invalid_lines += c.invalid_lines;
        coverage.missing_projection += c.missing_projection;
        coverage.missing_output_projection += c.missing_output_projection;
        coverage.unknown_cache_requests += c.unknown_cache_requests;
        coverage.unknown_expiry_requests += c.unknown_expiry_requests;
        coverage.unknown_route_requests += c.unknown_route_requests;
        let id = escape(&entry.file_name().to_string_lossy());
        let model = escape(&trajectory.model);
        let observed = format!(
            "${:.4}{}",
            estimate.observed_cost,
            if estimate.observed_priced {
                ""
            } else {
                " (partial)"
            }
        );
        let (carry, pi, savings) = if estimate.priced {
            eligible += 1;
            carry_total += estimate.carry_cost;
            pi_total += estimate.pi_cost;
            (
                format!("${:.4}", estimate.carry_cost),
                format!("${:.4}", estimate.pi_cost),
                format!("${:.4}", estimate.savings),
            )
        } else {
            (
                "unavailable".into(),
                "unavailable".into(),
                "unavailable".into(),
            )
        };
        let window = trajectory.scenario.context_window;
        let window_source = if estimate.recorded_window {
            "recorded"
        } else {
            "scenario"
        };
        rows.push_str(&format!("<tr><td>{id}</td><td>{model}</td><td>{}/{}</td><td>{observed}</td><td>{carry}</td><td>{pi}</td><td>{savings}</td><td>{}</td><td>{window} ({window_source})</td><td>{} unmatched; {} missing usage; {} orphan; {} unpriced; {} retries; {} invalid; {} missing input / {} output projections</td></tr>",c.responses,c.requests,estimate.pi_compactions,c.unmatched_requests,c.missing_usage,c.orphan_responses,c.unpriced_responses,c.retries,c.invalid_lines,c.missing_projection,c.missing_output_projection));
    }
    let savings_total = pi_total - carry_total;
    let rate = savings_percent(savings_total, carry_total)
        .map_or_else(|| "—".to_owned(), |rate| format!("{rate:.1}%"));
    let unavailable = count - eligible;
    Ok(format!(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Carry cost report</title>
<style>body{{font:15px system-ui;background:#101214;color:#e7e9eb;max-width:1400px;margin:2rem auto;padding:1rem}}table{{border-collapse:collapse;width:100%}}td,th{{padding:.5rem;border-bottom:1px solid #444;text-align:left}}.summary-cards{{display:flex;flex-wrap:wrap;gap:1rem;margin:1.5rem 0}}.summary-card{{display:flex;flex-direction:column;gap:.3rem;padding:1rem 1.3rem;border:1px solid #444;border-radius:12px;background:#1b1f23;min-width:180px}}.summary-card strong{{font-size:1.6rem}}.summary-card span{{color:#aeb6bf}}</style>
<h1>Carry cost report</h1><div class="summary-cards"><div class="summary-card"><strong>${observed_total:.4}</strong><span>Observed modeled response subtotal</span></div><div class="summary-card"><strong>${carry_total:.4}</strong><span>Carry proxy scenario</span></div><div class="summary-card"><strong>${pi_total:.4}</strong><span>Pi-style proxy scenario</span></div><div class="summary-card"><strong>${savings_total:.4}</strong><span>Conditional scenario savings</span></div><div class="summary-card"><strong>{rate}</strong><span>Conditional scenario savings rate</span></div></div>
<p>{count} traces; {no_responses} without responses; {no_trace} directories without traces. {eligible}/{count} scenario-priced sessions; {unavailable} unavailable, excluded from both scenario totals and the savings denominator.</p>
<p>Coverage: {responses} responses / {requests} requests; {unmatched} unmatched requests; {missing} missing usage; {orphans} orphan responses; {unpriced} unpriced responses; {retries} recorded retries (extra charges unknown); {invalid} invalid records. Observed subtotal covers only priced responses, not complete spend when coverage is partial. Input/output projections missing: {missing_input}/{missing_output}.</p>
<table><thead><tr><th>Session</th><th>Last model</th><th>Responses/requests</th><th>Observed modeled subtotal</th><th>Carry proxy</th><th>Pi-style proxy</th><th>Scenario savings</th><th>Pi summaries</th><th>Window</th><th>Coverage</th></tr></thead><tbody>{rows}</tbody></table>
<p>Both scenario input branches use serialized prompt-bearing input, tools and instructions UTF-8 bytes / 4 (rounded up), with native dropped byte-proxy units restored only in the alternative. No metered input tokens are mixed into proxy sizes. Carry scenario cost is independently simulated, not set equal to observed cost. Frozen provider output tokens are a common charge at the model's base output rate. Input rate tiers and cache minimums are applied in the declared proxy domain, not verified provider token counts.</p>
<p>Configured scenario window {window}, reserve {reserve}, recent {recent}, summary {summary} proxy units; recorded window metadata overrides the configured window when present. Default 272K is a scenario, not a verified model limit. Thresholds are checked at recorded turn_finished/run_finished agent-run completion, never before the first prompt or per model request. Summary discarded input is uncached and assumed summary output is charged even at a terminal boundary.</p>
<p>Symmetric modeled caching: first writes, minimum eligibility, model/cache-key/recorded-route changes, recorded TTL and expiry boundaries; non-append prefix edits reset both branches. Carry compaction resets its prefix; the alternative assumes retained restored bytes and append/frozen-size reuse until its own summary rewrites the prefix. Hypothetical prefix bytes and summary quality are unknown. Missing route/expiry metadata assumes same route/unexpired cache conditionally ({unknown_route} route-unknown requests; {unknown_expiry} expiry-unknown requests). Unknown cache capability uses uncached pricing ({unknown_cache} requests). Same future outputs/tools assumed. Not billed savings, actual Pi behavior, or evidence of preserved quality.</p></html>"#,
        responses = coverage.responses,
        requests = coverage.requests,
        unmatched = coverage.unmatched_requests,
        missing = coverage.missing_usage,
        orphans = coverage.orphan_responses,
        unpriced = coverage.unpriced_responses,
        retries = coverage.retries,
        invalid = coverage.invalid_lines,
        missing_input = coverage.missing_projection,
        missing_output = coverage.missing_output_projection,
        window = scenario.context_window,
        reserve = scenario.reserve,
        recent = scenario.keep_recent,
        summary = scenario.summary,
        unknown_route = coverage.unknown_route_requests,
        unknown_expiry = coverage.unknown_expiry_requests,
        unknown_cache = coverage.unknown_cache_requests
    ))
}

pub fn run(args: ReportCli) -> Result<()> {
    run_with_open(args, crate::auth::open_browser)
}

fn run_with_open(args: ReportCli, open: impl FnOnce(&str)) -> Result<()> {
    let ReportCommand::Cost {
        sessions,
        output,
        scenario_context_window,
    } = args.command;
    let sessions = sessions.unwrap_or(crate::auth::carry_home()?.join("sessions"));
    let html = match scenario_context_window {
        Some(context_window) => render_with_scenario(
            &sessions,
            Scenario {
                context_window,
                ..Scenario::default()
            },
        ),
        None => render(&sessions),
    }
    .with_context(|| format!("reading sessions in {}", sessions.display()))?;
    let output = match output {
        Some(path) => path,
        None => {
            tempfile::Builder::new()
                .prefix("carry-cost-report-")
                .suffix(".html")
                .tempfile()
                .context("creating temporary report")?
                .keep()
                .context("persisting temporary report")?
                .1
        }
    };
    fs::write(&output, html).with_context(|| format!("writing {}", output.display()))?;
    println!("Cost report: {}", output.display());
    let url = url::Url::from_file_path(&output.canonicalize()?)
        .map_err(|_| anyhow::anyhow!("could not construct file URL for {}", output.display()))?;
    println!("Open in browser: {url}");
    open(url.as_str());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_report_is_persisted_in_system_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let before = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect::<std::collections::HashSet<_>>();
        let mut opened = None;
        run_with_open(
            ReportCli {
                command: ReportCommand::Cost {
                    sessions: Some(dir.path().into()),
                    output: None,
                    scenario_context_window: None,
                },
            },
            |url| opened = Some(url.to_owned()),
        )
        .unwrap();
        let new_reports = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|path| {
                !before.contains(path)
                    && path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("carry-cost-report-")
            })
            .collect::<Vec<_>>();
        assert_eq!(new_reports.len(), 1);
        let url = url::Url::parse(opened.as_deref().unwrap()).unwrap();
        assert_eq!(
            url.to_file_path().unwrap(),
            new_reports[0].canonicalize().unwrap()
        );
        assert!(
            fs::read_to_string(&new_reports[0])
                .unwrap()
                .contains("Carry cost report")
        );
        fs::remove_file(&new_reports[0]).unwrap();
    }

    #[test]
    fn unanswered_request_changes_rendered_coverage() {
        fn fixture(root: &Path, pending: bool) -> PathBuf {
            fs::create_dir_all(root.join("same")).unwrap();
            let mut events = vec![
                serde_json::json!({"event":"run_started","data":{"model":"gpt-6-sol"}}),
                serde_json::json!({"event":"model_request","data":{"step":1}}),
                serde_json::json!({"event":"model_response","data":{"step":1,"usage":{"input_tokens":1000,"output_tokens":100}}}),
            ];
            if pending {
                events.push(serde_json::json!({"event":"model_request","data":{"step":2}}));
            }
            fs::write(
                root.join("same/trace.jsonl"),
                events
                    .iter()
                    .map(|v| v.to_string() + "\n")
                    .collect::<String>(),
            )
            .unwrap();
            root.into()
        }
        let dir = tempfile::tempdir().unwrap();
        let a = fixture(&dir.path().join("a"), false);
        let b = fixture(&dir.path().join("b"), true);
        assert_ne!(render(&a).unwrap(), render(&b).unwrap());
    }
    #[test]
    fn invalid_record_is_visible_and_invalidates_totals() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("one")).unwrap();
        fs::write(dir.path().join("one/trace.jsonl"), "{broken\n").unwrap();
        let html = render(dir.path()).unwrap();
        assert!(html.contains("1 invalid records"));
        assert!(html.contains("1/") || html.contains("0/1 scenario-priced"));
    }
    #[test]
    fn scenario_window_can_be_configured_without_breaking_existing_cli() {
        assert!(
            ReportCli::try_parse_from(["report", "cost", "--scenario-context-window", "300000"])
                .is_ok()
        );
        assert!(ReportCli::try_parse_from(["report", "cost"]).is_ok());
    }
    #[test]
    fn no_priced_sessions_have_no_savings_rate() {
        let dir = tempfile::tempdir().unwrap();
        let html = render(dir.path()).unwrap();
        assert!(html.contains("<strong>—</strong><span>Conditional scenario savings rate</span>"));
    }

    #[test]
    fn cost_report_uses_shared_estimator() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("one");
        fs::create_dir(&session).unwrap();
        fs::write(session.join("trace.jsonl"), concat!(
            "{\"event\":\"run_started\",\"data\":{\"model\":\"gpt-6-sol\"}}\n",
            "{\"event\":\"model_request\",\"data\":{\"request\":{\"input\":[\"abc\"],\"tools\":[]}}}\n",
            "{\"event\":\"model_response\",\"data\":{\"usage\":{\"input_tokens\":1000,\"output_tokens\":5},\"raw\":{\"output\":[]}}}\n",
            "{\"event\":\"run_finished\",\"data\":{}}\n"
        )).unwrap();
        let session_two = dir.path().join("two");
        fs::create_dir(&session_two).unwrap();
        fs::copy(session.join("trace.jsonl"), session_two.join("trace.jsonl")).unwrap();
        let output = dir.path().join("cost report.html");
        let mut opened = None;
        run_with_open(
            ReportCli {
                command: ReportCommand::Cost {
                    sessions: Some(dir.path().into()),
                    output: Some(output.clone()),
                    scenario_context_window: None,
                },
            },
            |url| opened = Some(url.to_owned()),
        )
        .unwrap();
        assert_eq!(
            url::Url::parse(opened.as_deref().unwrap())
                .unwrap()
                .to_file_path()
                .unwrap(),
            output.canonicalize().unwrap()
        );
        let html = fs::read_to_string(output).unwrap();
        assert!(html.contains("one"));
        assert!(html.contains("$0.0000"));
        assert!(html.contains("<td>$0.0000</td><td>0</td>"));
        assert!(html.contains("class=\"summary-cards\""));
        assert!(
            html.contains(
                "<strong>$0.0041</strong><span>Observed modeled response subtotal</span>"
            )
        );
        assert!(html.contains("<strong>$0.0000</strong><span>Conditional scenario savings</span>"));
        assert!(html.contains("2/2 scenario-priced sessions"));
        assert!(
            html.contains("<strong>0.0%</strong><span>Conditional scenario savings rate</span>")
        );
    }
}
