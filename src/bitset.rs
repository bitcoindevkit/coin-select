use alloc::{vec, vec::Vec};

/// A compact bitset addressed by candidate index, used by
/// [`CoinSelector`](crate::CoinSelector) to track which candidates are
/// currently selected or banned.
///
/// Bit `i` corresponds to the candidate at index `i` in the slice passed to
/// [`CoinSelector::new`](crate::CoinSelector::new). The capacity is fixed at
/// construction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bitset {
    words: Vec<u64>,
    bit_capacity: usize,
}

impl Bitset {
    pub(crate) fn with_capacity(bit_capacity: usize) -> Self {
        Self {
            words: vec![0; (bit_capacity + 63) / 64],
            bit_capacity,
        }
    }

    /// Bit-addressable capacity (number of candidates the bitset can represent).
    pub fn capacity(&self) -> usize {
        self.bit_capacity
    }

    /// Number of set bits.
    pub fn len(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Whether no bits are set.
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|w| *w == 0)
    }

    /// Whether bit at `index` is set. Out-of-range indices return `false`.
    pub fn contains(&self, index: usize) -> bool {
        index < self.bit_capacity && self.words[index / 64] & (1u64 << (index % 64)) != 0
    }

    /// Set bit at `index`. Returns `true` if the bit was previously unset.
    pub(crate) fn insert(&mut self, index: usize) -> bool {
        debug_assert!(index < self.bit_capacity);
        let mask = 1u64 << (index % 64);
        let w = &mut self.words[index / 64];
        let was_unset = *w & mask == 0;
        *w |= mask;
        was_unset
    }

    /// Clear bit at `index`. Returns `true` if the bit was previously set.
    pub(crate) fn remove(&mut self, index: usize) -> bool {
        if index >= self.bit_capacity {
            return false;
        }
        let mask = 1u64 << (index % 64);
        let w = &mut self.words[index / 64];
        let was_set = *w & mask != 0;
        *w &= !mask;
        was_set
    }

    /// Iterate over set bits in ascending index order.
    pub fn iter(&self) -> BitsetIter<'_> {
        BitsetIter {
            words: &self.words,
            front: 0,
            back: self.bit_capacity,
            remaining: self.len(),
        }
    }
}

impl<'a> IntoIterator for &'a Bitset {
    type Item = usize;
    type IntoIter = BitsetIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the set bits of a [`Bitset`].
#[derive(Clone, Debug)]
pub struct BitsetIter<'a> {
    words: &'a [u64],
    front: usize,
    back: usize,
    remaining: usize,
}

impl<'a> Iterator for BitsetIter<'a> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        if self.remaining == 0 {
            return None;
        }
        while self.front < self.back {
            let word_i = self.front / 64;
            let bit_i = self.front % 64;
            let word = self.words[word_i];
            let masked = word & !((1u64 << bit_i).wrapping_sub(1));
            if masked != 0 {
                let bit = masked.trailing_zeros() as usize;
                let idx = word_i * 64 + bit;
                if idx >= self.back {
                    return None;
                }
                self.front = idx + 1;
                self.remaining -= 1;
                return Some(idx);
            }
            self.front = (word_i + 1) * 64;
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<'a> ExactSizeIterator for BitsetIter<'a> {}

impl<'a> DoubleEndedIterator for BitsetIter<'a> {
    fn next_back(&mut self) -> Option<usize> {
        if self.remaining == 0 {
            return None;
        }
        while self.front < self.back {
            let bit_pos = self.back - 1;
            let word_i = bit_pos / 64;
            let last_bit = bit_pos % 64;
            let word = self.words[word_i];
            // Keep only bits 0..=last_bit within this word.
            let keep_mask = if last_bit == 63 {
                u64::MAX
            } else {
                (1u64 << (last_bit + 1)) - 1
            };
            let masked = word & keep_mask;
            if masked != 0 {
                let bit = 63 - masked.leading_zeros() as usize;
                let idx = word_i * 64 + bit;
                if idx < self.front {
                    return None;
                }
                self.back = idx;
                self.remaining -= 1;
                return Some(idx);
            }
            self.back = word_i * 64;
        }
        None
    }
}
