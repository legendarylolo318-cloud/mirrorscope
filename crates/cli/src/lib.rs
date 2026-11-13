//! Headless mode: `mirrorscope --cli [flags]`, modelled on reflector's core options.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::Parser;
use mirrorscope_core::config::{default_config_path, Config, Settings};
use mirrorscope_core::fetch::{fetch_status, http_client};
use mirrorscope_core::mirrorlist::{generate, HeaderInfo};
use mirrorscope_core::speedtest::{run_speedtest, SpeedEvent, SpeedOptions, SpeedOutcome};
use mirrorscope_core::{
    filter::filter_mirrors, privileged, sort::sort_mirrors, Mirror, Protocol, SortKey, SortOrder,
    STATUS_URL, SYSTEM_MIRRORLIST,
};
use tokio_util::sync::CancellationToken;

#[derive(Parser, Debug)]
#[command(
    name = "mirrorscope",
    version,
    about = "Arch Linux mirrorlist manager (CLI mode)"
)]
pub struct CliArgs {
    /// Run headless (required to select this mode)
    #[arg(long)]
    pub cli: bool,
    /// Restrict to countries (names or ISO codes, comma separated or repeated)
    #[arg(short, long, value_delimiter = ',')]
    pub country: Vec<String>,
    /// Allowed protocols: https, http, rsync (comma separated or repeated)
    #[arg(short, long, value_delimiter = ',')]
    pub protocol: Vec<String>,
    /// Maximum hours since last sync
    #[arg(short, long)]
    pub age: Option<f64>,
    /// Minimum completion percentage
    #[arg(long)]
    pub completion_percent: Option<f64>,
    /// Sort by: score, delay, age, country, rate
    #[arg(long)]
    pub sort: Option<String>,
    /// Only consider the N most recently synchronised mirrors
    #[arg(short, long)]
    pub latest: Option<usize>,
    /// Return at most N mirrors
    #[arg(short, long)]
    pub number: Option<usize>,
    /// Only mirrors reachable over IPv4
    #[arg(short = '4', long)]
    pub ipv4: bool,
    /// Only mirrors reachable over IPv6
    #[arg(short = '6', long)]
    pub ipv6: bool,
    /// Keep only URLs matching this regex
    #[arg(short, long)]
    pub include: Option<String>,
    /// Drop URLs matching this regex
    #[arg(short = 'x', long)]
    pub exclude: Option<String>,
    /// Write the mirrorlist here (default: print to stdout). The system
    /// mirrorlist is written through the privileged helper.
    #[arg(long)]
    pub save: Option<PathBuf>,
    /// Load a named preset from the config file
    #[arg(long)]
    pub preset: Option<String>,
    /// Use this config file instead of ~/.config/mirrorscope/config.toml
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Speed test concurrency
    #[arg(long)]
    pub concurrency: Option<usize>,
    /// Speed test timeout per mirror, seconds
    #[arg(long)]
    pub download_timeout: Option<u64>,
    /// Print the list of countries and mirror counts, then exit
    #[arg(long)]
    pub list_countries: bool,
}

fn parse_protocol(s: &str) -> Result<Protocol> {
    match s.to_ascii_lowercase().as_str() {
        "https" => Ok(Protocol::Https),
        "http" => Ok(Protocol::Http),
        "rsync" => Ok(Protocol::Rsync),
        other => bail!("unknown protocol {other:?} (use https, http or rsync)"),
    }
}

/// Merge: defaults (or preset) overridden by explicit flags.
pub fn build_settings(args: &CliArgs, config: &Config) -> Result<Settings> {
    let mut s = match &args.preset {
        Some(name) => config
            .presets
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("preset {name:?} not found in config"))?,
        None => Settings::default(),
    };
    if !args.country.is_empty() {
        s.filter.countries = args.country.clone();
    }
    if !args.protocol.is_empty() {
        s.filter.protocols = args
            .protocol
            .iter()
            .map(|p| parse_protocol(p))
            .collect::<Result<_>>()?;
    }
    if let Some(a) = args.age {
        s.filter.max_age_hours = Some(a);
    }
    if let Some(c) = args.completion_percent {
        s.filter.min_completion_pct = c;
    }
    s.filter.require_ipv4 |= args.ipv4;
    s.filter.require_ipv6 |= args.ipv6;
    if let Some(i) = &args.include {
        s.filter.include_regex = i.clone();
    }
    if let Some(x) = &args.exclude {
        s.filter.exclude_regex = x.clone();
    }
    if let Some(k) = &args.sort {
        s.sort = SortKey::parse(k).ok_or_else(|| anyhow!("unknown sort key {k:?}"))?;
        s.order = s.sort.default_order();
    }
    if let Some(l) = args.latest {
        s.latest = l;
    }
    if let Some(n) = args.number {
        s.number = n;
    }
    if let Some(c) = args.concurrency {
        s.concurrency = c;
    }
    if let Some(t) = args.download_timeout {
        s.timeout_secs = t;
    }
    Ok(s)
}

/// Entry point. `argv` includes the program name. Returns the process exit code.
pub fn run(argv: Vec<String>) -> i32 {
    let args = CliArgs::parse_from(argv);
    match run_inner(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mirrorscope: error: {e:#}");
            1
        }
    }
}

fn run_inner(args: CliArgs) -> Result<()> {
    let cfg_path = args.config.clone().or_else(default_config_path);
    let config = match &cfg_path {
        Some(p) => Config::load(p).with_context(|| "loading config")?,
        None => Config::default(),
    };
    let settings = build_settings(&args, &config)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(pipeline(args, settings))
}

async fn pipeline(args: CliArgs, s: Settings) -> Result<()> {
    let client = http_client(Duration::from_secs(10))?;
    eprintln!("Fetching mirror status from {STATUS_URL}");
    let doc = fetch_status(&client, None)
        .await
        .context("fetching mirror status")?;
    let now = Utc::now();

    if args.list_countries {
        let mut counts: std::collections::BTreeMap<(String, String), usize> = Default::default();
        for m in doc
            .urls
            .iter()
            .filter(|m| m.active && !m.country.is_empty())
        {
            *counts
                .entry((m.country.clone(), m.country_code.clone()))
                .or_default() += 1;
        }
        for ((name, code), n) in counts {
            println!("{name:<32} {code:<4} {n}");
        }
        return Ok(());
    }

    let mut list = filter_mirrors(&doc.urls, &s.filter, now)?;
    eprintln!("{} mirrors match the filters", list.len());
    if list.is_empty() {
        bail!("no mirrors match the given filters");
    }
    if s.latest > 0 {
        sort_mirrors(
            &mut list,
            SortKey::Age,
            SortOrder::Ascending,
            &HashMap::new(),
            now,
        );
        list.truncate(s.latest);
    }

    let mut rates: HashMap<String, f64> = HashMap::new();
    if s.sort == SortKey::Rate {
        rates = measure(&client, &list, &s).await;
        // Mirrors that failed the speed test are dropped, like reflector does.
        list.retain(|m| rates.contains_key(&m.url));
        if list.is_empty() {
            bail!("no mirror completed the speed test");
        }
    }
    sort_mirrors(&mut list, s.sort, s.order, &rates, now);
    list.truncate(s.number.max(1));

    let refs: Vec<&Mirror> = list.iter().collect();
    let filters = s.filter.describe();
    let sort_name = format!("{:?}", s.sort).to_lowercase();
    let text = generate(
        &refs,
        &HeaderInfo {
            when: now,
            filters: &format!("{filters} latest={} number={}", s.latest, s.number),
            sort: &sort_name,
            source: STATUS_URL,
        },
    );

    match &args.save {
        None => print!("{text}"),
        Some(p) if p.as_os_str() == SYSTEM_MIRRORLIST => {
            let out =
                tokio::task::spawn_blocking(move || privileged::run_helper(&["write"], &text))
                    .await?
                    .map_err(|e| anyhow!("{e}"))?;
            eprint!("{out}");
        }
        Some(p) => {
            let tmp = p.with_extension("mirrorscope-tmp");
            std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
            std::fs::rename(&tmp, p).with_context(|| format!("renaming to {}", p.display()))?;
            eprintln!("Wrote {}", p.display());
        }
    }
    Ok(())
}

async fn measure(client: &reqwest::Client, list: &[Mirror], s: &Settings) -> HashMap<String, f64> {
    let bases: Vec<String> = list.iter().map(|m| m.base_url()).collect();
    let url_by_base: HashMap<String, String> =
        list.iter().map(|m| (m.base_url(), m.url.clone())).collect();
    let results = std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
    let r2 = results.clone();
    let total = bases.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::Arc::new(done);
    eprintln!("Speed testing {total} mirrors...");
    run_speedtest(
        client.clone(),
        bases,
        SpeedOptions {
            concurrency: s.concurrency,
            timeout: Duration::from_secs(s.timeout_secs.max(1)),
        },
        CancellationToken::new(),
        move |ev| {
            if let SpeedEvent::Finished(base, out) = ev {
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                eprintln!("[{n}/{total}] {base}: {}", describe(&out));
                if let (SpeedOutcome::Rate(r), Some(url)) = (out, url_by_base.get(&base)) {
                    if let Ok(mut g) = r2.lock() {
                        g.insert(url.clone(), r);
                    }
                }
            }
        },
    )
    .await;
    let out = results.lock().map(|g| g.clone()).unwrap_or_default();
    out
}

fn describe(o: &SpeedOutcome) -> String {
    match o {
        SpeedOutcome::Rate(r) => mirrorscope_core::speedtest::format_rate(*r),
        SpeedOutcome::Timeout => "timeout".into(),
        SpeedOutcome::Error(e) => format!("error ({e})"),
        SpeedOutcome::Cancelled => "cancelled".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(a: &[&str]) -> CliArgs {
        CliArgs::parse_from(std::iter::once("mirrorscope").chain(a.iter().copied()))
    }

    #[test]
    fn flags_override_defaults() {
        let a = parse(&[
            "--cli",
            "-c",
            "Germany,fr",
            "-p",
            "https,http",
            "-a",
            "12",
            "--sort",
            "rate",
            "-n",
            "5",
            "-l",
            "50",
        ]);
        let s = build_settings(&a, &Config::default()).unwrap();
        assert_eq!(s.filter.countries, vec!["Germany", "fr"]);
        assert_eq!(s.filter.protocols.len(), 2);
        assert_eq!(s.filter.max_age_hours, Some(12.0));
        assert_eq!(s.sort, SortKey::Rate);
        assert_eq!(s.order, SortOrder::Descending);
        assert_eq!((s.number, s.latest), (5, 50));
    }

    #[test]
    fn preset_then_override() {
        let mut cfg = Config::default();
        let mut p = Settings {
            number: 3,
            ..Default::default()
        };
        p.filter.countries = vec!["Sweden".into()];
        cfg.presets.insert("w".into(), p);
        let s = build_settings(&parse(&["--cli", "--preset", "w", "-n", "7"]), &cfg).unwrap();
        assert_eq!(s.number, 7);
        assert_eq!(s.filter.countries, vec!["Sweden"]);
        assert!(build_settings(&parse(&["--cli", "--preset", "nope"]), &cfg).is_err());
    }

    #[test]
    fn bad_values() {
        assert!(build_settings(&parse(&["--cli", "-p", "gopher"]), &Config::default()).is_err());
        assert!(build_settings(&parse(&["--cli", "--sort", "wat"]), &Config::default()).is_err());
    }
}
