//! A column kept as the Parquet pages it was stored in.

use std::fmt;

use parquet::basic::{Encoding, Type as PhysicalType};
use parquet::column::page::Page;

/// One column inside one row group.
///
/// Pages stay whole. A dictionary, RLE, or bit-packed page cannot be split on
/// a row count without decoding it, so each page is the batch an operator sees.
#[derive(Debug)]
pub struct Column {
    name: String,
    physical_type: PhysicalType,
    pages: Vec<Page>,
}

impl Column {
    /// Collects one column chunk. `pages` includes the dictionary page when the
    /// column has one, followed by its data pages.
    #[must_use]
    pub fn new(name: &str, physical_type: PhysicalType, pages: Vec<Page>) -> Self {
        Self {
            name: name.to_owned(),
            physical_type,
            pages,
        }
    }

    /// Column name from the Parquet schema.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Parquet physical type. Logical types such as decimal stay inside the pages.
    #[must_use]
    pub fn physical_type(&self) -> PhysicalType {
        self.physical_type
    }

    /// Encoded pages in file order. The first page is the dictionary page when present.
    #[must_use]
    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// Bytes of encoded page buffers, after decompression.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.pages.iter().map(|page| page.buffer().len()).sum()
    }

    /// Values stored in data pages, including nulls. Dictionary entries are not rows.
    #[must_use]
    pub fn data_values(&self) -> u64 {
        self.pages
            .iter()
            .filter(|page| page.is_data_page())
            .map(|page| u64::from(page.num_values()))
            .sum()
    }
}

impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {:?} pages={} dict_pages={} values={} encoded_bytes={} encodings=",
            self.name,
            self.physical_type,
            self.pages.len(),
            self.pages
                .iter()
                .filter(|page| page.is_dictionary_page())
                .count(),
            self.data_values(),
            self.encoded_len(),
        )?;
        let mut seen: Vec<Encoding> = Vec::new();
        for page in &self.pages {
            let encoding = page.encoding();
            if seen.contains(&encoding) {
                continue;
            }
            if !seen.is_empty() {
                write!(f, "+")?;
            }
            write!(f, "{encoding:?}")?;
            seen.push(encoding);
        }
        Ok(())
    }
}
