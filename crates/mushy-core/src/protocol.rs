use crate::model::*;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{Read, Write};
pub const VERSION: u32 = 1;
pub const MAX_FRAME: usize = 4 * 1024 * 1024;
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Probe {
        path: String,
        group: Option<String>,
        excludes: Vec<String>,
        single_file: Option<bool>,
    },
    Activate {
        root: Root,
    },
    Snapshot {
        known: Clock,
    },
    Import {
        changes: Vec<Change>,
    },
    Get {
        hash: String,
    },
    Put {
        hash: String,
        size: u64,
    },
    Apply,
    Close,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Hello {
        version: u32,
        device: String,
        platform: String,
        time_ns: i64,
    },
    Probe {
        root: Root,
        usage: Usage,
        existing: bool,
    },
    Changes {
        changes: Vec<Change>,
        done: bool,
    },
    Ready {
        clock: Clock,
    },
    Missing {
        objects: Vec<(String, u64)>,
    },
    Object {
        size: u64,
    },
    Ok,
    Error {
        message: String,
    },
}
pub fn write<T: Serialize>(output: &mut impl Write, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        bail!("protocol frame too large");
    }
    output.write_all(&(bytes.len() as u32).to_be_bytes())?;
    output.write_all(&bytes)?;
    output.flush()?;
    Ok(())
}
pub fn read<T: DeserializeOwned>(input: &mut impl Read) -> Result<T> {
    let mut header = [0u8; 4];
    input
        .read_exact(&mut header)
        .context("SSH helper closed the connection or printed a shell banner")?;
    let size = u32::from_be_bytes(header) as usize;
    if size > MAX_FRAME {
        bail!("invalid protocol frame length");
    }
    let mut bytes = vec![0u8; size];
    input.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
