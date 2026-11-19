# Parquet

How a file is nested, which bytes are compressed, and how the engine uses each piece. Every optional piece can be missing. The engine uses it when it is there and falls through to the next piece when it is not. The Apache format is the authority.

The file is row groups of column chunks of pages, then an uncompressed directory at the end. The directory is the footer. It points back at every chunk, and at the optional bloom filter and page index for that chunk. There is no file-level statistic. Skipping a file means every row group was skipped.

```
PAR1
│
├─ row group 0                          rows of this slice
│  │
│  ├─ column chunk                      one column, all of its rows in this slice
│  │  ├─ dictionary page                optional
│  │  │    header                       uncompressed
│  │  │    body                         compressed     the distinct values, PLAIN
│  │  ├─ data page 0
│  │  │    header                       uncompressed   type, encoding, value count, body length
│  │  │    body                         compressed     encoded values
│  │  └─ data page 1 …                  same shape, in row order
│  │
│  └─ column chunk                      next column. its pages do not line up with the one above
│
├─ row group 1                          same nesting, its own dictionary
│
├─ bloom filter                         optional, one per column chunk, uncompressed
├─ column index + offset index          optional, one pair per column chunk, uncompressed
│
└─ footer                               uncompressed
   schema
   for each row group
     for each column chunk
       file offset, codec, encodings
       min / max / null count
       where its dictionary, bloom filter, and page index are
   4 byte length
PAR1
```

The chunk's bytes are only its pages. Its offset, codec, and statistics live in the footer, not in front of the pages. A row group is not one compressed block: the codec is stored once on the column chunk and applied to each page body.

## One page

```
data page v1
┌───────────────────────────┐
│ header          plain     │  type, encoding, value count, compressed size
│                 optional  │  min / max / null count for this page only
├───────────────────────────┤
│ body            compressed│  null levels, then encoded values, then the codec
└───────────────────────────┘

data page v2
┌───────────────────────────┐
│ header          plain     │  also: whether the values are compressed
├───────────────────────────┤
│ null levels     plain     │
│ values          compressed│  only this part takes the codec
└───────────────────────────┘

dictionary page
┌───────────────────────────┐
│ header          plain     │  value count, encoding. no statistics
├───────────────────────────┤
│ body            compressed│  the distinct values, PLAIN
└───────────────────────────┘
```

The header says how many body bytes follow, so a page can be skipped by seeking past them. `UNCOMPRESSED` is a real codec: the decompress step still runs, and it copies. Snappy, Zstd, and uncompressed are the codecs that matter first. These ClickBench files are Snappy on every column, and they use v1 pages.

Decompression happens on a worker, the first time that worker decodes a page, into a buffer the thread reuses. A later vector from the same page does not decompress it again. The footer, the bloom filter, and the page index are consulted before any of this.

Inside a compressed body the values are encoded (plain, dictionary codes, RLE, delta, …). Encoding and compression are stacked: encoding first, then the codec over that page. Codes in a dictionary page mean something only inside that column chunk.

## How the engine uses it

Cheapest evidence first. A step that is missing in the file is skipped, and the rows stay in play.

```
footer
  row counts                 count(*) with no filter ends here
  chunk min / max / nulls    skip the row group, or skip reading that column
bloom filter                 skip the row group on an equality or IN
column index                 skip a page before it is read
offset index                 the byte range of each page still wanted
read compressed ranges       filter columns first
dictionary page              skip the row group, or test codes instead of values
page header                  always: the body length, so the body can be seeked past
                             sometimes: a min / max that skips the decompress
decompress the body          once per page, into this thread's buffer
decode nulls, then values    a vector of about 2,048 rows
filter                       narrow the selection; stop when it is empty
other columns                only pages that contain a surviving row
```

A range has two useful answers. **No row can match:** skip that row group, column, or page. **Every row matches:** do not read that column, and keep testing the rest of the predicate. A value that merely falls inside the range still has to be read. The statistic never names the matching rows.

When the column index is absent, a page cannot be rejected before the read. The chunk is read, headers are walked, and a page is skipped by not decompressing it. That is all these ClickBench files allow past the footer. When the column index is present, the offset index beside it is the file offset and compressed size of each page, and a rejected page is never requested.

The page header always carries the body length, so any page the engine has already decided it does not want is skipped without decompressing. A min/max inside that header is optional and only saves the decompress, because the bytes are already in hand.

The dictionary page is used twice. If every data page in the chunk is dictionary-encoded and no entry matches, the row group is skipped after that one decompress. If some entries match, later pages are filtered on their codes. `page_encoding_stats` in the footer says whether that prune is safe. Without it, the page headers are walked. A chunk that switched to PLAIN partway through cannot be pruned this way, and the reader has to handle both encodings in one chunk.

Pages of two columns do not share boundaries. The selection is row numbers within the row group, and each column finds its own pages from those numbers. A page the selection misses is not decompressed. A null count of zero means the null levels can be skipped. A null count equal to the value count means the column is all null.

## Statistics

Every statistic is optional. Missing means unknown, and unknown means those rows are read. A range proves two things:

- **No row can match.** The values lie entirely outside the predicate. Skip the row group or the page.
- **Every row matches.** The values lie entirely inside the predicate. Do not read that column. Keep the rows and evaluate the rest of the predicate.

It does not say which rows match. An equality that falls inside the min/max still reads the pages.

### Fields on a column chunk

One set per column per row group. The same record can also sit in a data page header, for that page only.

| Field | Use |
|---|---|
| `min_value`, `max_value` | The range, in the type's real sort order. Comparisons (`>`, `<`, `=`, `BETWEEN`) |
| `min`, `max` | The old range fields, signed byte order, on files written before the new fields. A byte-array range from these alone is unknown |
| `null_count` | `IS NULL` / `IS NOT NULL`. Zero nulls means the column is all present. A count equal to the value count means it is all null |
| `distinct_count` | Rarely written. Not used for skipping |
| `is_min_value_exact`, `is_max_value_exact` | False means the bound was truncated. A truncated min is above or equal to the true min. A truncated max is below or equal to the true max. The skip is valid only when the comparison still holds after that weakening |
| `nan_count` | Floats and doubles. A range that may contain NaN does not prove a comparison |

`page_encoding_stats` is on the chunk and is not a min/max. It lists the encodings used for the dictionary page and for the data pages. Dictionary pruning is valid only when every data page is dictionary-encoded. When the list is absent, the page headers are walked to learn the same fact.

### Column index

Per page: min, max, whether the page is all nulls, and sometimes a null count. This is the per-page range that can drop a page before the read. The offset index next to it is each page's file offset, compressed size, and first row number.

### Bloom filter

A split-block bloom filter over the chunk's values. A miss proves the value is absent. A hit proves nothing, and it cannot prove that every row matches. The header's only compression is uncompressed.

### Dictionary

The values themselves, not a statistic record. When the chunk is entirely dictionary-encoded, decompress and decode that page, test the predicate on each entry, and skip the row group when none match. Data pages that stayed dictionary-encoded are then filtered on codes. A chunk that switched to PLAIN partway through cannot be pruned this way. The footer offset of the dictionary page is what lets that page be read on its own.

## What these files have

**TPC-H** (`tpchgen-cli`, `ZSTD(1)`, written by parquet-rs): every column chunk has a column index and an offset index, so per-page min/max and page locations are available. No bloom filters. `lineitem` has 53 row groups of about 113k rows, and about 6 data pages of about 19k rows per column chunk. Rows are in key order (`l_orderkey`), and dates are spread across keys, so every row group and every page spans the whole 1992-1998 date range: date filters prune nothing. Decimals are `INT64` at scale 2 and dates are `INT32` days, with exact statistics. Dictionary pages can be large: `l_extendedprice` has 18 MB of dictionary against 12 MB of data pages. The offset index lists only data pages.

**ClickBench** (`hits_0`, parquet-cpp 1.5.1, Snappy): 2 row groups (450,560 and 549,440 rows). On `hits_0`, all 210 column chunks have min, max, and null count, and none have a distinct count. The bounds are the new `min_value` / `max_value` fields. Every byte-array bound is marked inexact. The other types are exact. There is no bloom filter and no page index. The page headers checked so far (`EventDate`, `AdvEngineID`, `URL`) carry no statistics. The footer is the only statistic these files offer.

## Footer cost

Measured 2026-11-13 with the `parquet` crate 60.1.0. Cold read is `open` plus the 8-byte tail plus the footer bytes, one shot after `sudo purge`. Warm columns are the median of 5 cached runs, ignoring the first. Parse is `ParquetMetaDataReader::decode_metadata` on those bytes; open+parse is `parse_and_finish`, what a normal open does. The parser was already resident, so the cold parse is decode time, not code being faulted back in. None of these files has a page index.

| File | Footer | Cold read | Cold parse | Warm read | Warm parse | Warm open+parse |
|---|---|---|---|---|---|---|
| `hits_0.parquet` | 23,946 B | 0.49 ms | 0.08 ms | 0.02 ms | 0.07 ms | 0.08 ms |
| `hits.parquet` | 2,439,316 B | 1.0 ms | 6.6 ms | 0.44 ms | 5.8 ms | 6.7 ms |
| 100 partitions | 3,683,826 B | 39 ms | 11 ms | 2.0 ms | 11 ms | 13 ms |

Partition footers range from 22,863 to 57,061 bytes, median 34,546. Parsing runs at about 400 MB/s of footer bytes. The footer is about 11 KB per row group on the 105-column ClickBench schema (2.44 MB / 226), about 100 bytes per column chunk, plus the schema once per file.

A cold read follows the number of files, not the bytes: the 24 KB and 2.4 MB footers both cost about a millisecond, a seek plus a short transfer, and 100 files cost about 0.4 ms each. Once the tail is cached, parsing is the whole cost. Many files cost more per byte than one footer, because each repeats the schema and is decoded on its own.
