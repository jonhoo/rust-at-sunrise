extern crate hyper_native_tls;
extern crate egg_mode;
extern crate chrono;
extern crate regex;
extern crate hyper;
extern crate serde;
extern crate toml;

use std::thread;
use std::time;
use std::env;
use std::io;
use std::io::prelude::*;

use chrono::naive::date::NaiveDate;
use egg_mode::tweet::DraftTweet;

const CONSUMER_KEY: &'static str = "XurcamcbIvruiowuIuLLxpkEV";
const ACCESS_TOKEN: &'static str = "864346480437469185-itNALA4j82KEdvYg8Mh1XLZoYdHTiLK";
const NIGHTLY_MANIFEST: &'static str = "https://static.rust-lang.org/dist/channel-rust-nightly.toml";

struct Version {
    number: (usize, usize, usize),
    revision: String,
    date: NaiveDate,
}

use std::str::FromStr;
impl FromStr for Version {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // 0.20.0-nightly (13d92c64d 2017-05-12)
        use regex::Regex;
        let re = Regex::new(r"^(\d+)\.(\d+)\.(\d+)-nightly \(([0-9a-f]+) (\d{4}-\d{2}-\d{2})\)$")
            .unwrap();
        let matches = re.captures(s).ok_or(())?;
        Ok(Version {
               number: (usize::from_str(&matches[1]).unwrap(),
                        usize::from_str(&matches[2]).unwrap(),
                        usize::from_str(&matches[3]).unwrap()),
               revision: matches[4].to_string(),
               date: NaiveDate::parse_from_str(&matches[5], "%Y-%m-%d").unwrap(),
           })
    }
}

struct Latest {
    cargo: Version,
    rust: Version,
}

fn main() {
    println!("! authenticating with twitter");
    let con_secret = env::var("CONSUMER_SECRET").unwrap();
    let access_secret = env::var("ACCESS_TOKEN_SECRET").unwrap();
    let con_token = egg_mode::KeyPair::new(CONSUMER_KEY, con_secret);
    let access_token = egg_mode::KeyPair::new(ACCESS_TOKEN, access_secret);
    let token = egg_mode::Token::Access {
        consumer: con_token,
        access: access_token,
    };

    // yeah yeah i know i know
    let mut last = Latest {
        cargo: Version {
            number: (0, 20, 0),
            date: NaiveDate::from_ymd(2017, 5, 12),
            revision: "615565363".to_string(),
        },
        rust: Version {
            number: (1, 19, 0),
            date: NaiveDate::from_ymd(2017, 5, 14),
            revision: "386b0b9d3".to_string(),
        },
    };

    loop {
        let nightly = latest().unwrap();

        // did nightly change?
        if nightly.rust.revision != last.rust.revision {
            // yes!
            println!("!> new rust release detected ({}; {}..{})",
                     nightly.rust.date,
                     last.rust.revision,
                     nightly.rust.revision);

            // put github compare url in tweet
            let changes = format!("https://github.com/rust-lang/rust/compare/{}...{}",
                                  last.rust.revision,
                                  nightly.rust.revision);
            let mut draft = format!("{} nightly has been released.\n", nightly.rust.date);
            draft.push_str(&format!("Changes in Rust: {}", changes));

            // did cargo also change?
            if nightly.cargo.revision != last.cargo.revision {
                // yes!
                println!("!> new cargo release detected ({}; {}..{})",
                         nightly.cargo.date,
                         last.cargo.revision,
                         nightly.cargo.revision);

                // put github compare url for cargo in tweet too
                let changes = format!("https://github.com/rust-lang/cargo/compare/{}...{}",
                                      last.cargo.revision,
                                      nightly.cargo.revision);
                draft.push_str(&format!("\nChanges in Cargo: {}", changes));
            }

            // time to tweet!
            println!("! tweet will be:\n{}", draft);
            let draft = DraftTweet::new(&*draft);
            draft.send(&token).unwrap();

            last = nightly;
        }

        thread::sleep(time::Duration::from_secs(30 * 60));
    }
}

#[derive(Debug)]
enum ManifestError {
    Unavailable(hyper::error::Error),
    NotOk(hyper::status::StatusCode),
    LostConnection(io::Error),
    BadToml(toml::de::Error),
    BadManifest(&'static str),
}

fn latest() -> Result<Latest, ManifestError> {
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

    Ok(Latest { cargo, rust })
}
