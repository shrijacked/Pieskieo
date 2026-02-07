use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RecordKind {
    Put {
        family: DataFamily,
        key: Uuid,
        payload: Vec<u8>,
        #[serde(default)]
        namespace: Option<String>,
        #[serde(default)]
        collection: Option<String>,
        #[serde(default)]
        table: Option<String>,
    },
    Delete {
        family: DataFamily,
        key: Uuid,
        #[serde(default)]
        namespace: Option<String>,
        #[serde(default)]
        collection: Option<String>,
        #[serde(default)]
        table: Option<String>,
    },
    AddEdge {
        src: Uuid,
        dst: Uuid,
        weight: f32,
    },
    Schema {
        family: DataFamily,
        #[serde(default)]
        namespace: Option<String>,
        #[serde(default)]
        collection: Option<String>,
        #[serde(default)]
        table: Option<String>,
        schema: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum DataFamily {
    Row,
    Doc,
    Vec,
    Graph,
}

pub struct Wal {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl Wal {
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let path = dir.as_ref().join("wal.log");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;
        let writer = BufWriter::new(file);
        Ok(Self { path, writer })
    }

    pub fn append(&mut self, record: &RecordKind) -> Result<()> {
        let bytes = bincode::serialize(record)?;
        let len = bytes.len() as u32;
        self.writer.write_all(&len.to_le_bytes())?;
        self.writer.write_all(&bytes)?;
        Ok(())
    }

    /// Flush buffered WAL data to disk, including fsync for durability.
    pub fn flush_sync(&mut self) -> Result<()> {
        self.writer.flush()?;
        if let Some(inner) = self.writer.get_ref().try_clone().ok() {
            inner.sync_all()?;
        }
        Ok(())
    }

    pub fn replay(&self) -> Result<Vec<RecordKind>> {
        let mut res = Vec::new();
        let file = OpenOptions::new().read(true).open(&self.path)?;
        let mut reader = BufReader::new(file);
        loop {
            let mut len_buf = [0u8; 4];
            if let Err(e) = reader.read_exact(&mut len_buf) {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    break;
                }
                return Err(e.into());
            }
            let len = u32::from_le_bytes(len_buf) as usize;
            let mut data = vec![0u8; len];
            reader.read_exact(&mut data)?;
            let record: RecordKind = bincode::deserialize(&data)?;
            res.push(record);
        }
        Ok(res)
    }

    /// Return WAL length in bytes.
    pub fn len(&self) -> Result<u64> {
        Ok(std::fs::metadata(&self.path)?.len())
    }

    /// Replay records starting at a byte offset (aligned to record boundary).
    pub fn replay_since(&self, offset: u64) -> Result<(Vec<RecordKind>, u64)> {
        let mut res = Vec::new();
        let file = OpenOptions::new().read(true).open(&self.path)?;
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(offset))?;
        loop {
            let mut len_buf = [0u8; 4];
            if let Err(e) = reader.read_exact(&mut len_buf) {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    break;
                }
                return Err(e.into());
            }
            let len = u32::from_le_bytes(len_buf) as usize;
            let mut data = vec![0u8; len];
            reader.read_exact(&mut data)?;
            let record: RecordKind = bincode::deserialize(&data)?;
            res.push(record);
        }
        let end = reader.seek(SeekFrom::Current(0))?;
        Ok((res, end))
    }

    pub fn truncate(&mut self) -> Result<()> {
        let mut file = OpenOptions::new().write(true).open(&self.path)?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.sync_all()?;
        self.writer = BufWriter::new(file);
        Ok(())
    }
}
