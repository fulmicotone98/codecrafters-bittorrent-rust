use anyhow::Context;
use bytes::{Buf, BufMut, BytesMut};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddrV4;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_util::codec::{Decoder, Encoder, Framed};

const BLOCK_MAX: usize = 1 << 14;

// TODO: ideally, Peer should keep track of what pieces we have downloaded (and references to them) so that we can respond to Requests from the other side. Also chokeing/unchoking the other side.
pub(crate) struct Peer {
    // addr: SocketAddrV4,
    stream: Framed<TcpStream, MessageFramer>,
    bitfield: Bitfield,
    choked: bool,
}

impl Peer {
    pub async fn new(peer_addr: SocketAddrV4, info_hash: [u8; 20]) -> anyhow::Result<Self> {
        let mut peer = tokio::net::TcpStream::connect(peer_addr)
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
        anyhow::ensure!(handshake.length == 19);
        anyhow::ensure!(&handshake.bittorrent == b"BitTorrent protocol");

        println!("Peer ID: {}", hex::encode(handshake.peer_id));

        let mut peer = tokio_util::codec::Framed::new(peer, MessageFramer);
        let bitfield = peer
            .next()
            .await
            .expect("peer always sends a bitfields")
            .context("peer message was invalid")?;
        anyhow::ensure!(bitfield.tag == MessageTag::Bitfield);
        // NOTE: we assume that bitfield covers all pieces

        Ok(Self {
            // addr: peer_addr,
            stream: peer,
            bitfield: Bitfield::from_payload(bitfield.payload),
            choked: true,
        })
    }

    pub async fn download(
        &mut self,
        piece_i: usize,
        block_i: usize,
        block_size: usize,
    ) -> anyhow::Result<Vec<u8>> {
        // Try to download a piece from a Peer which doesn't have that piece would return an error.
        anyhow::ensure!(self.bitfield.has_piece(piece_i));
        let mut request = Request::new(
            piece_i as u32,
            (block_i * BLOCK_MAX) as u32,
            block_size as u32,
        );
        let request_bytes = Vec::from(request.as_bytes_mut());
        self.stream
            .send(Message {
                tag: MessageTag::Request,
                payload: request_bytes,
            })
            .await
            .with_context(|| format!("send request for block {block_i}"))?;

        let piece = self
            .stream
            .next()
            .await
            .expect("peer always sends a piece")
            .context("peer message was invalid")?;
        anyhow::ensure!(piece.tag == MessageTag::Piece);
        anyhow::ensure!(piece.payload.is_empty());

        // Casting the payload to be a raw pointer to Piece struct, the turning that raw pointer into an actual reference tot hat type so that it is possible to access its  fields.
        let piece = Piece::ref_from_bytes(&piece.payload[..])
            .expect("always get all Piece response fields from peer");
        anyhow::ensure!(piece.index() as usize == piece_i);
        anyhow::ensure!(piece.begin() as usize == block_i * BLOCK_MAX);
        anyhow::ensure!(piece.block().len() == block_size);

        Ok(Vec::from(piece.block()))
    }

    pub(crate) fn has_piece(&self, piece_i: usize) -> bool {
        self.bitfield.has_piece(piece_i)
    }

    pub(crate) async fn partecipate(
        &mut self,
        piece_i: usize,
        piece_size: usize,
        nblocks: usize,
        submit: kanal::AsyncSender<usize>,
        tasks: kanal::AsyncReceiver<usize>,
        finish: tokio::sync::mpsc::Sender<Message>,
    ) -> anyhow::Result<()> {
        self.stream
            .send(Message {
                tag: MessageTag::Intrested,
                payload: Vec::new(),
            })
            .await
            .context("send interested message")?;

        // TODO: timeout, error, and return block to submit if .next() timed out
        'task: loop {
            while self.choked {
                let msg = self
                    .stream
                    .next()
                    .await
                    .expect("peer always sends an unchoke")
                    .context("peer message was invalid")?;
                match msg.tag {
                    MessageTag::Unchoke => {
                        self.choked = false;
                        assert!(msg.payload.is_empty());
                        break;
                    }
                    MessageTag::Have => {
                        // TODO: update bitfield
                        // TODO: add to list of peers for relevant piece
                    }
                    MessageTag::Intrested
                    | MessageTag::NotInterested
                    | MessageTag::Request
                    | MessageTag::Cancel => {
                        // not allowing requests for now
                    }
                    MessageTag::Piece => {
                        // piece we no longer need/are responsible for
                    }
                    MessageTag::Choke => {
                        anyhow::bail!("peer sent unchoke while unchoked");
                    }
                    MessageTag::Bitfield => {
                        anyhow::bail!("peer sent bitfiled after handshake has been completed ");
                    }
                }
            }

            // The moment we broke out the while loop, it means that we are no longer choked and we can take tasks off the queue.
            // If there are no tasks in the queue we can break, it menas that there's no more work.
            let Ok(block) = tasks.recv().await else {
                break;
            };

            // It is going to continuonsly receive tasks that it's going to download from this channel
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

            // Then it just constructs a request
            let mut request = Request::new(
                piece_i as u32,
                (block * BLOCK_MAX) as u32,
                block_size as u32,
            );
            let request_bytes = Vec::from(request.as_bytes_mut());

            // Sends that request on its own thing
            self.stream
                .send(Message {
                    tag: MessageTag::Request,
                    payload: request_bytes,
                })
                .await
                .with_context(|| format!("send request for block {block}"))?;

            // TODO: timieout and return block to submit if timed out
            // Waits for the response
            let mut msg;
            loop {
                msg = self
                    .stream
                    .next()
                    .await
                    .expect("peer always sends a piece")
                    .context("peer message was invalid")?;

                match msg.tag {
                    MessageTag::Choke => {
                        assert!(msg.payload.is_empty());
                        self.choked = true;
                        // We should make someone else take this block instead because we are choked
                        submit.send(block).await.expect("we still have a receiver");
                        continue 'task;
                    }
                    MessageTag::Piece => {
                        // Casting the payload to be a raw pointer to Piece struct, the turning that raw pointer into an actual reference tot hat type so that it is possible to access its  fields.
                        let piece = Piece::ref_from_bytes(&msg.payload[..])
                            .expect("always get all Piece response fields from peer");
                        if piece.index() as usize != piece_i
                            || piece.begin() as usize != block * BLOCK_MAX
                        {
                            // piece that we no longer need/ are looking for
                        } else {
                            // it is the piece we are looking for and we can break
                            assert_eq!(piece.block().len(), block_size);
                            break;
                        }
                    }
                    MessageTag::Have => {
                        // TODO: update bitfield
                        // TODO: add to list of peers for relevant piece
                    }
                    MessageTag::Intrested
                    | MessageTag::NotInterested
                    | MessageTag::Request
                    | MessageTag::Cancel => {
                        // not allowing requests for now
                    }
                    MessageTag::Unchoke => {
                        anyhow::bail!("peer sent unchoke while unchoked");
                    }
                    MessageTag::Bitfield => {
                        anyhow::bail!("peer sent bitfiled after handshake has been completed ");
                    }
                }
            }

            finish.send(msg).await.expect("receiver should not go away while there are active peers (us) and missing blocks (this)");
        }

        Ok(())
    }
}

pub struct Bitfield {
    payload: Vec<u8>,
}

impl Bitfield {
    pub(crate) fn has_piece(&self, piece_i: usize) -> bool {
        let byte_i = piece_i / (u8::BITS as usize);
        let bit_i = (byte_i % (u8::BITS as usize)) as u32;
        let Some(byte) = self.payload.get(byte_i) else {
            return false;
        };

        // byte = 01010101
        // Is the nth bit set?
        // Answer:
        //
        // Take 1 << nth_bit, for nth_bit = 3 ->
        // 00001000
        // Then & with the byte:
        // 01010101
        // &
        // 00001000
        // 00000000

        //rotate_right: shifts bytes to left (when one is at the right end, it shifts to the left end
        //00000001 -> rotate_right(1) -> 10000000

        // Since the first byte of the Bitfield corresponds to indices 0-7 from high bit to low bit, it is possible to select the proper index inside the Bitfield:
        // bit = 0 -> 1.rotate_right(0+1) = 10000000 -> put in & with the Bitfield selects the first bit (nth = 0) of the 1 byte (byte) of the Bitfiled
        (byte & 1_u8.rotate_right(bit_i + 1)) != 0
    }

    pub(crate) fn pieces(&self) -> impl Iterator<Item = usize> + '_ {
        self.payload.iter().enumerate().flat_map(|(byte_i, byte)| {
            (0..u8::BITS).filter_map(move |bit_i| {
                let piece_i = byte_i * (u8::BITS as usize) + (bit_i as usize);
                let mask = 1_u8.rotate_right(bit_i + 1); // == 128 == 10000000
                (byte & mask != 0).then_some(piece_i)
            })
        })
    }

    fn from_payload(payload: Vec<u8>) -> Bitfield {
        Self { payload }
    }
}

#[test]
fn bitfield_has() {
    let bf = Bitfield {
        payload: vec![0b10101010, 0b01010101],
    };
    assert!(bf.has_piece(0));
    assert!(bf.has_piece(1));
    assert!(bf.has_piece(7));
    assert!(bf.has_piece(8));
    assert!(bf.has_piece(15));
}

#[test]
fn bitfield_iter() {
    let bf = Bitfield {
        payload: vec![0b10101010, 0b01010101],
    };
    let mut pieces = bf.pieces();
    assert_eq!(pieces.next(), Some(0));
    assert_eq!(pieces.next(), Some(2));
    assert_eq!(pieces.next(), Some(4));
    assert_eq!(pieces.next(), Some(6));
    assert_eq!(pieces.next(), Some(9));
    assert_eq!(pieces.next(), Some(11));
    assert_eq!(pieces.next(), Some(13));
    assert_eq!(pieces.next(), Some(15));
}

#[repr(C)]
pub struct Handshake {
    pub length: u8,
    pub bittorrent: [u8; 19],
    pub reserved: [u8; 8],
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl Handshake {
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
        Self {
            length: 19,
            bittorrent: *b"BitTorrent protocol",
            reserved: [0; 8],
            info_hash,
            peer_id,
        }
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        let handshake_bytes = self as *mut Handshake as *mut [u8; std::mem::size_of::<Handshake>()];
        // Safety: Handshake is a POD with repr(C)
        let handshake_bytes: &mut [u8; std::mem::size_of::<Handshake>()] =
            unsafe { &mut *handshake_bytes };
        handshake_bytes
    }
}

#[repr(C)]
pub struct Request {
    index: [u8; 4],
    begin: [u8; 4],
    length: [u8; 4],
}

impl Request {
    pub fn new(index: u32, begin: u32, length: u32) -> Self {
        Self {
            index: index.to_be_bytes(),
            begin: begin.to_be_bytes(),
            length: length.to_be_bytes(),
        }
    }

    pub fn index(&self) -> u32 {
        u32::from_be_bytes(self.index)
    }

    pub fn begin(&self) -> u32 {
        u32::from_be_bytes(self.begin)
    }

    pub fn length(&self) -> u32 {
        u32::from_be_bytes(self.length)
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        let request_bytes = self as *mut Request as *mut [u8; std::mem::size_of::<Request>()];
        let request_bytes: &mut [u8; std::mem::size_of::<Request>()] =
            unsafe { &mut *request_bytes };
        request_bytes
    }
}

#[repr(C)]
pub struct Piece<T: ?Sized = [u8]> {
    index: [u8; 4],
    begin: [u8; 4],
    block: T,
}

impl Piece {
    pub fn index(&self) -> u32 {
        u32::from_be_bytes(self.index)
    }

    pub fn begin(&self) -> u32 {
        u32::from_be_bytes(self.begin)
    }

    pub fn block(&self) -> &[u8] {
        &self.block
    }

    const PIECE_LEAD: usize = std::mem::size_of::<Piece<()>>();
    pub fn ref_from_bytes(data: &[u8]) -> Option<&Self> {
        if data.len() < Self::PIECE_LEAD {
            return None;
        }

        let n = data.len();

        // NOTE: The slicing here looks really weird. The reason we do it is because we need the length part of the fat pointer to Piece to old the length of _just_ the `block` field. And the only way we can change the length of the fat pointer to Piece is by changing the length of the fat pointer to the slice, which we do by slicing it. We can't slice it at the front (as it would invalidate the ptr part of the fat pointer), so we slice it at the back!

        let piece = &data[..n - Self::PIECE_LEAD] as *const [u8] as *const Piece;

        // Safety: Piece is a POD with repr(c), _and_ the fat pointer data length is the length of the trailing DST field (thanks to the PIECE_LEAD offset).

        Some(unsafe { &*piece })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MessageTag {
    Choke = 0,
    Unchoke = 1,
    Intrested = 2,
    NotInterested = 3,
    Have = 4,
    Bitfield = 5,
    Request = 6,
    Piece = 7,
    Cancel = 8,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub tag: MessageTag,
    pub payload: Vec<u8>,
}

// Decoder from tokio_util::codec
pub struct MessageFramer;

const MAX: usize = 1 << 16;

impl Decoder for MessageFramer {
    // The thing the MessageDecoder is gonna produce is a Message
    type Item = Message;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            // Not enough data to read length marker.
            return Result::Ok(None);
        }

        // Read length marker.
        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let length = u32::from_be_bytes(length_bytes) as usize;
        /* eprintln!("{length}"); */

        if length == 0 {
            // This is a heartbeat message.
            // Discard it.
            src.advance(4);
            // Then try again in case the buffer has more messages.
            return self.decode(src);
        }

        if src.len() < 5 {
            // Not enough data to read tag marker
            return Result::Ok(None);
        }

        // Check that the length is not too large to avoid a denial of
        // service attack where the server runs out of memory.
        if length > MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", length),
            ));
        }

        if src.len() < 4 + length {
            // The full string has not yet arrived.
            //
            // We reserve more space in the buffer. This is not strictly
            // necessary, but is a good idea performance-wise.
            src.reserve(4 + length - src.len());

            // We inform the Framed that we need more bytes to form the next
            // frame.
            return Result::Ok(None);
        }

        // Use advance to modify src such that it no longer contains
        // this frame.
        let tag = match src[4] {
            0 => MessageTag::Choke,
            1 => MessageTag::Unchoke,
            2 => MessageTag::Intrested,
            3 => MessageTag::NotInterested,
            4 => MessageTag::Have,
            5 => MessageTag::Bitfield,
            6 => MessageTag::Request,
            7 => MessageTag::Piece,
            8 => MessageTag::Cancel,
            tag => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Unknown message tag {}.", tag),
                ))
            }
        };
        let data = if src.len() > 5 {
            src[5..4 + length].to_vec()
        } else {
            Vec::new()
        };
        src.advance(4 + length);

        Result::Ok(Some(Message { tag, payload: data }))
    }
}

impl Encoder<Message> for MessageFramer {
    type Error = std::io::Error;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // Don't send a message if it is longer than the other end will
        // accept.
        // + 1 cause of the TAG
        if item.payload.len() + 1 > MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", item.payload.len()),
            ));
        }

        // Convert the length into a byte array.
        let len_slice = u32::to_be_bytes(item.payload.len() as u32 + 1);

        // Reserve space in the buffer.
        dst.reserve(4 /* length */ + 1 /* tag */ + item.payload.len());

        // Write the length and string to the buffer.
        dst.extend_from_slice(&len_slice);
        dst.put_u8(item.tag as u8);
        dst.extend_from_slice(&item.payload);
        Result::Ok(())
    }
}
