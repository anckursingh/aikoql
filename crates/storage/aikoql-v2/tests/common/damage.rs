//! PR6-F3 (P0-3) — the deterministic damage corpus shared by the recovery
//! tests: one mutation vocabulary, one way to apply it, so every codec path
//! exercises the same damage classes through the FormatError classifiers.
//! See tests/damage_corpus.rs for the matrix and the pinned semantics.

#[derive(Clone, Copy, Debug)]
pub enum Damage {
    BitFlip { offset: usize },
    Truncate(usize),
    TrailingByte(u8),
    ZeroRegion { from: usize, len: usize },
}

impl Damage {
    pub fn apply(&self, bytes: &[u8]) -> Vec<u8> {
        match *self {
            Damage::BitFlip { offset } => {
                let mut b = bytes.to_vec();
                b[offset] ^= 0x80;
                b
            }
            Damage::Truncate(cut) => bytes[..cut.min(bytes.len())].to_vec(),
            Damage::TrailingByte(extra) => {
                let mut b = bytes.to_vec();
                b.push(extra);
                b
            }
            Damage::ZeroRegion { from, len } => {
                let mut b = bytes.to_vec();
                b[from..from + len].fill(0);
                b
            }
        }
    }
}
