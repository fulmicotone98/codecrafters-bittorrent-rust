use crate::BLOCK_MAX;
use crate::{
    peer::Peer,
    piece::Piece,
    torrent::{File, Torrent},
    tracker::TrackerResponse,
};
use anyhow::Context;
use futures_util::stream::StreamExt;
use sha1::{Digest, Sha1};
use std::collections::BinaryHeap;

pub(crate) async fn all(t: &Torrent) -> anyhow::Result<Downloaded> {
    let info_hash = t.info_hash();
    let peer_info = TrackerResponse::query(t, info_hash)
        .await
        .context("query tracker for peer info")?;

    let mut peer_list = Vec::new();
    let mut peers = futures_util::stream::iter(peer_info.peers.0.iter())
        .map(|&peer_addr| async move {
            let peer = Peer::new(peer_addr, info_hash).await;
            (peer_addr, peer)
        })
        .buffer_unordered(5 /* user config */);

    while let Some((peer_addr, peer)) = peers.next().await {
        match peer {
            Ok(peer) => {
                peer_list.push(peer);
                if peer_list.len() >= 5 {
                    /* TODO: user config */
                    break;
                }
            }
            Err(e) => {
                eprintln!("failed to connect to peer {peer_addr:?}: {e:?}");
            }
        }
    }
    // Drop all the connections with peers
    drop(peers);
    let mut peers = peer_list;

    let mut need_pieces = BinaryHeap::new();
    let mut no_peers = Vec::new();
    for piece_i in 0..t.info.pieces.0.len() {
        let piece = Piece::new(piece_i, t, &peers);
        if piece.peers().is_empty() {
            no_peers.push(piece);
        } else {
            need_pieces.push(piece);
        }
    }

    // TODO
    assert!(no_peers.is_empty());

    // TODO: this is dumb because all the pieces for a given torrent may not fit in memory! Should probably .write every piece to disk so that we can also reasume downloads, and seed later on
    let mut all_pieces = vec![0; t.length()];
    while let Some(next_piece) = need_pieces.pop() {
        let piece_size = next_piece.length();

        // Each piece has a bunch of blocks
        // The + (BLOCK_MAX - 1) rounds up
        let nblocks = (piece_size + (BLOCK_MAX - 1)) / BLOCK_MAX;

        // We are gonna have a mutable ref to each of the peers that have this particular piece available
        let peers: Vec<_> = peers
            .iter_mut()
            .enumerate()
            .filter_map(|(peer_i, peer)| next_piece.peers().contains(&peer_i).then_some(peer))
            .collect();

        let (submit, tasks) = kanal::bounded_async(nblocks);
        // We are gonna send all of the block in as jobs
        for block in 0..nblocks {
            submit
                .send(block)
                .await
                .expect("bound holds all these items");
        }
        let (finish, mut done) = tokio::sync::mpsc::channel(nblocks);
        let mut participants = futures_util::stream::futures_unordered::FuturesUnordered::new();

        // We are gonna make all these peers partecipate by running this loop
        for peer in peers {
            participants.push(peer.partecipate(
                next_piece.index(),
                piece_size,
                nblocks,
                submit.clone(),
                tasks.clone(),
                finish.clone(),
            ));
        }
        drop(submit);
        drop(finish);
        drop(tasks);

        // An then we are gonna observe both the done list and the join set
        let mut all_blocks = vec![0u8, piece_size as u8];
        let mut bytes_received = 0;
        loop {
            tokio::select! {
                joined = participants.next(), if !participants.is_empty() => {
                    // If a partecipant ends early, it's either slow or failed
                    match joined {
                        None => {
                            // There are no peers!
                            // This must mean we are about to get None from done.recv(), so we'll handle it there
                        }
                        Some(Ok(_)) => {
                            // The peer gave up because it timed out
                            // Nothing to do, except maybe de-prioritize this peer for later
                            // TODO
                        }
                        Some(Err(_)) => {
                            // The peer failed and should be removed
                            // It already isn't partecipating in this piece anymore, so this is more of an indicator that we shouldn't try this peer again, and should remove it from the global peer list
                            // TODO
                        }
                    }
                }
                piece = done.recv() => {
                    if let Some(piece) = piece {
                        // Keep track of the bytes in message
                        let piece = crate::peer::Piece::ref_from_bytes(&piece.payload[..])
                            .expect("always get all Piece response fields from peers");
                        bytes_received += piece.block().len();
                        all_blocks[piece.begin() as usize..][..piece.block().len()].copy_from_slice(piece.block());
                        if bytes_received == piece_size {
                            // have received every piece
                            // this must mean that all partecipations have either or are waiting for more work -- in either case, it is okay to drop all the partecipant futures.
                            break;
                        }
                    } else {
                        // there are no peers left
                        break;
                    }
                }
            }
        }
        drop(participants);

        if bytes_received == piece_size {
            // great we got all the bytes
        } else {
            // We'll need to connect to more peers, and make sure that those additional peers also have this piece, and then download the pieces we _didn't_ get from them.
            // Probably also stick this back onto the pieces_heap.
            anyhow::bail!("no peers left to get piece {}", next_piece.index())
        }

        let mut hasher = Sha1::new();
        hasher.update(&all_blocks);
        let hash: [u8; 20] = hasher.finalize().into();
        assert_eq!(hash, next_piece.hash());

        all_pieces[next_piece.index() * t.info.plength..][..piece_size]
            .copy_from_slice(&all_blocks);
    }

    Ok(Downloaded {
        bytes: all_pieces,
        files: match &t.info.keys {
            crate::torrent::Keys::SingleFile { length } => vec![File {
                length: *length,
                path: vec![t.info.name.clone()],
            }],
            crate::torrent::Keys::MultiFile { files } => files.clone(),
        },
    })
}

pub struct Downloaded {
    bytes: Vec<u8>, //TODO: maybe Bytes?
    files: Vec<File>,
}

impl<'a> IntoIterator for &'a Downloaded {
    type Item = DownloadedFile<'a>;
    type IntoIter = DownloadedIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        DownloadedIter::new(self)
    }
}

pub struct DownloadedIter<'d> {
    downloaded: &'d Downloaded,
    file_iter: std::slice::Iter<'d, File>,
    offset: usize,
}

impl<'d> DownloadedIter<'d> {
    fn new(d: &'d Downloaded) -> Self {
        Self {
            downloaded: d,
            file_iter: d.files.iter(),
            offset: 0,
        }
    }
}

impl<'d> Iterator for DownloadedIter<'d> {
    type Item = DownloadedFile<'d>;

    fn next(&mut self) -> Option<Self::Item> {
        let file = self.file_iter.next()?;
        let bytes = &self.downloaded.bytes[self.offset..][..file.length]; // has the same effect of[self.offset..self.offset + file.length]

        Some(DownloadedFile { file, bytes })
    }
}

pub struct DownloadedFile<'d> {
    file: &'d File,
    bytes: &'d [u8],
}

impl<'d> DownloadedFile<'d> {
    pub fn path(&self) -> &'d [String] {
        &self.file.path
    }

    pub fn bytes(&self) -> &'d [u8] {
        self.bytes
    }
}
