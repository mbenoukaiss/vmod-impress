use crate::backend::FileTransfer;
use std::io::{Cursor, Read};
use varnish::vcl::{VclError, VclResponse};

pub struct MemoryTransfer {
    cursor: Cursor<Vec<u8>>,
    total: usize,
}

impl MemoryTransfer {
    pub fn new(bytes: Vec<u8>) -> Self {
        let total = bytes.len();
        MemoryTransfer {
            cursor: Cursor::new(bytes),
            total,
        }
    }

    //size() is only consumed in tests; production code reaches the byte count
    //via the pre-built `content_length_str` on FetchResult or via VclResponse::len.
    #[cfg(test)]
    pub fn size(&self) -> usize {
        self.total
    }
}

impl VclResponse for MemoryTransfer {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, VclError> {
        self.cursor
            .read(buf)
            .map_err(|e| VclError::new(e.to_string()))
    }

    fn len(&self) -> Option<usize> {
        Some(self.total)
    }
}

pub enum Transfer {
    File(FileTransfer),
    Memory(MemoryTransfer),
}

impl Transfer {
    //test-only helper, see MemoryTransfer::size
    #[cfg(test)]
    pub fn size(&self) -> usize {
        match self {
            Transfer::File(f) => f.size(),
            Transfer::Memory(m) => m.size(),
        }
    }
}

impl VclResponse for Transfer {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, VclError> {
        match self {
            Transfer::File(f) => f.read(buf),
            Transfer::Memory(m) => m.read(buf),
        }
    }

    fn len(&self) -> Option<usize> {
        match self {
            Transfer::File(f) => f.len(),
            Transfer::Memory(m) => m.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_transfer_streams_in_chunks() {
        let payload = b"hello world".to_vec();
        let mut t = MemoryTransfer::new(payload.clone());
        assert_eq!(t.size(), payload.len());
        assert_eq!(t.len(), Some(payload.len()));

        let mut buf = [0u8; 4];
        assert_eq!(t.read(&mut buf).unwrap(), 4);
        assert_eq!(&buf, b"hell");
        assert_eq!(t.read(&mut buf).unwrap(), 4);
        assert_eq!(&buf, b"o wo");
        assert_eq!(t.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf[..3], b"rld");
        assert_eq!(t.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn empty_payload_reads_zero() {
        let mut t = MemoryTransfer::new(Vec::new());
        assert_eq!(t.size(), 0);
        let mut buf = [0u8; 8];
        assert_eq!(t.read(&mut buf).unwrap(), 0);
    }
}
