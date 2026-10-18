//! Small deterministic random number generator and hashing helpers.
//!
//! The simulator must be reproducible from [`SimConfig::seed`](crate::SimConfig),
//! so it uses its own SplitMix64 generator instead of an external crate.

/// SplitMix64 pseudo random number generator.
#[derive(Clone, Debug)]
pub(crate) struct Rng {
    state: u64,
}

impl Rng {
    /// A generator seeded from `seed` and a stream identifier, so that
    /// independent consumers (fault injection, sensor noise, bed shape) do
    /// not influence each other's sequences.
    pub(crate) fn new(seed: u64, stream: u64) -> Rng {
        let mut rng = Rng {
            state: seed ^ stream.wrapping_mul(0xA076_1D64_78BD_642F),
        };
        // Discard the first output so nearby seeds diverge quickly.
        rng.next_u64();
        rng
    }

    /// Next 64 random bits.
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform value in `0.0..1.0`.
    pub(crate) fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform value in `lo..hi`.
    pub(crate) fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    /// True with probability `p` (never draws when `p <= 0`).
    pub(crate) fn chance(&mut self, p: f32) -> bool {
        if p <= 0.0 {
            return false;
        }
        if p >= 1.0 {
            return true;
        }
        self.next_f64() < f64::from(p)
    }
}

/// 64-bit FNV-1a hash of `bytes`, used for deterministic identifiers.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// CRC-16/XMODEM (polynomial 0x1021), as used by Marlin for its EEPROM.
pub(crate) fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_per_seed_and_stream() {
        let a: Vec<u64> = {
            let mut r = Rng::new(42, 1);
            (0..4).map(|_| r.next_u64()).collect()
        };
        let b: Vec<u64> = {
            let mut r = Rng::new(42, 1);
            (0..4).map(|_| r.next_u64()).collect()
        };
        let c: Vec<u64> = {
            let mut r = Rng::new(42, 2);
            (0..4).map(|_| r.next_u64()).collect()
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn floats_in_range_and_chance_edges() {
        let mut r = Rng::new(7, 0);
        for _ in 0..1000 {
            let v = r.next_f64();
            assert!((0.0..1.0).contains(&v));
            let u = r.uniform(-2.0, 3.0);
            assert!((-2.0..3.0).contains(&u));
        }
        assert!(!r.chance(0.0));
        assert!(r.chance(1.0));
        let hits = (0..10_000).filter(|_| r.chance(0.25)).count();
        assert!((2000..3000).contains(&hits), "{hits}");
    }

    #[test]
    fn crc16_known_value() {
        // CRC-16/XMODEM check value for "123456789".
        assert_eq!(crc16(b"123456789"), 0x31C3);
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
    }
}
