use anyhow::Context;
use clap::{Parser, Subcommand};
use hashes::Hashes;
use serde::Deserialize;
use serde_bencode;
use serde_json;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Decode { value: String },
    Info { torrent: PathBuf },
}

/// Metainfo files (also known as .torrent files) are bencoded dictionaries with the following keys:
#[derive(Debug, Clone, Deserialize)]
struct Torrent {
    /// The URL of the tracker.
    announce: String,
    /// This maps to a dictionary, with keys described below.
    info: Info,
}

#[derive(Debug, Clone, Deserialize)]
struct Info {
    /// The suggested name to save the file (or directory) as. It is purely advisory.
    ///
    /// In single file case, the name key is the name of a file, in the multiple file case, it is the same name of a directory.
    name: String,

    /// The number of bytes in each piece the file is split into.
    ///
    /// For the purposes of transfer, files are split into fixed-size pieces which are all the same length except for possibly the last one which may be truncated. piece length is almost always a power of two, most commonly 2^18 = 256 K (BitTorrent prior to version 3.2 uses 2 20 = 1 M as default).
    #[serde(rename = "piece length")]
    plength: usize,

    /// Each entry of 'pieces' is the SHA1 hash of the piece at the corresponding index.
    pieces: Hashes,

    #[serde(flatten)]
    keys: Keys,
}

/// There is also a key 'length' or a key 'files', but not both or neither.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Keys {
    /// If 'length' is present then the download represents a single file.
    SingleFile {
        /// The length of the file in bytes.
        length: usize,
    },

    /// Otherwise it represents a set of files which go in a directory structure.
    ///
    /// For the purposes of the other keys in 'Info', the multi-file case is treated as only having a single file by concatenating the files in the order they appear in the files list.
    MultiFile {
        /// The files list is the value files maps to, and is a list of dictionaries containing the following keys:
        files: Vec<File>,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct File {
    /// The length of the file, in bytes.
    length: usize,

    /// Subdirectory names the file, the last is the actual file name (a zero length list is an error case).
    path: Vec<String>,
}

#[allow(dead_code)]
fn decode_bencoded_value(encoded_value: &str) -> (serde_json::Value, &str) {
    match encoded_value.chars().next() {
        Some('0'..='9') => {
            // If encoded_value starts with a digit, it's a string
            // Example: "5:hello" -> "hello"
            if let Some((len, rest)) = encoded_value.split_once(':') {
                if let Ok(len) = len.parse::<usize>() {
                    (rest[..len].to_string().into(), &rest[len..])
                } else {
                    (serde_json::Value::Null, encoded_value)
                }
            } else {
                (serde_json::Value::Null, encoded_value)
            }
        }
        Some('i') => {
            // If encoded_value starts with 'i', it's an integer
            // Example: "i45e" -> 45
            if let Some((integer, rest)) =
                encoded_value
                    .split_at(1)
                    .1
                    .split_once('e')
                    .and_then(|(digits, rest)| {
                        let integer = digits.parse::<i64>().ok()?;
                        Some((integer, rest))
                    })
            {
                (integer.into(), rest)
            } else {
                (serde_json::Value::Null, encoded_value)
            }
        }
        Some('l') => {
            // If encoded_value starts with 'l', it's a list
            // For example: "l5:helloi52ee" -> ["hello", 52]

            // Value type represents any JSON type; since we will have a list of JSON vlaues, we can use a Vec of Value to store all the values of the bencoded list.
            let mut values = Vec::new();
            let mut rest = encoded_value.split_at(1).1;
            while !rest.is_empty() && !rest.starts_with('e') {
                let (val, remainder) = decode_bencoded_value(rest);
                values.push(val);
                rest = remainder;
            }

            (values.into(), &rest[1..])
        }
        Some('d') => {
            // If encoded_value starts with 'd', it's a dictionary.
            // For example: "d3:foo3:bar5:helloi52ee" -> {"hello": 52, "foo":"bar"}

            // Value type represents any JSON type; since we will have a list of JSON vlaues, we can use a Vec of Value to store all the values of the bencoded list.
            let mut dictionary = serde_json::Map::new();
            let mut rest = encoded_value.split_at(1).1;
            while !rest.is_empty() && !rest.starts_with('e') {
                let (k, remainder) = decode_bencoded_value(rest);
                let k = match k {
                    serde_json::Value::String(k) => k,
                    k => panic!("dictionary keys must be strings, not {k:?}"),
                };

                let (v, remainder) = decode_bencoded_value(remainder);
                dictionary.insert(k, v);

                rest = remainder;
            }

            (dictionary.into(), &rest[1..])
        }

        _ => (serde_json::Value::Null, encoded_value),
    }
}

// Usage: your_bittorrent.sh decode "<encoded_value>"
fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Decode { value } => {
            let v = decode_bencoded_value(&value).0;
            println!("{v}");
        }

        Command::Info { torrent } => {
            let dot_torrent = std::fs::read(torrent).context("open torrent file")?;
            let t: Torrent =
                serde_bencode::from_bytes(&dot_torrent).context("parse torrent file")?;

            eprintln!("{t:?}");
            println!("Tracker URL: {}", t.announce);
            if let Keys::SingleFile { length } = t.info.keys {
                println!("Length: {length}");
            } else {
                todo!();
            }
        }
    }

    Ok(())
}

mod hashes {
    use serde::de::{self, Deserialize, Deserializer, Visitor};
    use std::fmt;

    #[derive(Debug, Clone)]
    pub struct Hashes(pub Vec<[u8; 20]>);
    struct HashesVisitor;

    impl<'de> Visitor<'de> for HashesVisitor {
        type Value = Hashes;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a byte string whose length is a multiple of 20")
        }

        // It can't be a visit_string because the byte array is not necessarly a valid UTF-8 string.
        fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if v.len() % 20 != 0 {
                return Err(E::custom(format!("length is {}", v.len())));
            }

            Ok(Hashes(
                v.chunks_exact(20)
                    .map(|slice_20| slice_20.try_into().expect("guaranteed to be length 20"))
                    .collect(),
            ))
        }
    }

    impl<'de> Deserialize<'de> for Hashes {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_bytes(HashesVisitor)
        }
    }
}
