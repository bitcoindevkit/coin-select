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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;
    use proptest::prelude::*;

    #[test]
    fn empty_bitset() {
        let b = Bitset::with_capacity(0);
        assert_eq!(b.capacity(), 0);
        assert_eq!(b.len(), 0);
        assert!(b.is_empty());
        assert!(!b.contains(0));
        assert!(!b.contains(usize::MAX));
        assert_eq!(b.iter().next(), None);
        assert_eq!(b.iter().next_back(), None);
    }

    #[test]
    fn default_matches_zero_capacity() {
        assert_eq!(Bitset::default(), Bitset::with_capacity(0));
    }

    #[test]
    fn capacity_reported_verbatim() {
        for cap in [1, 5, 63, 64, 65, 127, 128, 129, 1000] {
            assert_eq!(Bitset::with_capacity(cap).capacity(), cap);
        }
    }

    #[test]
    fn insert_returns_was_unset() {
        let mut b = Bitset::with_capacity(10);
        assert!(b.insert(3));
        assert!(!b.insert(3));
        assert!(b.contains(3));
    }

    #[test]
    fn remove_returns_was_set() {
        let mut b = Bitset::with_capacity(10);
        b.insert(5);
        assert!(b.remove(5));
        assert!(!b.remove(5));
        assert!(!b.contains(5));
    }

    #[test]
    fn out_of_range_is_safe() {
        let mut b = Bitset::with_capacity(10);
        assert!(!b.contains(10));
        assert!(!b.contains(usize::MAX));
        assert!(!b.remove(10));
        assert!(!b.remove(usize::MAX));
    }

    #[test]
    fn word_boundary_bits() {
        let mut b = Bitset::with_capacity(192);
        let expected = [0, 1, 62, 63, 64, 65, 126, 127, 128, 129, 190, 191];
        for &i in &expected {
            assert!(b.insert(i));
        }
        for &i in &expected {
            assert!(b.contains(i));
        }
        assert_eq!(b.len(), expected.len());
        assert_eq!(
            b.iter().collect::<alloc::vec::Vec<_>>(),
            expected.to_vec()
        );
        assert_eq!(
            b.iter().rev().collect::<alloc::vec::Vec<_>>(),
            expected.iter().rev().copied().collect::<alloc::vec::Vec<_>>()
        );
    }

    #[test]
    fn non_word_aligned_capacity_last_bit() {
        let mut b = Bitset::with_capacity(100);
        assert!(b.insert(99));
        assert!(b.contains(99));
        assert!(!b.contains(100));
        assert_eq!(b.iter().collect::<alloc::vec::Vec<_>>(), [99]);
    }

    #[test]
    fn iter_size_hint_decrements() {
        let mut b = Bitset::with_capacity(200);
        for i in [0, 7, 63, 64, 199] {
            b.insert(i);
        }
        let mut iter = b.iter();
        for expected in (1..=5).rev() {
            assert_eq!(iter.size_hint(), (expected, Some(expected)));
            assert_eq!(iter.len(), expected);
            iter.next().unwrap();
        }
        assert_eq!(iter.size_hint(), (0, Some(0)));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn double_ended_meets_in_middle() {
        let mut b = Bitset::with_capacity(200);
        for i in [0, 5, 63, 64, 127, 128, 199] {
            b.insert(i);
        }
        let mut it = b.iter();
        assert_eq!(it.next(), Some(0));
        assert_eq!(it.next_back(), Some(199));
        assert_eq!(it.next(), Some(5));
        assert_eq!(it.next_back(), Some(128));
        assert_eq!(it.next(), Some(63));
        assert_eq!(it.next_back(), Some(127));
        assert_eq!(it.next(), Some(64));
        assert_eq!(it.next(), None);
        assert_eq!(it.next_back(), None);
    }

    #[test]
    fn double_ended_back_only_full_drain() {
        let mut b = Bitset::with_capacity(200);
        for i in [0, 5, 63, 64, 127, 128, 199] {
            b.insert(i);
        }
        let mut out = alloc::vec::Vec::new();
        let mut it = b.iter();
        while let Some(v) = it.next_back() {
            out.push(v);
        }
        assert_eq!(out, [199, 128, 127, 64, 63, 5, 0]);
    }

    #[test]
    fn equality_set_based() {
        let mut a = Bitset::with_capacity(100);
        let mut b = Bitset::with_capacity(100);
        for i in [3, 50, 99] {
            a.insert(i);
            b.insert(i);
        }
        assert_eq!(a, b);
        b.remove(50);
        assert_ne!(a, b);
    }

    #[test]
    fn into_iterator_for_ref() {
        let mut b = Bitset::with_capacity(10);
        b.insert(2);
        b.insert(7);
        let v: alloc::vec::Vec<usize> = (&b).into_iter().collect();
        assert_eq!(v, [2, 7]);
    }

    proptest! {
        /// Any sequence of insert/remove/contains operations on Bitset must
        /// produce identical results to the same sequence on BTreeSet.
        #[test]
        fn matches_btreeset_under_random_ops(
            cap in 1usize..256,
            ops in prop::collection::vec((any::<bool>(), 0usize..256), 0..200),
        ) {
            let mut bitset = Bitset::with_capacity(cap);
            let mut model: BTreeSet<usize> = BTreeSet::new();
            for (is_insert, raw_idx) in ops {
                let idx = raw_idx % cap;
                if is_insert {
                    prop_assert_eq!(bitset.insert(idx), model.insert(idx));
                } else {
                    prop_assert_eq!(bitset.remove(idx), model.remove(&idx));
                }
                prop_assert_eq!(bitset.contains(idx), model.contains(&idx));
                prop_assert_eq!(bitset.len(), model.len());
                prop_assert_eq!(bitset.is_empty(), model.is_empty());
            }
            let bvec: alloc::vec::Vec<usize> = bitset.iter().collect();
            let mvec: alloc::vec::Vec<usize> = model.iter().copied().collect();
            prop_assert_eq!(&bvec, &mvec);
            let brev: alloc::vec::Vec<usize> = bitset.iter().rev().collect();
            let mrev: alloc::vec::Vec<usize> = model.iter().rev().copied().collect();
            prop_assert_eq!(brev, mrev);
        }

        /// Interleaved next() / next_back() must yield each set bit exactly
        /// once, in the order dictated by the call pattern.
        #[test]
        fn double_ended_interleaved_matches_model(
            cap in 1usize..256,
            bits in prop::collection::vec(0usize..256, 0..100),
            front_first in prop::collection::vec(any::<bool>(), 0..200),
        ) {
            let mut bitset = Bitset::with_capacity(cap);
            let mut model: BTreeSet<usize> = BTreeSet::new();
            for raw in bits {
                let i = raw % cap;
                bitset.insert(i);
                model.insert(i);
            }
            let mut it = bitset.iter();
            let mut model_vec: alloc::vec::Vec<usize> = model.iter().copied().collect();
            for take_front in front_first {
                let model_pick = if take_front {
                    if model_vec.is_empty() { None } else { Some(model_vec.remove(0)) }
                } else {
                    model_vec.pop()
                };
                let bitset_pick = if take_front { it.next() } else { it.next_back() };
                prop_assert_eq!(bitset_pick, model_pick);
                if bitset_pick.is_none() { break; }
            }
        }
    }
}
