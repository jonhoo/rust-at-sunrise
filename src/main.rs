extern crate hyper_native_tls;
extern crate egg_mode;
extern crate chrono;
extern crate regex;
extern crate hyper;
extern crate serde;
extern crate toml;
#[macro_use]
extern crate clap;
#[macro_use]
extern crate slog;
extern crate slog_term;
extern crate rustc_perf;
extern crate serde_json;

use std::thread;
use std::sync;
use std::time;
use std::fmt;
use std::env;
use std::io;
use std::io::prelude::*;

use chrono::naive::date::NaiveDate;
use chrono::offset::utc::UTC;
use chrono::date::Date;
use egg_mode::tweet::DraftTweet;
use clap::{Arg, App};

const CONSUMER_KEY: &'static str = "XurcamcbIvruiowuIuLLxpkEV";
const ACCESS_TOKEN: &'static str = "864346480437469185-itNALA4j82KEdvYg8Mh1XLZoYdHTiLK";
const NIGHTLY_MANIFEST: &'static str = "https://static.rust-lang.org/dist/channel-rust-nightly.toml";
const PERF_ENDPOINT: &'static str = "http://perf.rust-lang.org/perf/get";

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
        .about("Tweets information about newest Rust Nightly to @rust_at_sunrise")
        .arg(Arg::with_name("dry")
                 .help("Do a dry run which does not loop or post to twitter")
                 .short("d")
                 .long("dry-run"))
        .arg(Arg::with_name("RUSTV")
                 .help("Last rust nightly version string")
                 .required(true)
                 .index(1))
        .arg(Arg::with_name("CARGOV")
                 .help("Last cargo nightly version string")
                 .required(true)
                 .index(2))
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

    let twitter = if matches.is_present("dry") {
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
        let config = match egg_mode::service::config(&token) {
            Ok(c) => c,
            Err(e) => {
                crit!(log, "failed to get twitter config: {}", e);
                return;
            }
        };
        Some((token, config))
    };

    // and then we loop
    loop {
        match nightly() {
            Ok(mut nightly) => {
                // did nightly change?
                if nightly.rust.revision != last.rust.revision {
                    fill_perf(&log, &mut nightly, &last);
                    let tweet = new_nightly(&log, &nightly, &last);
                    last = nightly;

                    if let Some((ref token, ref config)) = twitter {
                        match egg_mode::text::character_count(&*tweet,
                                                              config.short_url_length,
                                                              config.short_url_length_https) {
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

fn fill_perf(log: &slog::Logger, new: &mut Nightly, old: &Nightly) {
    // we want tls
    let ssl = hyper_native_tls::NativeTlsClient::new().unwrap();
    let connector = hyper::net::HttpsConnector::new(ssl);
    let client = hyper::Client::with_connector(connector);

    // get perf info
    let perf_req = rustc_perf::api::days::Request {
        // date_a searches for first following test, and falls back to first sample
        // XXX: which is really weird btw...
        // see rustc_perf::util::get_commit_data_from_start
        date_a: rustc_perf::date::OptionalDate::new(old.rust.date.and_hms(23, 59, 59).into()),
        // date_a searches for first following test, and falls back to last sample
        // see rustc_perf::util::get_commit_data_from_start
        date_b: rustc_perf::date::OptionalDate::new(new.rust.date.and_hms(23, 59, 59).into()),
        group_by: rustc_perf::server::GroupBy::Crate,
        crates: rustc_perf::api::List::All,
        phases: rustc_perf::api::List::All,
    };
    let req = serde_json::to_string(&perf_req).unwrap();
    let req = hyper::client::Body::BufBody(req.as_bytes(), req.as_bytes().len());

    match client.post(PERF_ENDPOINT).body(req).send() {
        Err(e) => {
            error!(log, "failed to get perf data: {}", e);
        }
        Ok(res) => {
            if res.status != hyper::Ok {
                error!(log, "perf did not give 200 OK"; "gave" => %res.status);
                return;
            }

            match serde_json::from_reader(res) {
                Ok(res) => {
                    let res: rustc_perf::api::days::Response = res;
                    // res.a is first benchmark following old
                    // res.b is first benchmark following new
                    // note that they will both be for the day *following* the date of the nightly
                    if res.a.date.0.date() != old.rust.date.succ() {
                        error!(log, "old perf benchmark not found";
                                "needed" => %old.rust.date,
                                "found" => %res.a.date.0.date());
                        return;
                    }
                    if res.a.commit == res.b.commit {
                        // this serves stale results, but it's better than nothing
                        error!(log, "new perf benchmark not found";
                                "needed" => %new.rust.date,
                                "found" => %res.b.date.0.date());
                        return;
                    }

                    debug!(log, "comparing perf results";
                           "old" => res.a.commit,
                           "new" => res.b.commit);

                    // we want to compute the average improvement in time + rss
                    let mut time_imp = 0f64;
                    let mut rss_imp = 0f64;
                    let mut n = 0;
                    for (crt, newrec) in &res.b.data {
                        if let Some(oldrec) = res.a.data.get(crt) {
                            time_imp += (newrec.time - oldrec.time) / oldrec.time;
                            rss_imp += (newrec.rss as f64 - oldrec.rss as f64) / oldrec.rss as f64;
                            n += 1;
                        }
                    }

                    time_imp *= 100f64;
                    time_imp /= n as f64;
                    rss_imp *= 100f64;
                    rss_imp /= n as f64;
                    info!(log, "perf improvements";
                          "time" => format!("{:.1}%", time_imp),
                          "rss" => format!("{:.1}%", rss_imp));

                    new.perf = Some(PerfChange {
                                        time: time_imp,
                                        rss: rss_imp,
                                    });
                }
                Err(e) => {
                    error!(log, "malformed response from perf: {}", e);
                }
            }
        }
    }
}


/// Construct a tweet based on information about old and new nightly
fn new_nightly(log: &slog::Logger, new: &Nightly, old: &Nightly) -> String {
    warn!(log, "new rust release detected";
          "version" => %new.rust.number,
          "rev" => &new.rust.revision,
          "date" => %new.rust.date);

    // put github compare url in tweet
    let changes = format!("https://github.com/rust-lang/rust/compare/{}...{}",
                          old.rust.revision,
                          new.rust.revision);
    let mut desc = format!("{} nightly released\n", new.rust.date.naive_utc());
    desc.push_str(&format!("rust diff: {}", changes));

    // did cargo also change?
    if new.cargo.revision != old.cargo.revision {
        // yes!
        warn!(log, "new cargo release also detected";
              "version" => %new.cargo.number,
              "rev" => &new.cargo.revision,
              "date" => %new.cargo.date);

        // put github compare url for cargo in tweet too
        let changes = format!("https://github.com/rust-lang/cargo/compare/{}...{}",
                              old.cargo.revision,
                              new.cargo.revision);
        desc.push_str(&format!("\ncargo diff {}", changes));
    }

    if let Some(ref perf) = new.perf {
        desc.push_str(&format!("\nperf {}: http://perf.rust-lang.org/graphs.html", perf));
    }
    desc
}

/// Fetch information about the latest Rust nightly
fn nightly() -> Result<Nightly, ManifestError> {
    // we want tls
    let ssl = hyper_native_tls::NativeTlsClient::new().unwrap();
    let connector = hyper::net::HttpsConnector::new(ssl);
    let client = hyper::Client::with_connector(connector);

    // download
    let mut res = client
        .get(NIGHTLY_MANIFEST)
        .send()
        .map_err(|e| ManifestError::Unavailable(e))?;
    if res.status != hyper::Ok {
        return Err(ManifestError::NotOk(res.status));
    }

    // reader
    let mut s = String::new();
    res.read_to_string(&mut s)
        .map_err(|e| ManifestError::LostConnection(e))?;

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
        .ok_or(ManifestError::BadManifest("[cargo] does not have a version field"))?
        .as_str()
        .ok_or(ManifestError::BadManifest("cargo version is not a string"))?;
    let rust = pkgs.get("rust")
        .ok_or(ManifestError::BadManifest("no rust in [pkg]"))?
        .as_table()
        .ok_or(ManifestError::BadManifest("[rust] is not a section"))?
        .get("version")
        .ok_or(ManifestError::BadManifest("[rust] does not have a version field"))?
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

enum ManifestError {
    Unavailable(hyper::error::Error),
    NotOk(hyper::status::StatusCode),
    LostConnection(io::Error),
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
    date: Date<UTC>,
}

use std::str::FromStr;
impl FromStr for Version {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // 0.20.0-nightly (13d92c64d 2017-05-12)
        use regex::Regex;
        let re = Regex::new(r"^(rustc |cargo )?(\d+)\.(\d+)\.(\d+)-nightly \(([0-9a-f]+) (\d{4}-\d{2}-\d{2})\)$")
            .unwrap();
        let matches = re.captures(s).ok_or(())?;
        Ok(Version {
               number: VersionNumber(usize::from_str(&matches[2]).unwrap(),
                                     usize::from_str(&matches[3]).unwrap(),
                                     usize::from_str(&matches[4]).unwrap()),
               revision: matches[5].to_string(),
               date: Date::from_utc(NaiveDate::parse_from_str(&matches[6], "%Y-%m-%d").unwrap(),
                                    UTC),
           })
    }
}

struct PerfChange {
    time: f64,
    #[allow(dead_code)]
    rss: f64,
}

impl fmt::Display for PerfChange {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        if self.time == 0f64 {
            write!(f, "unchanged")
        } else if self.time > 0f64 {
            // positive means compile time went *up*
            // which means speed (⚡) went down
            write!(f, "down {:.1}%", self.time)
        } else {
            write!(f, "up {:.1}%", -self.time)
        }
    }
}

struct Nightly {
    cargo: Version,
    rust: Version,
    perf: Option<PerfChange>,
}
