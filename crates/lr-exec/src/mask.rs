use lr_abstractions::{SecretValue, MASK};

/// Masks secret values in a byte stream that arrives in arbitrary chunks.
///
/// [`lr_abstractions::mask_secrets`] sees one buffer only, so a value split between two reads
/// would escape it. The masker holds back the tail of a chunk only while it can still be the
/// start of a secret, so at most `longest secret - 1` bytes wait for the next chunk and output
/// is otherwise passed on at once. Bytes are never decoded: non-UTF-8 output passes unchanged.
pub struct Masker {
    secrets: Vec<Vec<u8>>,
    held: Vec<u8>,
}

impl Masker {
    /// A masker for these values; empty values are ignored, longer values win over their
    /// prefixes.
    pub fn new(secrets: &[&SecretValue]) -> Masker {
        let mut secrets: Vec<Vec<u8>> = secrets
            .iter()
            .map(|s| s.expose().as_bytes().to_vec())
            .filter(|s| !s.is_empty())
            .collect();
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Masker {
            secrets,
            held: Vec::new(),
        }
    }

    /// Takes the next chunk and returns the bytes that are safe to pass on, masked.
    pub fn push(&mut self, data: &[u8]) -> Vec<u8> {
        if self.secrets.is_empty() {
            return data.to_vec();
        }
        let mut buf = std::mem::take(&mut self.held);
        buf.extend_from_slice(data);
        self.scan(&buf, false)
    }

    /// Ends the stream: the held bytes cannot grow into a longer secret any more and are
    /// returned masked.
    pub fn finish(&mut self) -> Vec<u8> {
        let buf = std::mem::take(&mut self.held);
        self.scan(&buf, true)
    }

    fn scan(&mut self, buf: &[u8], at_end: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(buf.len());
        let mut i = 0;
        while i < buf.len() {
            let rest = &buf[i..];
            if !at_end && self.may_grow(rest) {
                self.held = rest.to_vec();
                return out;
            }
            if let Some(len) = self.matching(rest) {
                out.extend_from_slice(MASK.as_bytes());
                i += len;
            } else {
                out.push(buf[i]);
                i += 1;
            }
        }
        out
    }

    /// Number of bytes held back waiting for the next chunk.
    pub fn held(&self) -> usize {
        self.held.len()
    }

    fn matching(&self, rest: &[u8]) -> Option<usize> {
        self.secrets
            .iter()
            .find(|s| rest.starts_with(s))
            .map(|s| s.len())
    }

    fn may_grow(&self, rest: &[u8]) -> bool {
        self.secrets
            .iter()
            .any(|s| rest.len() < s.len() && s.starts_with(rest))
    }
}
