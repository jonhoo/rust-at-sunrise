#[macro_use]
extern crate slog;
#[macro_use]
extern crate serde_derive;

use std::env;
use std::fmt;
use std::path::Path;
use std::time;

use chrono::prelude::*;
use clap::{crate_version, App, Arg};
use egg_mode::tweet::DraftTweet;

const CONSUMER_KEY: &'static str = "XurcamcbIvruiowuIuLLxpkEV";
const ACCESS_TOKEN: &'static str = "864346480437469185-itNALA4j82KEdvYg8Mh1XLZoYdHTiLK";
const NIGHTLY_MANIFEST: &'static str =
    "https://static.rust-lang.org/dist/channel-rust-nightly.toml";

#[tokio::main]
async fn main() {
    // argument parsing
    let matches = App::new("Rust at Sunrise")
        .version(crate_version!())
        .author("Jon Gjengset <jon@thesqsuareplanet.com>")
        .about("Tweets information about newest Rust Nightly to @rust_at_sunrise")
        .arg(
            Arg::with_name("dry")
                .help("Do a dry run which does not loop or post to twitter")
                .short("d")
                .long("dry-run"),
        )
        .arg(
            Arg::with_name("RUSTV")
                .help("Last rust nightly version string [read from .sunrise-last.toml otherwise]")
                .requires("CARGOV")
                .index(1),
        )
        .arg(
            Arg::with_name("CARGOV")
                .help("Last cargo nightly version string [read from .sunrise-last.toml otherwise]")
                .index(2),
        )
        .get_matches();

    // we want to log things
    use slog::Drain;
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();
    let log = slog::Logger::root(drain, o!());

    // what is current nightly?
    let mut last = if matches.is_present("RUSTV") {
        Nightly {
            rust: Version::from_str(matches.value_of("RUSTV").unwrap()).unwrap(),
            cargo: Version::from_str(matches.value_of("CARGOV").unwrap()).unwrap(),
            perf: None,
        }
    } else {
        let path = Path::new(".sunrise-last.toml");
        if path.exists() {
            match std::fs::read(path) {
                Ok(last) => match toml::from_slice(&last) {
                    Ok(last) => last,
                    Err(e) => {
                        error!(log, "invalid .sunrise-last.toml");
                        eprintln!("{:?}", e);
                        return;
                    }
                },
                Err(e) => {
                    error!(log, "could not read .sunrise-last.toml");
                    eprintln!("{:?}", e);
                    return;
                }
            }
        } else {
            info!(log, "no known last nightly -- assuming current is last");
            match nightly().await {
                Ok(nightly) => {
                    if !matches.is_present("dry") {
                        info!(log, "saving discovered nightly to .sunrise-last.toml");
                        match toml::ser::to_vec(&nightly) {
                            Ok(bytes) => {
                                if let Err(e) = std::fs::write(".sunrise-last.toml", &bytes) {
                                    warn!(log, "could not save current nightly to disk");
                                    eprintln!("{:?}", e);
                                }
                            }
                            Err(e) => {
                                warn!(log, "could not save current nightly");
                                eprintln!("{:?}", e);
                            }
                        }
                    }
                    nightly
                }
                Err(e) => {
                    error!(log, "could not discover current nightly");
                    eprintln!("{:?}", e);
                    return;
                }
            }
        }
    };

    info!(log, "last rust nightly";
      "version" => %last.rust.number,
      "rev" => &last.rust.revision,
      "date" => %last.rust.date);
    info!(log, "last cargo nightly";
      "version" => %last.cargo.number,
      "rev" => &last.cargo.revision,
      "date" => %last.cargo.date);

    let twitter = if matches.is_present("dry") && env::var("CONSUMER_SECRET").is_err() {
        None
    } else {
        // we need twitter access
        info!(log, "authenticating with twitter");
        let con_secret = env::var("CONSUMER_SECRET").unwrap();
        let access_secret = env::var("ACCESS_TOKEN_SECRET").unwrap();
        let con_token = egg_mode::KeyPair::new(CONSUMER_KEY, con_secret);
        let access_token = egg_mode::KeyPair::new(ACCESS_TOKEN, access_secret);
        let token = egg_mode::Token::Access {
            consumer: con_token,
            access: access_token,
        };

        match egg_mode::service::config(&token).await {
            Ok(c) => Some((token, c)),
            Err(_) if matches.is_present("dry") => None,
            Err(e) => {
                crit!(log, "failed to get twitter config: {}", e);
                return;
            }
        }
    };

    // and then we loop
    loop {
        match nightly().await {
            Ok(mut nightly) => {
                // did nightly change?
                if !last.rust.revision.starts_with(&nightly.rust.revision) {
                    fill_perf(&log, &mut nightly, &mut last).await;
                    let tweet = new_nightly(&log, &nightly, &last);
                    last = nightly;

                    if let Some((ref token, ref config)) = twitter {
                        let limit = 280;
                        match egg_mode_text::character_count(
                            &*tweet,
                            config.short_url_length,
                            config.short_url_length_https,
                        ) {
                            chars if chars <= limit && matches.is_present("dry") => {
                                info!(log, "would have tweeted"; "chars" => chars);
                                println!("{}", tweet);
                            }
                            chars if chars <= limit => {
                                info!(log, "tweeting"; "chars" => chars);
                                println!("{}", tweet);

                                let draft = DraftTweet::new(tweet.to_string());
                                if let Err(e) = draft.send(&token).await {
                                    error!(log, "could not tweet: {}", e);
                                    break;
                                }

                                info!(log, "saving last seen nightly to .sunrise-last.toml");
                                match toml::ser::to_vec(&last) {
                                    Ok(bytes) => {
                                        if let Err(e) = std::fs::write(".sunrise-last.toml", &bytes)
                                        {
                                            warn!(log, "could not save latest nightly to disk");
                                            eprintln!("{:?}", e);
                                        }
                                    }
                                    Err(e) => {
                                        warn!(log, "could not save latest nightly");
                                        eprintln!("{:?}", e);
                                    }
                                }
                            }
                            chars => {
                                error!(log, "tweet is too long"; "chars" => chars);
                                break;
                            }
                        }
                    } else {
                        info!(log, "would have tweeted:");
                        println!("{}", tweet);
                    }
                } else {
                    debug!(log, "nightly did not change"; "current" => %nightly.rust.date);
                }
            }
            Err(e) => {
                error!(log, "{}", e);
                // try again later -- manifest shouldn't generally be broken
            }
        }

        if matches.is_present("dry") {
            warn!(log, "exiting early since we're doing a dry run");
            break;
        }

        tokio::time::delay_for(time::Duration::from_secs(30 * 60)).await;
    }
}

async fn expand_sha(
    client: &reqwest::Client,
    log: &slog::Logger,
    n: &mut Nightly,
) -> Option<(String, DateTime<Utc>)> {
    let bors_merge_commits = format!(
        "https://api.github.com/repos/rust-lang/rust/commits?author=bors&per_page=1&sha={}",
        n.rust.revision
    );
    let bors_merge_commits = client
        .get(&bors_merge_commits)
        .header("User-Agent", "jonhoo-rust-at-sunrise")
        .send()
        .await
        .ok()?;
    let bors_merge_commits = bors_merge_commits
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let mut bors_merge_commits = bors_merge_commits.as_array()?.iter();
    debug!(log, "looking for commit info for {}", n.rust.revision);

    let commit = bors_merge_commits.next()?;
    let commit = commit.as_object()?;
    let sha = commit["sha"].as_str()?;
    debug!(log, "found commit {}", sha);
    assert!(sha.starts_with(&n.rust.revision));

    // keep track of the full revision hash
    debug!(log, "expanded {} to {}", n.rust.revision, sha);
    n.rust.revision = sha.to_owned();
    let commit = commit["commit"].as_object()?;

    // just for debugging really
    let _message = commit["message"].as_str()?.lines().next()?;
    let date = commit["committer"].as_object()?["date"]
        .as_str()?
        .parse::<DateTime<Utc>>()
        .ok()?;

    Some((sha.to_string(), date))
}

async fn fill_perf(log: &slog::Logger, new: &mut Nightly, old: &mut Nightly) {
    let client = reqwest::Client::new();
    let (old_sha, old_date) = match expand_sha(&client, log, old).await {
        Some(r) => r,
        None => {
            error!(
                log,
                "could not find old perf data for commit {}", old.rust.revision
            );
            return;
        }
    };
    let (new_sha, new_date) = match expand_sha(&client, log, new).await {
        Some(r) => r,
        None => {
            error!(
                log,
                "could not find old perf data for commit {}", old.rust.revision
            );
            return;
        }
    };

    // see if we have a perf result for the current commit
    let mut desc = serde_json::Map::default();
    desc.insert("start".into(), old_sha.clone().into());
    desc.insert("end".into(), new_sha.clone().into());
    desc.insert("stat".into(), "cpu-clock".into());
    let timing = client
        .post("https://perf.rust-lang.org/perf/get")
        .json(&desc);
    let perf = timing
        .header("User-Agent", "jonhoo-rust-at-sunrise")
        .send()
        .await;
    let perf: serde_json::Value = match perf {
        Ok(r) => {
            let bytes = match r.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    // move on to an earlier commit
                    debug!(log, "could not fetch perf results for commit: {:?}", e);
                    return;
                }
            };

            if let Some(j) = rmp_serde::from_read_ref(&bytes).ok() {
                j
            } else {
                return;
            }
        }
        Err(e) => {
            if let Some(reqwest::StatusCode::NOT_FOUND) = e.status() {
                warn!(log, "commits do not have perf results");
                return;
            } else {
                error!(log, "GitHub API didn't like us: {:?}", e);
                return;
            }
        }
    };

    debug!(log, "comparing old perf results";
           "ref" => &old_sha,
           "date" => format!("{}", old_date));
    debug!(log, "with new perf results";
           "ref" => &new_sha,
           "date" => format!("{}", new_date));

    let get = |ab: &str| -> Option<&serde_json::Map<String, serde_json::Value>> {
        let commit = perf.get(ab)?.as_object()?;
        commit.get("data")?.as_object()
    };
    let (perf_old, perf_new) = if let (Some(a), Some(b)) = (get("a"), get("b")) {
        (a, b)
    } else {
        error!(log, "failed to extract commit perf data");
        return;
    };

    // each of perf_old and perf_new holds the absolute measurement values for multiple experiments
    // run on commit a and b respectively. we want to average the _percentage_ change across _all_
    // metrics. it's not great, but it's concise. we'll link to the actual compare page for
    // details.

    let mut pct_chgs = Vec::new();
    for (benchmark, results) in perf_old {
        debug!(log, "found benchmark"; "benchmark" => benchmark);
        let in_new = if let Some(b) = perf_new.get(benchmark) {
            b
        } else {
            // not in new, so we can't really compare
            debug!(log, "benchmark in old but not new");
            continue;
        };

        let (results, in_new) = if let (Some(a), Some(b)) = (results.as_array(), in_new.as_array())
        {
            (a, b)
        } else {
            debug!(log, "benchmark results aren't JSON arrays");
            continue;
        };

        for measurement in results {
            if !measurement.is_array() {
                debug!(log, "benchmark measurement isn't a JSON array");
                continue;
            }
            let measurement = measurement.as_array().unwrap();
            if measurement.len() != 2 {
                debug!(log, "benchmark measurement structure changed");
                continue;
            }
            let (measurement, old_cost) = (&measurement[0], &measurement[1]);
            if !measurement.is_string() || !old_cost.is_f64() {
                debug!(log, "benchmark measurement fields changed");
                continue;
            }
            let measurement = measurement.as_str().unwrap();
            let old_cost = old_cost.as_f64().unwrap();

            trace!(log, "found measurement"; "measurement" => measurement);
            let new_measurement = if let Some(m) = in_new.iter().find(|m| {
                if !m.is_array() {
                    return false;
                }
                let m = m.as_array().unwrap();
                m.len() == 2
                    && m[0].is_string()
                    && m[1].is_f64()
                    && m[0].as_str().unwrap() == measurement
            }) {
                m
            } else {
                // not in new, so we can't really compare
                debug!(log, "measurement in old but not new");
                continue;
            };
            let new_cost = new_measurement[1].as_f64().unwrap();

            // https://github.com/rust-lang/rustc-perf/blob/566e4d2f7728928c7c2d8ee0ae2633bc842e7273/site/static/compare.html#L92
            let percent_chg = 100.0 * (new_cost - old_cost) / old_cost;
            trace!(log, "computed change"; "change" => percent_chg);
            pct_chgs.push(percent_chg);
        }
    }

    if pct_chgs.is_empty() {
        warn!(log, "found no comparable benchmarks between nightlies");
    }

    let mean = pct_chgs.iter().sum::<f64>() / pct_chgs.len() as f64;
    let variance =
        pct_chgs.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / (pct_chgs.len() - 1) as f64;
    info!(log, "perf change";
        "percent" => format!("{:.1}%", mean),
        "variance" => format!("{:.1}%", variance));

    new.perf = Some(PerfChange { mean, variance });
}

/// Construct a tweet based on information about old and new nightly
fn new_nightly(log: &slog::Logger, new: &Nightly, old: &Nightly) -> String {
    warn!(log, "new rust release detected";
          "version" => %new.rust.number,
          "rev" => &new.rust.revision,
          "date" => %new.rust.date);

    // put github compare url in tweet
    let changes = format!(
        "https://github.com/rust-lang/rust/compare/{}...{}",
        old.rust.revision, new.rust.revision
    );
    let mut desc = format!(
        "{} @rustlang nightly is up 🎉\n",
        new.rust.date.naive_utc().date()
    );
    desc.push_str(&format!("rust 🔬: {}", changes));

    // did cargo also change?
    if new.cargo.revision != old.cargo.revision {
        // yes!
        warn!(log, "new cargo release also detected";
              "version" => %new.cargo.number,
              "rev" => &new.cargo.revision,
              "date" => %new.cargo.date);

        // put github compare url for cargo in tweet too
        let changes = format!(
            "https://github.com/rust-lang/cargo/compare/{}...{}",
            old.cargo.revision, new.cargo.revision
        );
        desc.push_str(&format!("\ncargo 🔬: {}", changes));
    }

    if let Some(ref perf) = new.perf {
        desc.push_str(&format!(
            "\nperf {}: https://perf.rust-lang.org/compare.html?start={}&end={}&stat=cpu-clock",
            perf, old.rust.revision, new.rust.revision
        ));
    }
    desc
}

/// Fetch information about the latest Rust nightly
async fn nightly() -> Result<Nightly, ManifestError> {
    let res = reqwest::get(NIGHTLY_MANIFEST)
        .await
        .map_err(|e| ManifestError::Unavailable(e))?;
    if res.status() != reqwest::StatusCode::OK {
        return Err(ManifestError::NotOk(res.status()));
    }

    // reader
    let s = res
        .text()
        .await
        .map_err(|_| ManifestError::BadManifest("invalid utf-8"))?;

    // parse
    let r: toml::Value = toml::from_str(&*s).map_err(|e| ManifestError::BadToml(e))?;
    let manifest = r
        .as_table()
        .ok_or(ManifestError::BadManifest("expected table at root"))?;

    // traverse
    let pkgs = manifest
        .get("pkg")
        .ok_or(ManifestError::BadManifest("no [pkg] section"))?
        .as_table()
        .ok_or(ManifestError::BadManifest("expected [pkg] to be table"))?;
    let cargo = pkgs
        .get("cargo")
        .ok_or(ManifestError::BadManifest("no cargo in [pkg]"))?
        .as_table()
        .ok_or(ManifestError::BadManifest("[cargo] is not a section"))?
        .get("version")
        .ok_or(ManifestError::BadManifest(
            "[cargo] does not have a version field",
        ))?
        .as_str()
        .ok_or(ManifestError::BadManifest("cargo version is not a string"))?;
    let rust = pkgs
        .get("rust")
        .ok_or(ManifestError::BadManifest("no rust in [pkg]"))?
        .as_table()
        .ok_or(ManifestError::BadManifest("[rust] is not a section"))?
        .get("version")
        .ok_or(ManifestError::BadManifest(
            "[rust] does not have a version field",
        ))?
        .as_str()
        .ok_or(ManifestError::BadManifest("rust version is not a string"))?;

    // arrange
    let cargo = Version::from_str(cargo)
        .map_err(|_| ManifestError::BadManifest("cargo had weird version"))?;
    let rust = Version::from_str(rust)
        .map_err(|_| ManifestError::BadManifest("rust had weird version"))?;

    Ok(Nightly {
        cargo,
        rust,
        perf: None,
    })
}

#[derive(Debug)]
enum ManifestError {
    Unavailable(reqwest::Error),
    NotOk(reqwest::StatusCode),
    BadToml(toml::de::Error),
    BadManifest(&'static str),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match *self {
            ManifestError::Unavailable(ref e) => write!(f, "manifest unavailable: {}", e),
            ManifestError::NotOk(ref s) => write!(f, "manifest returned {}", s),
            ManifestError::BadToml(ref e) => write!(f, "manifest not valid toml: {}", e),
            ManifestError::BadManifest(e) => write!(f, "manifest malformed: {}", e),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct VersionNumber(usize, usize, usize);

impl fmt::Display for VersionNumber {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Version {
    number: VersionNumber,
    revision: String,
    // NOTE: needs to be a DateTime to be Serialize
    date: DateTime<Utc>,
}

use std::str::FromStr;
impl FromStr for Version {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // 0.20.0-nightly (13d92c64d 2017-05-12)
        use regex::Regex;
        let re = Regex::new(
            r"^(rustc |cargo )?(\d+)\.(\d+)\.(\d+)-nightly \(([0-9a-f]+) (\d{4}-\d{2}-\d{2})\)$",
        )
        .unwrap();
        let matches = re
            .captures(s)
            .ok_or(())
            .expect("are you sure you ran with +nightly ?");
        Ok(Version {
            number: VersionNumber(
                usize::from_str(&matches[2]).unwrap(),
                usize::from_str(&matches[3]).unwrap(),
                usize::from_str(&matches[4]).unwrap(),
            ),
            revision: matches[5].to_string(),
            date: Date::from_utc(
                NaiveDate::parse_from_str(&matches[6], "%Y-%m-%d").unwrap(),
                Utc,
            )
            .and_hms(0, 0, 0),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PerfChange {
    mean: f64,
    variance: f64,
}

impl fmt::Display for PerfChange {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        if self.mean > 0f64 {
            // positive means compile time went *up*
            // which means speed (⚡) went down
            write!(f, "📉 {:.1}%±{:.1}%", self.mean, self.variance)
        } else {
            write!(f, "📈 {:.1}%±{:.1}%", -self.mean, self.variance)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Nightly {
    cargo: Version,
    rust: Version,
    perf: Option<PerfChange>,
}
