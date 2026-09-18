//! 利用 XZ 索引跳过无关 tar 文件；每次只保留一个已校验的压缩块。

use super::range::{invalid, seek_position};
use std::io::{self, Read, Seek, SeekFrom};

const MAX_BLOCK: u64 = 64 * 1024 * 1024;

struct Block {
    compressed: u64,
    unpacked: u64,
    offset: u64,
    position: u64,
}

pub struct IndexedXz<R> {
    source: R,
    header: [u8; 12],
    blocks: Vec<Block>,
    size: u64,
    position: u64,
    cached: Option<usize>,
    buffer: Vec<u8>,
}

impl<R: Read + Seek> IndexedXz<R> {
    pub fn new(mut source: R, start: u64, length: u64) -> io::Result<Self> {
        if length < 32 {
            return Err(invalid());
        }
        let mut header = [0; 12];
        source.seek(SeekFrom::Start(start))?;
        source.read_exact(&mut header)?;
        if &header[..6] != b"\xfd7zXZ\0"
            || crc32fast::hash(&header[6..8]).to_le_bytes() != header[8..12]
        {
            return Err(invalid());
        }
        let mut footer = [0; 12];
        source.seek(SeekFrom::Start(
            start.checked_add(length - 12).ok_or_else(invalid)?,
        ))?;
        source.read_exact(&mut footer)?;
        if &footer[10..] != b"YZ"
            || footer[8..10] != header[6..8]
            || crc32fast::hash(&footer[4..10]).to_le_bytes() != footer[..4]
        {
            return Err(invalid());
        }
        let index_size = (u64::from(u32::from_le_bytes(
            footer[4..8].try_into().map_err(|_| invalid())?,
        )) + 1)
            * 4;
        if index_size > 1024 * 1024 || index_size > length - 24 {
            return Err(invalid());
        }
        let mut index = vec![0; index_size as usize];
        source.seek(SeekFrom::Start(start + length - 12 - index_size))?;
        source.read_exact(&mut index)?;
        if index[0] != 0
            || crc32fast::hash(&index[..index.len() - 4]).to_le_bytes() != index[index.len() - 4..]
        {
            return Err(invalid());
        }
        let mut cursor = 1;
        let count = vli(&index, &mut cursor)?;
        if count == 0 || count > 4096 {
            return Err(invalid());
        }
        let mut blocks = Vec::new();
        let mut offset = start + 12;
        let mut size = 0;
        for _ in 0..count {
            let compressed = vli(&index, &mut cursor)?;
            let unpacked = vli(&index, &mut cursor)?;
            if compressed == 0 || compressed > MAX_BLOCK || unpacked == 0 || unpacked > MAX_BLOCK {
                return Err(invalid());
            }
            blocks.push(Block {
                compressed,
                unpacked,
                offset,
                position: size,
            });
            offset += (compressed + 3) & !3;
            size += unpacked;
            if size > 4 * 1024 * 1024 * 1024 {
                return Err(invalid());
            }
        }
        if offset != start + length - 12 - index_size
            || cursor > index.len() - 4
            || index[cursor..index.len() - 4].iter().any(|b| *b != 0)
        {
            return Err(invalid());
        }
        Ok(Self {
            source,
            header,
            blocks,
            size,
            position: 0,
            cached: None,
            buffer: Vec::new(),
        })
    }
}

impl<R: Read + Seek> Read for IndexedXz<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.position == self.size {
            return Ok(0);
        }
        let i = self
            .blocks
            .partition_point(|b| b.position <= self.position)
            .checked_sub(1)
            .ok_or_else(invalid)?;
        let block = &self.blocks[i];
        if self.cached != Some(i) {
            let mut bytes = self.header.to_vec();
            let padded = (block.compressed + 3) & !3;
            bytes.resize(12 + padded as usize, 0);
            self.source.seek(SeekFrom::Start(block.offset))?;
            self.source.read_exact(&mut bytes[12..])?;
            // 重建单块 XZ 外壳，让 liblzma 同时校验原始块的校验和与解压长度。
            let mut index = vec![0, 1];
            put_vli(block.compressed, &mut index);
            put_vli(block.unpacked, &mut index);
            while !index.len().is_multiple_of(4) {
                index.push(0);
            }
            index.extend_from_slice(&crc32fast::hash(&index).to_le_bytes());
            let mut footer = ((index.len() as u32 / 4) - 1).to_le_bytes().to_vec();
            footer.extend_from_slice(&self.header[6..8]);
            bytes.extend_from_slice(&index);
            bytes.extend_from_slice(&crc32fast::hash(&footer).to_le_bytes());
            bytes.extend_from_slice(&footer);
            bytes.extend_from_slice(b"YZ");
            let stream = xz2::stream::Stream::new_stream_decoder(128 * 1024 * 1024, 0)
                .map_err(|_| invalid())?;
            let decoder = xz2::read::XzDecoder::new_stream(bytes.as_slice(), stream);
            self.buffer.clear();
            decoder
                .take(block.unpacked + 1)
                .read_to_end(&mut self.buffer)?;
            if self.buffer.len() as u64 != block.unpacked {
                return Err(invalid());
            }
            self.cached = Some(i);
        }
        let offset = (self.position - block.position) as usize;
        let count = out.len().min(self.buffer.len() - offset);
        out[..count].copy_from_slice(&self.buffer[offset..offset + count]);
        self.position += count as u64;
        Ok(count)
    }
}
impl<R> Seek for IndexedXz<R> {
    fn seek(&mut self, seek: SeekFrom) -> io::Result<u64> {
        self.position = seek_position(self.position, self.size, seek)?;
        Ok(self.position)
    }
}
fn vli(bytes: &[u8], cursor: &mut usize) -> io::Result<u64> {
    let mut value = 0;
    for shift in (0..63).step_by(7) {
        let byte = *bytes.get(*cursor).ok_or_else(invalid)?;
        *cursor += 1;
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            if shift > 0 && byte == 0 {
                return Err(invalid());
            }
            return Ok(value);
        }
    }
    Err(invalid())
}
fn put_vli(mut value: u64, bytes: &mut Vec<u8>) {
    while value >= 128 {
        bytes.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
}
