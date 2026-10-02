//! Offline session cost report; shares the exact trajectory estimator with the web UI.
use crate::{openai::estimated_cost_usd, savings::Trajectory};
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
        /// Destination HTML file (defaults to ./carry-cost-report.html).
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn render(sessions: &Path) -> Result<String> {
    let mut entries = fs::read_dir(sessions)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    let mut rows = String::new();
    let mut count = 0;
    let mut actual_total = 0.;
    let mut savings_total = 0.;
    for entry in entries {
        let path = entry.path().join("trace.jsonl");
        if !path.is_file() {
            continue;
        }
        let trace = BufReader::new(fs::File::open(&path)?);
        let mut trajectory = Trajectory::default();
        let mut estimate = None;
        for line in trace.lines() {
            let line = line.with_context(|| format!("reading {}", path.display()))?;
            if let Ok(event) = serde_json::from_str(&line) {
                if let Some(current) = trajectory.record(&event) {
                    estimate = Some(current);
                }
            } // Ignore an interrupted final JSONL line.
        }
        let Some(estimate) = estimate else {
            continue;
        };
        count += 1;
        let actual = trajectory
            .requests
            .iter()
            .map(|r| {
                r.actual_cost
                    .or_else(|| estimated_cost_usd(&r.model, &r.usage))
            })
            .collect::<Option<Vec<_>>>();
        let id = escape(&entry.file_name().to_string_lossy());
        let model = escape(trajectory.requests.last().map_or("", |r| r.model.as_str()));
        let (actual, savings) = if estimate.priced {
            let actual = actual.unwrap_or_default().iter().sum::<f64>();
            actual_total += actual;
            savings_total += estimate.savings;
            (format!("${actual:.4}"), format!("${:.4}", estimate.savings))
        } else {
            ("unpriced".into(), "unpriced".into())
        };
        rows.push_str(&format!("<tr><td>{id}</td><td>{}</td><td>{}</td><td>{actual}</td><td>{}</td><td>{savings}</td></tr>",
            model, trajectory.requests.len(), estimate.pi_compactions));
    }
    Ok(format!(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><title>Carry cost report</title>
<style>body{{font:15px system-ui;background:#101214;color:#e7e9eb;max-width:1100px;margin:2rem auto;padding:1rem}}table{{border-collapse:collapse;width:100%}}td,th{{padding:.5rem;border-bottom:1px solid #444;text-align:left}}td:nth-child(n+3){{text-align:right}}.summary-cards{{display:flex;flex-wrap:wrap;gap:1rem;margin:1.5rem 0}}.summary-card{{display:flex;flex-direction:column;gap:.3rem;padding:1rem 1.3rem;border:1px solid #444;border-radius:12px;background:#1b1f23;min-width:180px}}.summary-card strong{{font-size:1.6rem}}.summary-card span{{color:#aeb6bf}}</style>
<h1>Carry cost report</h1><div class="summary-cards"><div class="summary-card"><strong>${actual_total:.4}</strong><span>Actual cost</span></div><div class="summary-card"><strong>${savings_total:.4}</strong><span>Estimated savings</span></div></div><p>{count} sessions with completed responses</p>
<table><thead><tr><th>Session</th><th>Last model</th><th>Responses</th><th>Actual</th><th>Estimated Pi compactions</th><th>Estimated savings</th></tr></thead><tbody>{rows}</tbody></table>
<p>Estimated savings = Pi-style counterfactual minus observed Carry cost. Shares the web UI estimator: 272K window, 16,384-token reserve, 20K recent tokens retained, 1K-token summary charged as uncached input plus output, rewritten prompt after each simulated compaction. Cache reuse is modeled from request sizes, not Carry cache classifications. Same outputs and tool behavior assumed; not billed savings. Unpriced sessions are excluded from totals.</p></html>"#
    ))
}

pub fn run(args: ReportCli) -> Result<()> {
    let ReportCommand::Cost { sessions, output } = args.command;
    let sessions = sessions.unwrap_or(crate::auth::carry_home()?.join("sessions"));
    let output = output.unwrap_or_else(|| PathBuf::from("carry-cost-report.html"));
    let html =
        render(&sessions).with_context(|| format!("reading sessions in {}", sessions.display()))?;
    fs::write(&output, html).with_context(|| format!("writing {}", output.display()))?;
    println!("Cost report: {}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cost_report_uses_shared_estimator() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("one");
        fs::create_dir(&session).unwrap();
        fs::write(session.join("trace.jsonl"), concat!(
            "{\"event\":\"run_started\",\"data\":{\"model\":\"gpt-6-sol\"}}\n",
            "{\"event\":\"model_response\",\"data\":{\"usage\":{\"input_tokens\":1000,\"output_tokens\":5}}}\n"
        )).unwrap();
        let session_two = dir.path().join("two");
        fs::create_dir(&session_two).unwrap();
        fs::copy(session.join("trace.jsonl"), session_two.join("trace.jsonl")).unwrap();
        let output = dir.path().join("cost.html");
        run(ReportCli {
            command: ReportCommand::Cost {
                sessions: Some(dir.path().into()),
                output: Some(output.clone()),
            },
        })
        .unwrap();
        let html = fs::read_to_string(output).unwrap();
        assert!(html.contains("one"));
        assert!(html.contains("$0.0000"));
        assert!(html.contains("<td>0</td><td>$0.0000"));
        assert!(html.contains("class=\"summary-cards\""));
        assert!(html.contains("<strong>$0.0041</strong><span>Actual cost</span>"));
        assert!(html.contains("<strong>$0.0000</strong><span>Estimated savings</span>"));
        assert!(html.contains("2 sessions with completed responses"));
    }
}
