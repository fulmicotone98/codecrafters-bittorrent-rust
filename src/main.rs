use anyhow::Context;
use bittorrent_starter_rust::{
    peer::*,
    torrent::{self, Torrent},
    tracker::*,
};
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use sha1::{Digest, Sha1};
use std::{net::SocketAddrV4, path::PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BLOCK_MAX: usize = 1 << 14;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
#[clap(rename_all = "snake_case")]
enum Command {
    Decode {
        value: String,
    },
    Info {
        torrent: PathBuf,
    },
    Peers {
        torrent: PathBuf,
    },
    Handshake {
        torrent: PathBuf,
        peer: String,
    },
    DownloadPiece {
        #[arg(short)]
        output: PathBuf,
        torrent: PathBuf,
        piece: usize,
    },
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

        Command::DownloadPiece {
            output,
            torrent,
            piece: piece_i,
        } => {
            let dot_torrent = std::fs::read(torrent).context("read torrent file")?;
            let t: Torrent =
                serde_bencode::from_bytes(&dot_torrent).context("parse torrent file")?;

            let length = if let torrent::Keys::SingleFile { length } = t.info.keys {
                length
            } else {
                todo!();
            };
            assert!(piece_i < t.info.pieces.0.len());

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

            let response = reqwest::get(tracker_url).await.context("query tracker")?;
            let response = response.bytes().await.context("fetch tracker response")?;
            let tracker_info: TrackerResponse =
                serde_bencode::from_bytes(&response).context("parse tracker response")?;

            // Get the 1st peer
            let peer: SocketAddrV4 = tracker_info.peers.0[0];
            let mut peer = tokio::net::TcpStream::connect(peer)
                .await
                .context("connect to peer")?;

            let mut handshake = Handshake::new(info_hash, *b"00112233445566778899");

            // handshake_bytes shoud be only valid inside the scope and dropped and the end.
            {
                let handshake_bytes = handshake.as_bytes_mut();

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

            let mut peer = tokio_util::codec::Framed::new(peer, MessageFramer);
            let bitfield = peer
                .next()
                .await
                .expect("peer always sends a bitfields")
                .context("peer message was invalid")?;
            assert_eq!(bitfield.tag, MessageTag::Bitfield);
            eprintln!("{:?}", bitfield.tag);
            // NOTE: we assume that bitfield covers all pieces

            peer.send(Message {
                tag: MessageTag::Intrested,
                payload: Vec::new(),
            })
            .await
            .context("send interested message")?;

            let unchoke = peer
                .next()
                .await
                .expect("peer always sends an unchoke")
                .context("peer message was invalid")?;
            assert_eq!(unchoke.tag, MessageTag::Unchoke);
            assert!(unchoke.payload.is_empty());

            let piece_hash = &t.info.pieces.0[piece_i];
            let piece_size = if piece_i == t.info.pieces.0.len() + 1 {
                length % t.info.plength
            } else {
                t.info.plength
            };
            // The + (BLOCK_MAX - 1) rounds up
            let nblocks = (piece_size + (BLOCK_MAX - 1)) / BLOCK_MAX;
            let mut all_blocks = Vec::with_capacity(piece_size);
            for block in 0..nblocks {
                let block_size = if block == nblocks - 1 {
                    let md = piece_size % BLOCK_MAX;
                    if md == 0 {
                        BLOCK_MAX
                    } else {
                        md
                    }
                } else {
                    BLOCK_MAX
                };
                let mut request = Request::new(
                    piece_i as u32,
                    (block * BLOCK_MAX) as u32,
                    block_size as u32,
                );
                let request_bytes = Vec::from(request.as_bytes_mut());
                peer.send(Message {
                    tag: MessageTag::Request,
                    payload: request_bytes,
                })
                .await
                .with_context(|| format!("send request for block {block}"))?;

                let piece = peer
                    .next()
                    .await
                    .expect("peer always sends a piece")
                    .context("peer message was invalid")?;
                assert_eq!(unchoke.tag, MessageTag::Unchoke);
                assert!(unchoke.payload.is_empty());

                // Casting the payload to be a raw pointer to Piece struct, the turning that raw pointer into an actual reference tot hat type so that it is possible to access its  fields.
                let piece = Piece::ref_from_bytes(&piece.payload[..])
                    .expect("always get all Piece response fields from peer");
                assert_eq!(piece.index() as usize, piece_i);
                assert_eq!(piece.begin() as usize, block * BLOCK_MAX);
                assert_eq!(piece.block().len(), block_size);

                all_blocks.extend(piece.block());
            }

            let mut hasher = Sha1::new();
            hasher.update(&all_blocks);
            let hash: [u8; 20] = hasher.finalize().into();

            assert_eq!(&hash, piece_hash);

            tokio::fs::write(&output, all_blocks)
                .await
                .context("write out downloaded piece")?;

            println!("Piece {piece_i} downloaded to {}.", output.display());
        }
    }
    Ok(())
}
