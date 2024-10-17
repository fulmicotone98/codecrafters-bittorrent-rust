use anyhow::Context;
use bittorrent_starter_rust::peer::*;
use bittorrent_starter_rust::{
    torrent::{self, Torrent},
    tracker::{TrackerRequest, TrackerResponse},
};
use clap::{Parser, Subcommand};
use std::{net::SocketAddrV4, path::PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    Peers { torrent: PathBuf },
    Handshake { torrent: PathBuf, peer: String },
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

fn urlencode(t: &[u8; 20]) -> String {
    let mut encoded = String::with_capacity(2 * t.len());
    for &byte in t {
        encoded.push('%');
        encoded.push_str(&hex::encode([byte]));
    }

    encoded
}

// Usage: your_bittorrent.sh decode "<encoded_value>"
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Decode { value } => {
            let v = decode_bencoded_value(&value).0;
            println!("{v}");
        }

        Command::Info { torrent } => {
            // Parse the torrent file
            let dot_torrent = std::fs::read(torrent).context("open torrent file")?;
            let t: Torrent =
                serde_bencode::from_bytes(&dot_torrent).context("parse torrent file")?;

            eprintln!("{t:?}");
            println!("Tracker URL: {}", t.announce);
            let length = if let torrent::Keys::SingleFile { length } = t.info.keys {
                length
            } else {
                todo!();
            };

            println!("Length: {length}");

            let info_hash = t.info_hash();
            println!("Info Hash: {}", hex::encode(info_hash));

            // Piece length and piece hashes
            println!("Piece Length: {}", t.info.plength);
            println!("Piece Hashes:");
            for hash in t.info.pieces.0 {
                println!("{}", hex::encode(hash));
            }
        }

        Command::Peers { torrent } => {
            let dot_torrent = std::fs::read(torrent).context("read torrent file")?;
            let t: Torrent =
                serde_bencode::from_bytes(&dot_torrent).context("parse torrent file")?;

            let length = if let torrent::Keys::SingleFile { length } = t.info.keys {
                length
            } else {
                todo!();
            };

            let info_hash = t.info_hash();
            let request = TrackerRequest {
                peer_id: String::from("00112233445566778899"),
                port: 6881,
                uploaded: 0,
                downloaded: 0,
                left: length,
                compact: 1,
            };

            let url_params =
                serde_urlencoded::to_string(&request).context("url-encode tracker parameters")?;

            let tracker_url = format!(
                "{}?{}&info_hash={}",
                t.announce,
                url_params,
                &urlencode(&info_hash)
            );

            // let tracker_url =
            //     reqwest::Url::parse(&t.announce).context("parse tracker announce URL")?;

            let response = reqwest::get(tracker_url).await.context("query tracker")?;
            let response = response.bytes().await.context("fetch tracker response")?;
            let response: TrackerResponse =
                serde_bencode::from_bytes(&response).context("parse tracker response")?;

            for peer in &response.peers.0 {
                println!("{}:{}", peer.ip(), peer.port());
            }
        }

        Command::Handshake { torrent, peer } => {
            let dot_torrent = std::fs::read(torrent).context("read torrent file")?;
            let t: Torrent =
                serde_bencode::from_bytes(&dot_torrent).context("parse torrent file")?;
            let info_hash = t.info_hash();

            let peer: SocketAddrV4 = peer.parse().context("parse peer address")?;
            let mut peer = tokio::net::TcpStream::connect(peer)
                .await
                .context("connect to peer")?;

            let mut handshake = Handshake::new(info_hash, *b"00112233445566778899");

            // handshake_bytes shoud be only valid inside the scope and dropped and the end.
            {
                let handshake_bytes =
                    &mut handshake as *mut Handshake as *mut [u8; std::mem::size_of::<Handshake>()];
                let handshake_bytes: &mut [u8; std::mem::size_of::<Handshake>()] =
                    unsafe { &mut *handshake_bytes };

                peer.write_all(handshake_bytes)
                    .await
                    .context("write handshake")?;

                peer.read_exact(handshake_bytes)
                    .await
                    .context("read handshake")?;
            }
            assert_eq!(handshake.length, 19);
            assert_eq!(handshake.bittorrent, *b"BitTorrent protocol");
            assert_eq!(handshake.info_hash, info_hash);

            println!("Peer ID: {}", hex::encode(handshake.peer_id));
        }
    }

    Ok(())
}
