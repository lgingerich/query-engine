//! One Parquet file's footer, column chunks, and pages, as the bounds need them

use std::error::Error;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use datafusion::arrow::datatypes::Schema;
use datafusion::parquet::file::metadata::{
    PageIndexPolicy, ParquetMetaData, ParquetMetaDataReader,
};

use crate::stats::{Kind, Range};

/// An opened file: metadata with the page index, if the file has one
pub(crate) struct ParquetFile {
    meta: ParquetMetaData,
    /// Footer metadata plus its 8-byte tail
    pub(crate) footer_bytes: u64,
}

/// One column chunk, as far as the bounds go
pub(crate) struct Chunk {
    pub(crate) rows: u64,
    pub(crate) compressed: u64,
    pub(crate) uncompressed: u64,
    /// Column index plus offset index
    pub(crate) index_bytes: u64,
    pub(crate) range: Range,
    /// Data pages from the offset index, or `None` without one
    pub(crate) pages: Option<Vec<Page>>,
}

/// One data page. Rows are within the row group
pub(crate) struct Page {
    pub(crate) first_row: u64,
    pub(crate) rows: u64,
    /// Compressed, including the page header
    pub(crate) bytes: u64,
    pub(crate) range: Range,
}

impl Chunk {
    /// Bytes outside the data pages: the dictionary page and any other pages
    /// the offset index doesn't list. Needed whenever any data page is read
    pub(crate) fn dictionary_bytes(&self) -> u64 {
        self.pages.as_ref().map_or(0, |pages| {
            self.compressed
                .saturating_sub(pages.iter().map(|page| page.bytes).sum())
        })
    }

    /// The page covering row `row` of the row group
    pub(crate) fn page_at(&self, row: u64) -> Option<&Page> {
        let pages = self.pages.as_ref()?;
        let after = pages.partition_point(|page| page.first_row <= row);
        pages.get(after.checked_sub(1)?)
    }
}

impl ParquetFile {
    pub(crate) fn open(path: &Path) -> Result<Self, Box<dyn Error>> {
        let mut file = File::open(path)?;
        let meta = ParquetMetaDataReader::new()
            .with_page_index_policy(PageIndexPolicy::Optional)
            .parse_and_finish(&file)?;
        // The last 8 bytes are the footer length (little-endian u32) and `PAR1`
        let mut tail = [0u8; 8];
        file.seek(SeekFrom::End(-8))?;
        file.read_exact(&mut tail)?;
        let length = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
        Ok(Self {
            meta,
            footer_bytes: u64::from(length) + 8,
        })
    }

    pub(crate) fn num_row_groups(&self) -> usize {
        self.meta.num_row_groups()
    }

    pub(crate) fn row_group_rows(&self, rg: usize) -> u64 {
        u64::try_from(self.meta.row_group(rg).num_rows()).unwrap_or(0)
    }

    pub(crate) fn rows(&self) -> u64 {
        u64::try_from(self.meta.file_metadata().num_rows()).unwrap_or(0)
    }

    /// Leaf column index of the top-level column `name`
    pub(crate) fn leaf(&self, name: &str) -> Option<usize> {
        self.meta
            .file_metadata()
            .schema_descr()
            .columns()
            .iter()
            .position(|column| column.path().parts() == [name])
    }

    /// How `name`'s statistics compare, from its Arrow type and physical type
    pub(crate) fn kind(&self, name: &str, schema: &Schema) -> Kind {
        match (self.leaf(name), schema.field_with_name(name)) {
            (Some(leaf), Ok(field)) => Kind::of(
                field,
                self.meta
                    .file_metadata()
                    .schema_descr()
                    .column(leaf)
                    .physical_type(),
            ),
            _ => Kind::Other,
        }
    }

    /// Column chunk `leaf` of row group `rg`
    pub(crate) fn chunk(&self, rg: usize, leaf: usize, kind: Kind) -> Chunk {
        let group = self.meta.row_group(rg);
        let column = group.column(leaf);
        let rows = self.row_group_rows(rg);
        let size = |bytes: Option<i32>| bytes.and_then(|b| u64::try_from(b).ok()).unwrap_or(0);
        let page_index = self.meta.page_index_for_row_group(rg);
        let offsets = page_index.offset_index(leaf);
        let column_index = page_index.column_index(leaf);
        let pages = offsets.map(|offsets| {
            let locations = offsets.page_locations();
            locations
                .iter()
                .enumerate()
                .map(|(i, location)| {
                    let first_row = u64::try_from(location.first_row_index).unwrap_or(0);
                    let end = locations.get(i + 1).map_or(rows, |next| {
                        u64::try_from(next.first_row_index).unwrap_or(rows)
                    });
                    let page_rows = end.saturating_sub(first_row);
                    Page {
                        first_row,
                        rows: page_rows,
                        bytes: u64::try_from(location.compressed_page_size).unwrap_or(0),
                        range: column_index.map_or_else(
                            || Range {
                                rows: page_rows,
                                ..Range::default()
                            },
                            |index| Range::of_page(index, i, kind, page_rows),
                        ),
                    }
                })
                .collect()
        });
        Chunk {
            rows,
            compressed: u64::try_from(column.compressed_size()).unwrap_or(0),
            uncompressed: u64::try_from(column.uncompressed_size()).unwrap_or(0),
            index_bytes: size(column.column_index_length()) + size(column.offset_index_length()),
            range: Range::of_chunk(column.statistics(), kind, rows),
            pages,
        }
    }
}
