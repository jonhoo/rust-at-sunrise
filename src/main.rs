#[macro_use]
extern crate slog;
#[macro_use]
extern crate serde_derive;

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::path::Path;
use std::thread;
use std::time;

use chrono::prelude::*;
use clap::{crate_version, App, Arg};
use egg_mode::tweet::DraftTweet;

const CONSUMER_KEY: &'static str = "XurcamcbIvruiowuIuLLxpkEV";
const ACCESS_TOKEN: &'static str = "864346480437469185-itNALA4j82KEdvYg8Mh1XLZoYdHTiLK";
const NIGHTLY_MANIFEST: &'static str =
    "https://static.rust-lang.org/dist/channel-rust-nightly.toml";

fn main() {
    // we want to log things
    use slog::Drain;
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();
    let log = slog::Logger::root(drain, o!());

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
            match nightly() {
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

    let mut rt = tokio::runtime::current_thread::Runtime::new().unwrap();
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

        match rt.block_on(egg_mode::service::config(&token)) {
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
        match nightly() {
            Ok(mut nightly) => {
                // did nightly change?
                if !last.rust.revision.starts_with(&nightly.rust.revision) {
                    fill_perf(&log, &mut nightly, &mut last);
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

                                let draft = DraftTweet::new(&*tweet);
                                if let Err(e) = rt.block_on(draft.send(&token)) {
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
                break;
            }
        }

        if matches.is_present("dry") {
            warn!(log, "exiting early since we're doing a dry run");
            break;
        }
        thread::sleep(time::Duration::from_secs(30 * 60));
    }
}

fn find_perf_before(
    log: &slog::Logger,
    n: &mut Nightly,
    stop_before: Option<&str>,
) -> Option<(String, DateTime<Utc>, HashMap<String, f64>)> {
    let bors_merge_commits = format!(
        "https://api.github.com/repos/rust-lang/rust/commits?author=bors&sha={}",
        n.rust.revision
    );
    let bors_merge_commits = reqwest::get(&bors_merge_commits)
        .ok()?
        .json::<serde_json::Value>()
        .ok()?;
    let bors_merge_commits = bors_merge_commits.as_array()?;
    debug!(log, "looking for perf results for {}", n.rust.revision);

    for commit in bors_merge_commits {
        // in reverse chronological order, staring with the target commit (n.rust.revision)
        let commit = commit.as_object()?;
        let sha = commit["sha"].as_str()?;
        debug!(log, "checking commit {}", sha);
        if sha.starts_with(&n.rust.revision) {
            // keep track of the full revision hash
            debug!(log, "expanded {} to {}", n.rust.revision, sha);
            n.rust.revision = sha.to_owned();
        }
        let commit = commit["commit"].as_object()?;

        if let Some(stop) = stop_before {
            // make sure we don't run further back that, say, previous nightly
            if sha.starts_with(stop) {
                debug!(log, "stoppping at this commit as instructed");
                break;
            }
        }

        let message = commit["message"].as_str()?.lines().next()?;
        if message.contains("r=try") {
            // don't know if these would ever even appear here?
            // but just to make sure...
            warn!(log, "ignoring r=try commit");
            continue;
        }

        // just for debugging really
        let date = commit["committer"].as_object()?["date"]
            .as_str()?
            .parse::<DateTime<Utc>>()
            .ok()?;

        // see if we have a perf result for the current commit
        let perf: serde_json::Value = match reqwest::get(&format!(
            "https://raw.githubusercontent.com/\
             rust-lang-nursery/rustc-timing/master/\
             times/commit-{}-x86_64-unknown-linux-gnu.json.sz",
            sha
        )) {
            Ok(r) => {
                use std::io::Read;
                let mut out = Vec::new();
                let mut szip_reader = snap::Reader::new(r);
                szip_reader.read_to_end(&mut out).ok()?;
                serde_json::from_slice(&out).ok()?
            }
            Err(e) => {
                if let Some(reqwest::StatusCode::NOT_FOUND) = e.status() {
                    // move on to an earlier commit
                    debug!(log, "commit has no perf results");
                    continue;
                } else {
                    // some other error? give up.
                    error!(log, "GitHub API didn't like us: {:?}", e);
                    return None;
                }
            }
        };

        let benchmarks = perf["benchmarks"].as_object()?;
        let mut ts = HashMap::new();
        for (benchmark, v) in benchmarks {
            let v = match v.get("Ok") {
                None => continue,
                Some(v) => v,
            };
            let v = v.get(0).unwrap_or(v);
            let runs = match v.get("runs").and_then(|v| v.as_array()) {
                None => continue,
                Some(v) => v,
            };

            trace!(log, "collecting perf results for '{}' benchmark", benchmark);
            let mut t = 0.0;
            for run in runs {
                let v = match run.get("stats") {
                    None => continue,
                    Some(v) => v,
                };
                // WHY?!
                let v = match v.get("stats") {
                    None => continue,
                    Some(v) => v,
                };
                let v = match v.as_array() {
                    None => continue,
                    Some(v) => v,
                };
                // [9] is cpu-clock: https://github.com/rust-lang-nursery/rustc-perf/blob/31b65db74e034d7e82c6aa9a0c1710170f0a1700/collector/src/lib.rs#L298
                if let Some(&serde_json::Value::Number(ref n)) = v.get(9) {
                    if let Some(v) = n.as_f64() {
                        t += v;
                    }
                }
            }
            ts.insert(benchmark.to_owned(), t);
        }

        return Some((sha.to_owned(), date, ts));
    }
    None
}

fn fill_perf(log: &slog::Logger, new: &mut Nightly, old: &mut Nightly) {
    let perf_old = match find_perf_before(log, old, None) {
        Some(p) => p,
        None => {
            error!(
                log,
                "could not find old perf data for commit {}", old.rust.revision
            );
            return;
        }
    };
    let perf_new = match find_perf_before(log, new, Some(&old.rust.revision)) {
        Some(p) => p,
        None => {
            error!(
                log,
                "could not find old perf data for commit {}", old.rust.revision
            );
            return;
        }
    };

    debug!(log, "comparing old perf results";
           "ref" => &perf_old.0,
           "date" => format!("{}", perf_old.1));
    debug!(log, "with new perf results";
           "ref" => &perf_new.0,
           "date" => format!("{}", perf_new.1));

    // we want to compute the average improvement in time
    let mut time_old = 0f64;
    let mut time_new = 0f64;
    let mut time_imp = 0f64;
    let mut n = 0;
    for (benchmark, ntime) in perf_new.2 {
        if ntime == 0.0 {
            continue;
        }
        if let Some(&otime) = perf_old.2.get(&benchmark) {
            if otime != 0.0 {
                time_old += otime;
                time_new += ntime;
                time_imp += (ntime - otime) / otime;
                n += 1;
            }
        }
    }

    time_imp *= 100f64;
    time_imp /= n as f64;
    info!(log, "perf improvements";
          "change" => format!("{:.1}%", time_imp),
          "old" => format!("{:.1}", time_old),
          "new" => format!("{:.1}", time_new));

    new.perf = Some(PerfChange { time: time_imp });
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
fn nightly() -> Result<Nightly, ManifestError> {
    let mut res = reqwest::get(NIGHTLY_MANIFEST).map_err(|e| ManifestError::Unavailable(e))?;
    if res.status() != reqwest::StatusCode::OK {
        return Err(ManifestError::NotOk(res.status()));
    }

    // reader
    let s = res
        .text()
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
        let matches = re.captures(s).ok_or(())?;
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
    time: f64,
}

impl fmt::Display for PerfChange {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        if self.time > 0f64 {
            // positive means compile time went *up*
            // which means speed (⚡) went down
            write!(f, "📉 {:.1}%", self.time)
        } else {
            write!(f, "📈 {:.1}%", -self.time)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Nightly {
    cargo: Version,
    rust: Version,
    perf: Option<PerfChange>,
}
