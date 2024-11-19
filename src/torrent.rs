use super::download;
use crate::download::Downloaded;
use anyhow::{Context, Ok};
pub use hashes::Hashes;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::path::Path;

/// Metainfo files (also known as .torrent files) are bencoded dictionaries with the following keys:
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Torrent {
    /// The URL of the tracker.
    pub announce: String,
    /// This maps to a dictionary, with keys described below.
    pub info: Info,
}

impl Torrent {
    pub fn info_hash(&self) -> [u8; 20] {
        // Calculate the info hash of the Torrent file
        let info_encoded = serde_bencode::to_bytes(&self.info).expect("re-encoded info section");
        let mut hasher = Sha1::new();
        hasher.update(info_encoded);
        hasher.finalize().into()
    }

    pub async fn read(file: impl AsRef<Path>) -> anyhow::Result<Self> {
        let dot_torrent = tokio::fs::read(file).await.context("read torrent file")?;
        let t: Torrent = serde_bencode::from_bytes(&dot_torrent).context("parse torrent file")?;
        Ok(t)
    }

    pub fn print_tree(&self) {
        match &self.info.keys {
            Keys::SingleFile { .. } => {
                eprintln!("{}", self.info.name);
            }
            Keys::MultiFile { files } => {
                for file in files {
                    eprintln!("{:?}", file.path.join(std::path::MAIN_SEPARATOR_STR));
                }
            }
        }
    }

    pub fn length(&self) -> usize {
        match &self.info.keys {
            Keys::SingleFile { length } => *length,
            Keys::MultiFile { files } => files.iter().map(|file| file.length).sum(),
        }
    }

    pub async fn download_all(&self) -> anyhow::Result<Downloaded> {
        download::all(self).await
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Info {
    /// The suggested name to save the file (or directory) as. It is purely advisory.
    ///
    /// In single file case, the name key is the name of a file, in the multiple file case, it is the same name of a directory.
    pub name: String,

    /// The number of bytes in each piece the file is split into.
    ///
    /// For the purposes of transfer, files are split into fixed-size pieces which are all the same length except for possibly the last one which may be truncated. piece length is almost always a power of two, most commonly 2^18 = 256 K (BitTorrent prior to version 3.2 uses 2 20 = 1 M as default).
    #[serde(rename = "piece length")]
    pub plength: usize,

    /// Each entry of 'pieces' is the SHA1 hash of the piece at the corresponding index.
    pub pieces: Hashes,

    #[serde(flatten)]
    pub keys: Keys,
}

/// There is also a key 'length' or a key 'files', but not both or neither.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Keys {
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct File {
    /// The length of the file, in bytes.
    pub length: usize,

    /// Subdirectory names the file, the last is the actual file name (a zero length list is an error case).
    pub path: Vec<String>,
}

mod hashes {
    use serde::de::{self, Deserialize, Deserializer, Visitor};
    use serde::ser::{Serialize, Serializer};
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

    impl Serialize for Hashes {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let single_slice = self.0.concat();
            serializer.serialize_bytes(&single_slice)
        }
    }
}
