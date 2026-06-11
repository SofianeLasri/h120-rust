//! Lecture/écriture binaire MSB-first (§1.6.1 : « the most significant digit
//! is in the leading position »).

/// Écrivain de bits, MSB en premier.
pub struct BitWriter {
    buf: Vec<u8>,
    cur: u8,
    nbits: u8,
    total_bits: u64,
}

impl BitWriter {
    pub fn new() -> Self {
        BitWriter { buf: Vec::new(), cur: 0, nbits: 0, total_bits: 0 }
    }

    #[inline]
    pub fn put_bit(&mut self, bit: bool) {
        self.cur = (self.cur << 1) | bit as u8;
        self.nbits += 1;
        self.total_bits += 1;
        if self.nbits == 8 {
            self.buf.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
    }

    /// Écrit les `n` bits de poids faible de `v`, MSB en premier.
    pub fn put_bits(&mut self, v: u32, n: u8) {
        for i in (0..n).rev() {
            self.put_bit((v >> i) & 1 != 0);
        }
    }

    /// Nombre total de bits écrits jusqu'ici.
    pub fn bit_len(&self) -> u64 {
        self.total_bits
    }

    /// Termine le flux (bourrage à zéro jusqu'à l'octet entier).
    pub fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.cur <<= 8 - self.nbits;
            self.buf.push(self.cur);
        }
        self.buf
    }
}

/// Lecteur de bits, MSB en premier. `None` = fin du flux.
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Position en bits depuis le début.
    pos: u64,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    #[inline]
    pub fn bit_pos(&self) -> u64 {
        self.pos
    }

    pub fn seek(&mut self, bit_pos: u64) {
        self.pos = bit_pos;
    }

    #[inline]
    pub fn remaining(&self) -> u64 {
        (self.data.len() as u64 * 8).saturating_sub(self.pos)
    }

    #[inline]
    pub fn read_bit(&mut self) -> Option<bool> {
        let byte = self.data.get((self.pos / 8) as usize)?;
        let bit = (byte >> (7 - (self.pos % 8))) & 1 != 0;
        self.pos += 1;
        Some(bit)
    }

    /// Lit `n` bits (n ≤ 32), MSB en premier.
    pub fn read_bits(&mut self, n: u8) -> Option<u32> {
        if self.remaining() < n as u64 {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.read_bit()? as u32;
        }
        Some(v)
    }

    /// Regarde `n` bits sans consommer.
    pub fn peek_bits(&mut self, n: u8) -> Option<u32> {
        let save = self.pos;
        let v = self.read_bits(n);
        self.pos = save;
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_bits() {
        let mut w = BitWriter::new();
        w.put_bits(0b1011, 4);
        w.put_bits(0x00, 8);
        w.put_bits(0b1, 1);
        w.put_bits(0x2A3, 10);
        assert_eq!(w.bit_len(), 23);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(4), Some(0b1011));
        assert_eq!(r.read_bits(8), Some(0));
        assert_eq!(r.read_bits(1), Some(1));
        assert_eq!(r.read_bits(10), Some(0x2A3));
        // Bourrage : 1 bit restant à zéro.
        assert_eq!(r.read_bits(1), Some(0));
        assert_eq!(r.read_bit(), None);
    }

    #[test]
    fn peek_does_not_consume() {
        let bytes = [0b1010_0101, 0xFF];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.peek_bits(8), Some(0b1010_0101));
        assert_eq!(r.peek_bits(16), Some(0b1010_0101_1111_1111));
        assert_eq!(r.read_bits(8), Some(0b1010_0101));
        assert_eq!(r.bit_pos(), 8);
    }
}
