//! Packed bits, one per row. Used for null tracking and row selections

/// One bit per row
///
/// Bit `i` lives in `words[i / 64]` at position `i % 64` (LSB first). Bits past
/// `len` are always `0`, so whole-word ops never see garbage
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bitmap {
    words: Vec<u64>,
    len: usize,
}

impl Bitmap {
    /// Empty bitmap with room for `capacity` bits
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            words: Vec::with_capacity(capacity.div_ceil(64)),
            len: 0,
        }
    }

    /// `len` bits, all set
    #[must_use]
    pub fn all_set(len: usize) -> Self {
        let mut words = vec![u64::MAX; len.div_ceil(64)];
        if let Some(last) = words.last_mut()
            && !len.is_multiple_of(64)
        {
            *last = (1 << (len % 64)) - 1;
        }
        Self { words, len }
    }

    /// Number of bits
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether there are no bits
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends one bit
    pub fn push(&mut self, bit: bool) {
        let offset = self.len % 64;
        if offset == 0 {
            self.words.push(0);
        }
        if bit && let Some(word) = self.words.last_mut() {
            *word |= 1 << offset;
        }
        self.len += 1;
    }

    /// Bit `i`, or `None` if out of range
    #[must_use]
    pub fn get(&self, i: usize) -> Option<bool> {
        if i >= self.len {
            return None;
        }
        self.words
            .get(i / 64)
            .map(|word| word & (1 << (i % 64)) != 0)
    }

    /// Number of set bits
    #[must_use]
    pub fn count_set(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    /// Indices of set bits, in order. Skips 64 unset bits per step
    pub fn set_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(w, &word)| {
            let mut rest = word;
            std::iter::from_fn(move || {
                if rest == 0 {
                    return None;
                }
                let bit = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                Some(w * 64 + bit)
            })
        })
    }

    /// Bitwise AND, for combining a selection with validity. `None` if lengths differ
    #[must_use]
    pub fn and(&self, other: &Self) -> Option<Self> {
        if self.len != other.len {
            return None;
        }
        let words = self
            .words
            .iter()
            .zip(&other.words)
            .map(|(a, b)| a & b)
            .collect();
        Some(Self {
            words,
            len: self.len,
        })
    }
}
