extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate tokio_core;

extern crate chrono;
#[macro_use]
extern crate clap;
extern crate egg_mode;
extern crate regex;
extern crate serde_json;
#[macro_use]
extern crate slog;
extern crate slog_term;
extern crate toml;

use std::thread;
use std::sync;
use std::time;
use std::fmt;
use std::env;
use std::path::Path;
use std::process::Command;

use chrono::prelude::*;
use egg_mode::tweet::DraftTweet;
use clap::{App, Arg};
use futures::{Future, Stream};

const CONSUMER_KEY: &'static str = "XurcamcbIvruiowuIuLLxpkEV";
const ACCESS_TOKEN: &'static str = "864346480437469185-itNALA4j82KEdvYg8Mh1XLZoYdHTiLK";
const NIGHTLY_MANIFEST: &'static str = "https://static.rust-lang.org/dist/channel-rust-nightly.toml";

fn main() {
    // we want to log things
    use slog::Drain;
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::CompactFormat::new(decorator).build().fuse();
    let drain = sync::Mutex::new(drain).fuse();
    let log = slog::Logger::root(drain, o!());

    // argument parsing
    let matches = App::new("Rust at Sunrise")
        .version(crate_version!())
        .author("Jon Gjengset <jon@thesqsuareplanet.com>")
        .about(
            "Tweets information about newest Rust Nightly to @rust_at_sunrise",
        )
        .arg(
            Arg::with_name("dry")
                .help("Do a dry run which does not loop or post to twitter")
                .short("d")
                .long("dry-run"),
        )
        .arg(
            Arg::with_name("RUSTV")
                .help("Last rust nightly version string")
                .required(true)
                .index(1),
        )
        .arg(
            Arg::with_name("CARGOV")
                .help("Last cargo nightly version string")
                .required(true)
                .index(2),
        )
        .get_matches();

    // what is current nightly?
    let mut last = Nightly {
        rust: Version::from_str(matches.value_of("RUSTV").unwrap()).unwrap(),
        cargo: Version::from_str(matches.value_of("CARGOV").unwrap()).unwrap(),
        perf: None,
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
        match egg_mode::service::config(&token) {
            Ok(c) => Some((token, c)),
            Err(_) if matches.is_present("dry") => None,
            Err(e) => {
                crit!(log, "failed to get twitter config: {}", e);
                return;
            }
        }
    };

    // initialize perf data repository
    let perf = Path::new("./.sunrise-perf-data");
    if !perf.exists() {
        info!(log, "cloning perf repository");
        let url = "https://github.com/rust-lang-nursery/rustc-timing.git";
        match Command::new("git")
            .args(&["clone", url, &*perf.to_string_lossy()])
            .status()
        {
            Ok(ref s) if s.success() => {}
            Ok(ref s) => {
                crit!(log, "failed to clone perf timing repository: {}", s);
                return;
            }
            Err(e) => {
                crit!(log, "failed to clone perf timing repository: {}", e);
                return;
            }
        }
    } else {
        info!(log, "using existing perf repository clone");
    }

    // and then we loop
    loop {
        match nightly() {
            Ok(mut nightly) => {
                // did nightly change?
                if nightly.rust.revision != last.rust.revision {
                    fill_perf(&log, perf, &mut nightly, &last);
                    let tweet = new_nightly(&log, &nightly, &last);
                    last = nightly;

                    if let Some((ref token, ref config)) = twitter {
                        match egg_mode::text::character_count(
                            &*tweet,
                            config.short_url_length,
                            config.short_url_length_https,
                        ) {
                            (chars, true) if matches.is_present("dry") => {
                                info!(log, "would have tweeted"; "chars" => chars);
                                println!("{}", tweet);
                            }
                            (chars, true) => {
                                info!(log, "tweeting"; "chars" => chars);
                                println!("{}", tweet);

                                let draft = DraftTweet::new(&*tweet);
                                if let Err(e) = draft.send(&token) {
                                    error!(log, "could not tweet: {}", e);
                                }
                            }
                            (chars, false) => {
                                error!(log, "tweet is too long"; "chars" => chars);
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
            }
        }

        if matches.is_present("dry") {
            warn!(log, "exiting early since we're doing a dry run");
            break;
        }
        thread::sleep(time::Duration::from_secs(30 * 60));
    }
}

fn fill_perf(log: &slog::Logger, perf: &std::path::Path, new: &mut Nightly, old: &Nightly) {
    // fetch any new timing results
    match Command::new("git")
        .args(&["-C", &*perf.to_string_lossy(), "fetch", "origin"])
        .status()
    {
        Ok(ref s) if s.success() => {}
        Ok(ref s) => {
            error!(log, "failed to update perf timing repository: {}", s);
            return;
        }
        Err(e) => {
            error!(log, "failed to update perf timing repository: {}", e);
            return;
        }
    }
    // instead of git pull (which doesn't work with forced pushes), we do a fetch+reset
    match Command::new("git")
        .args(&[
            "-C",
            &*perf.to_string_lossy(),
            "reset",
            "--hard",
            "origin/master",
        ])
        .status()
    {
        Ok(ref s) if s.success() => {}
        Ok(ref s) => {
            error!(log, "failed to update perf timing repository: {}", s);
            return;
        }
        Err(e) => {
            error!(log, "failed to update perf timing repository: {}", e);
            return;
        }
    }

    // iterate through the directory
    use std::fs;
    use std::collections::BTreeMap;
    // filenames are on the form
    // 2017-02-23T16:56:13+00:00-<commit hash>-x86_64-unknown-linux-gnu.json
    // we don't want to parse and insert *all* files, because that'd be quite slow. instead, we
    // only consider those that have happened in the two months represented by old and new.
    let oldm = format!("{}-{:02}-", old.rust.date.year(), old.rust.date.month());
    let newm = format!("{}-{:02}-", new.rust.date.year(), new.rust.date.month());
    let newm = if newm == oldm { None } else { Some(newm) };
    let mut perfs = BTreeMap::new();
    for path in fs::read_dir(perf.join("times")).unwrap() {
        let path = match path {
            Ok(f) => f,
            Err(_) => continue,
        };
        let f = path.file_name();
        let f = f.to_str().unwrap();
        if !f.starts_with(&oldm) {
            if let Some(ref newm) = newm {
                if !f.starts_with(newm) {
                    // not in either interesting month
                    continue;
                }
            } else {
                // not in the one interesting month
                continue;
            }
        }
        if !f.ends_with("-x86_64-unknown-linux-gnu.json") {
            // wrong target
            continue;
        }
        // in an interesting month!
        let (at, _) = f.split_at(25); // get only the timestamp
        let at = match at.parse::<DateTime<Utc>>() {
            Ok(d) => d,
            Err(e) => {
                warn!(log, "could not parse timing date '{}': {}", at, e);
                continue;
            }
        };
        perfs.insert(at, perf.join("times").join(f));
    }

    // find benchmark closest to old and closest to new
    let closest_to = |d| {
        let gt = perfs.range(d..).next();
        let lt = perfs.range(..d).next_back();
        match (lt, gt) {
            (Some((_, f)), None) => f,
            (None, Some((_, f))) => f,
            (Some((lt_d, lt_f)), Some((gt_d, gt_f))) => {
                if d.signed_duration_since(*lt_d) < gt_d.signed_duration_since(d) {
                    // lt is closer
                    lt_f
                } else {
                    gt_f
                }
            }
            (None, None) => {
                // this means there are no timings, which shouldn't happen
                crit!(log, "no perf timing information available");
                unreachable!();
            }
        }
    };
    let near_old = closest_to(old.rust.date.and_hms(0, 0, 0));
    let near_new = closest_to(new.rust.date.and_hms(0, 0, 0));

    let reduce = |f| -> Result<_, Box<std::error::Error>> {
        use std::collections::HashMap;
        let f = std::fs::File::open(f)?;
        let v: serde_json::Value = serde_json::from_reader(f)?;
        let mut ts = HashMap::new();
        let (commit, date) = match v.get("commit") {
            Some(v) => (
                v.get("sha").and_then(|v| v.as_str()).map(|v| v.to_owned()),
                v.get("date")
                    .and_then(|v| v.as_str())
                    .and_then(|v| v.parse::<DateTime<Utc>>().ok()),
            ),
            None => (None, None),
        };
        match v.get("benchmarks") {
            Some(b) => {
                if !b.is_object() {
                    Err("benchmarks not a map")?;
                }
                for (benchmark, v) in b.as_object().unwrap() {
                    let mut t = 0.0;
                    let v = match v.get("Ok") {
                        None => continue,
                        Some(v) => v,
                    };
                    let v = match v.get(0) {
                        None => continue,
                        Some(v) => v,
                    };
                    let v = match v.get("runs") {
                        None => continue,
                        Some(v) => v,
                    };
                    let v = match v.get(0) {
                        None => continue,
                        Some(v) => v,
                    };
                    let v = match v.get("stats") {
                        None => continue,
                        Some(v) => v,
                    };
                    let v = match v.as_array() {
                        None => continue,
                        Some(v) => v,
                    };
                    for v in v {
                        match v.get("name") {
                            Some(&serde_json::Value::String(ref s)) if s == "cpu-clock" => {
                                if let Some(v) = v.get("cnt").and_then(|v| v.as_f64()) {
                                    t += v;
                                }
                            }
                            _ => continue,
                        }
                    }
                    ts.insert(benchmark.to_owned(), t);
                }
            }
            None => Err("no benchmark")?,
        }
        Ok((commit, date, ts))
    };

    let perf_old = match reduce(near_old) {
        Ok(p) => p,
        Err(e) => {
            error!(log, "could not parse old perf data: {}", e);
            return;
        }
    };
    let perf_new = match reduce(near_new) {
        Ok(p) => p,
        Err(e) => {
            error!(log, "could not parse new perf data: {}", e);
            return;
        }
    };

    debug!(log, "comparing old perf results";
           "ref" => perf_old.0,
           "date" => perf_old.1.map(|v| format!("{}", v)));
    debug!(log, "with new perf results";
           "ref" => perf_new.0,
           "date" => perf_new.1.map(|v| format!("{}", v)));

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
        old.rust.revision,
        new.rust.revision
    );
    let mut desc = format!(
        "{} @rustlang nightly is up 🎉\n",
        new.rust.date.naive_utc()
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
            old.cargo.revision,
            new.cargo.revision
        );
        desc.push_str(&format!("\ncargo 🔬: {}", changes));
    }

    if let Some(ref perf) = new.perf {
        desc.push_str(&format!(
            "\nperf {}: http://perf.rust-lang.org/graphs.html",
            perf
        ));
    }
    desc
}

/// Fetch information about the latest Rust nightly
fn nightly() -> Result<Nightly, ManifestError> {
    // we want tls
    let mut core = tokio_core::reactor::Core::new().unwrap();
    let client = hyper::Client::configure()
        .connector(hyper_tls::HttpsConnector::new(4, &core.handle()).unwrap())
        .build(&core.handle());

    // download
    let res = core.run(client.request(hyper::Request::new(
        hyper::Method::Get,
        NIGHTLY_MANIFEST.parse().unwrap(),
    ))).map_err(|e| ManifestError::Unavailable(e))?;
    if res.status() != hyper::Ok {
        return Err(ManifestError::NotOk(res.status()));
    }

    // reader
    let s = core.run(
        res.body()
            .concat2()
            .map_err(|e| ManifestError::LostConnection(e))
            .and_then(|s| {
                String::from_utf8(s.to_vec())
                    .map_err(|_| ManifestError::BadManifest("invalid utf-8"))
            }),
    )?;

    // parse
    let r: toml::Value = toml::from_str(&*s).map_err(|e| ManifestError::BadToml(e))?;
    let manifest = r.as_table()
        .ok_or(ManifestError::BadManifest("expected table at root"))?;

    // traverse
    let pkgs = manifest
        .get("pkg")
        .ok_or(ManifestError::BadManifest("no [pkg] section"))?
        .as_table()
        .ok_or(ManifestError::BadManifest("expected [pkg] to be table"))?;
    let cargo = pkgs.get("cargo")
        .ok_or(ManifestError::BadManifest("no cargo in [pkg]"))?
        .as_table()
        .ok_or(ManifestError::BadManifest("[cargo] is not a section"))?
        .get("version")
        .ok_or(ManifestError::BadManifest(
            "[cargo] does not have a version field",
        ))?
        .as_str()
        .ok_or(ManifestError::BadManifest("cargo version is not a string"))?;
    let rust = pkgs.get("rust")
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
    let rust =
        Version::from_str(rust).map_err(|_| ManifestError::BadManifest("rust had weird version"))?;

    Ok(Nightly {
        cargo,
        rust,
        perf: None,
    })
}

enum ManifestError {
    Unavailable(hyper::error::Error),
    NotOk(hyper::StatusCode),
    LostConnection(hyper::Error),
    BadToml(toml::de::Error),
    BadManifest(&'static str),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match *self {
            ManifestError::Unavailable(ref e) => write!(f, "manifest unavailable: {}", e),
            ManifestError::NotOk(ref s) => write!(f, "manifest returned {}", s),
            ManifestError::LostConnection(ref e) => write!(f, "manifest unreadable: {}", e),
            ManifestError::BadToml(ref e) => write!(f, "manifest not valid toml: {}", e),
            ManifestError::BadManifest(e) => write!(f, "manifest malformed: {}", e),
        }
    }
}

struct VersionNumber(usize, usize, usize);

impl fmt::Display for VersionNumber {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

struct Version {
    number: VersionNumber,
    revision: String,
    date: Date<Utc>,
}

use std::str::FromStr;
impl FromStr for Version {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // 0.20.0-nightly (13d92c64d 2017-05-12)
        use regex::Regex;
        let re = Regex::new(
            r"^(rustc |cargo )?(\d+)\.(\d+)\.(\d+)-nightly \(([0-9a-f]+) (\d{4}-\d{2}-\d{2})\)$",
        ).unwrap();
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
            ),
        })
    }
}

struct PerfChange {
    time: f64,
}

impl fmt::Display for PerfChange {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        if self.time == 0f64 {
            write!(f, "unchanged")
        } else if self.time > 0f64 {
            // positive means compile time went *up*
            // which means speed (⚡) went down
            write!(f, "📉 {:.1}%", self.time)
        } else {
            write!(f, "📈 {:.1}%", -self.time)
        }
    }
}

struct Nightly {
    cargo: Version,
    rust: Version,
    perf: Option<PerfChange>,
}
