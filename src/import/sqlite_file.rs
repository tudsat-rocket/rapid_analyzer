//! A read-only reader for the SQLite file format, in plain Rust.
//!
//! It exists for the web build: `rusqlite` compiles SQLite from C, which has
//! no `wasm32-unknown-unknown` build, and a browser hands over a file's bytes
//! rather than a path anyway. The sensor log importer needs exactly one thing
//! from a database -- every row of one table -- so that is all this does: no
//! SQL, no indexes, no writing. It walks the table's b-tree in rowid order
//! and decodes each record.
//!
//! Everything here follows <https://www.sqlite.org/fileformat2.html>; the
//! section names in the comments are that document's. The two details that
//! are easy to miss, and are therefore tested: a row too big for its page
//! continues on a chain of overflow pages, and a `REAL` column may hold an
//! *integer* on disk, which SQLite does to save space for whole numbers.
//!
//! Not supported, with an error rather than a wrong answer: UTF-16 databases
//! and `WITHOUT ROWID` tables. A write-ahead log (`-wal` file) is never read,
//! so rows the writer had not yet checkpointed into the main file are not
//! there -- the same as copying the database file alone anywhere else.

use anyhow::{Context as _, Result, bail, ensure};

const MAGIC: &[u8] = b"SQLite format 3\0";
const HEADER_LEN: usize = 100;

const INTERIOR_TABLE: u8 = 0x05;
const LEAF_TABLE: u8 = 0x0d;

/// One value of a row, borrowing from the database (or, for a row that
/// spilled onto overflow pages, from the buffer it was reassembled in).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    Null,
    Integer(i64),
    Real(f64),
    Text(&'a str),
    Blob(&'a [u8]),
}

impl Value<'_> {
    /// A number, whichever way SQLite chose to store it.
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::Integer(i) => Some(i as f64),
            Value::Real(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
}

/// A table as the schema describes it.
#[derive(Clone, Debug)]
pub struct Table {
    pub name: String,
    /// Column names, in the order a record stores them.
    pub columns: Vec<String>,
    root_page: u32,
    /// The `INTEGER PRIMARY KEY` column, if there is one. Its value is the
    /// rowid, and the record itself only holds a NULL in its place.
    rowid_alias: Option<usize>,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.eq_ignore_ascii_case(name))
    }
}

pub struct Database<'a> {
    bytes: &'a [u8],
    page_size: usize,
    /// Page size less the bytes an extension reserves at the end of each
    /// page. Every size calculation in the format is in terms of this.
    usable: usize,
    page_count: usize,
}

impl<'a> Database<'a> {
    pub fn open(bytes: &'a [u8]) -> Result<Self> {
        ensure!(bytes.len() >= HEADER_LEN && bytes.starts_with(MAGIC), "not an SQLite database");
        // "The database header": the page size is a big-endian u16, where 1
        // stands for 65536, which does not fit.
        let page_size = match u16::from_be_bytes([bytes[16], bytes[17]]) {
            1 => 65536,
            n => n as usize,
        };
        ensure!(
            page_size.is_power_of_two() && (512..=65536).contains(&page_size),
            "invalid page size {page_size}"
        );
        let reserved = bytes[20] as usize;
        let usable = page_size - reserved;
        ensure!(usable >= 480, "invalid reserved space {reserved} for page size {page_size}");
        let encoding = u32::from_be_bytes(bytes[56..60].try_into().unwrap());
        ensure!(
            encoding <= 1,
            "the database stores text as UTF-16, which this reader does not support"
        );
        Ok(Self {
            bytes,
            page_size,
            usable,
            page_count: bytes.len() / page_size,
        })
    }

    /// The table called `name` (case-insensitively, like SQL).
    pub fn table(&self, name: &str) -> Result<Table> {
        let mut found = None;
        // "Storage of the SQL database schema": page 1 is the root of the
        // `sqlite_schema` table (type, name, tbl_name, rootpage, sql).
        self.scan(1, &mut |_, row| {
            if found.is_none()
                && row.first().and_then(Value::as_str) == Some("table")
                && row.get(1).and_then(Value::as_str).is_some_and(|n| n.eq_ignore_ascii_case(name))
            {
                let root = match row.get(3) {
                    Some(Value::Integer(n)) => u32::try_from(*n).context("invalid root page")?,
                    _ => bail!("table {name} has no root page"),
                };
                let sql = row.get(4).and_then(Value::as_str).unwrap_or_default();
                found = Some((root, sql.to_string()));
            }
            Ok(())
        })?;
        let (root_page, sql) = found.with_context(|| format!("no table named {name}"))?;
        let (columns, rowid_alias) = parse_columns(&sql).with_context(|| format!("reading the definition of {name}"))?;
        Ok(Table {
            name: name.to_string(),
            columns,
            root_page,
            rowid_alias,
        })
    }

    /// Calls `f` with every row of `table`, in rowid order. A row with fewer
    /// values than the table has columns -- written before an `ALTER TABLE
    /// ADD COLUMN` -- is padded with NULLs.
    pub fn for_each_row(&self, table: &Table, mut f: impl FnMut(&[Value<'_>]) -> Result<()>) -> Result<()> {
        let width = table.columns.len();
        self.scan(table.root_page, &mut |rowid, row| {
            let alias = table
                .rowid_alias
                .filter(|&i| row.get(i).is_none_or(|v| *v == Value::Null));
            // The common case -- every value there, no rowid to fill in --
            // hands the row over as it is.
            if row.len() >= width && alias.is_none() {
                return f(row);
            }
            let mut padded = row.to_vec();
            padded.resize(width.max(row.len()), Value::Null);
            if let Some(i) = alias {
                padded[i] = Value::Integer(rowid);
            }
            f(&padded)
        })
    }

    /// Walks the table b-tree rooted at `root`, depth first and left to
    /// right, which is rowid order.
    fn scan(&self, root: u32, f: &mut dyn FnMut(i64, &[Value<'_>]) -> Result<()>) -> Result<()> {
        let mut stack = vec![root];
        let mut visited = 0usize;
        let mut overflow_buf = Vec::new();
        while let Some(number) = stack.pop() {
            // A well-formed tree visits each page once; a corrupt one could
            // point back at itself forever.
            visited += 1;
            ensure!(visited <= self.page_count, "the table's b-tree has a cycle");

            let page = self.page(number)?;
            // Page 1 starts with the database header; the b-tree header
            // follows it, but cell offsets still count from the page start.
            let header = if number == 1 { HEADER_LEN } else { 0 };
            let kind = page[header];
            let cells = u16::from_be_bytes([page[header + 3], page[header + 4]]) as usize;
            match kind {
                INTERIOR_TABLE => {
                    let right = read_u32(page, header + 8)?;
                    let pointers = header + 12;
                    // Pushed in reverse so the leftmost child comes off first.
                    stack.push(right);
                    for i in (0..cells).rev() {
                        let cell = cell_offset(page, pointers, i)?;
                        stack.push(read_u32(page, cell)?);
                    }
                }
                LEAF_TABLE => {
                    let pointers = header + 8;
                    for i in 0..cells {
                        let mut at = cell_offset(page, pointers, i)?;
                        let payload_len = read_varint(page, &mut at)? as usize;
                        let rowid = read_varint(page, &mut at)? as i64;
                        let payload = self.payload(page, at, payload_len, &mut overflow_buf)?;
                        let mut values = Vec::with_capacity(8);
                        decode_record(payload, &mut values)?;
                        f(rowid, &values)?;
                    }
                }
                0x02 | 0x0a => bail!("page {number} belongs to an index, not a table (a WITHOUT ROWID table?)"),
                other => bail!("page {number} has unknown b-tree page type {other:#04x}"),
            }
        }
        Ok(())
    }

    fn page(&self, number: u32) -> Result<&'a [u8]> {
        let index = (number as usize).checked_sub(1).context("page number 0")?;
        ensure!(index < self.page_count, "page {number} is past the end of the file");
        let start = index * self.page_size;
        Ok(&self.bytes[start..start + self.usable])
    }

    /// A cell's payload: in place if it fits on the page, otherwise
    /// reassembled from the overflow chain into `buf`.
    fn payload<'b>(&self, page: &'b [u8], at: usize, len: usize, buf: &'b mut Vec<u8>) -> Result<&'b [u8]>
    where
        'a: 'b,
    {
        // "B-tree Pages", the table leaf case: how much of the payload is
        // kept on the page itself.
        let u = self.usable;
        let max_local = u - 35;
        let local = if len <= max_local {
            len
        } else {
            let min_local = (u - 12) * 32 / 255 - 23;
            let k = min_local + (len - min_local) % (u - 4);
            if k <= max_local { k } else { min_local }
        };
        let on_page = page.get(at..at + local).context("cell runs past the end of its page")?;
        if local == len {
            return Ok(on_page);
        }

        buf.clear();
        buf.extend_from_slice(on_page);
        let mut next = read_u32(page, at + local)?;
        let mut hops = 0usize;
        while buf.len() < len {
            hops += 1;
            ensure!(next != 0 && hops <= self.page_count, "overflow chain ends early");
            let overflow = self.page(next)?;
            next = read_u32(overflow, 0)?;
            let take = (len - buf.len()).min(u - 4);
            buf.extend_from_slice(&overflow[4..4 + take]);
        }
        Ok(buf)
    }
}

fn cell_offset(page: &[u8], pointers: usize, i: usize) -> Result<usize> {
    let at = pointers + 2 * i;
    let bytes = page.get(at..at + 2).context("cell pointer array runs past its page")?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]) as usize)
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32> {
    let b = bytes.get(at..at + 4).context("truncated page")?;
    Ok(u32::from_be_bytes(b.try_into().unwrap()))
}

/// "Varint": one to nine bytes, big-endian, seven bits a byte with the top
/// bit meaning "more follows" -- except the ninth, which contributes all
/// eight.
fn read_varint(bytes: &[u8], at: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for i in 0..9 {
        let byte = *bytes.get(*at).context("truncated varint")?;
        *at += 1;
        if i == 8 {
            return Ok((value << 8) | byte as u64);
        }
        value = (value << 7) | (byte & 0x7f) as u64;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    unreachable!()
}

/// "Record Format": a header of serial types, then the values they describe.
fn decode_record<'b>(payload: &'b [u8], out: &mut Vec<Value<'b>>) -> Result<()> {
    let mut at = 0;
    let header_len = read_varint(payload, &mut at)? as usize;
    ensure!(header_len <= payload.len(), "record header runs past its payload");
    let mut body = header_len;
    while at < header_len {
        let serial = read_varint(payload, &mut at)?;
        let size = match serial {
            0 | 8 | 9 => 0,
            1..=4 => serial as usize,
            5 => 6,
            6 | 7 => 8,
            10 | 11 => bail!("reserved serial type {serial}"),
            n => ((n - 12) / 2) as usize,
        };
        let data = payload.get(body..body + size).context("record value runs past its payload")?;
        body += size;
        out.push(match serial {
            0 => Value::Null,
            1..=6 => {
                // Big-endian two's complement, sign-extended from its width.
                let raw = data.iter().fold(0i64, |acc, &b| (acc << 8) | b as i64);
                let shift = 64 - 8 * size as u32;
                Value::Integer((raw << shift) >> shift)
            }
            7 => Value::Real(f64::from_be_bytes(data.try_into().unwrap())),
            8 => Value::Integer(0),
            9 => Value::Integer(1),
            n if n % 2 == 0 => Value::Blob(data),
            _ => Value::Text(std::str::from_utf8(data).context("text that is not UTF-8")?),
        });
    }
    Ok(())
}

/// Column names out of a `CREATE TABLE` statement, and which one (if any)
/// is an `INTEGER PRIMARY KEY` and so stands for the rowid.
///
/// Only as much SQL as that takes: the text between the outermost
/// parentheses, split on the commas that are not inside nested ones (a
/// `DECIMAL(10, 2)`, a `CHECK (...)`), with table constraints skipped.
fn parse_columns(sql: &str) -> Result<(Vec<String>, Option<usize>)> {
    let open = sql.find('(').context("no column list")?;
    let close = sql.rfind(')').context("no column list")?;
    ensure!(close > open, "no column list");
    ensure!(
        !sql[close..].to_ascii_uppercase().contains("WITHOUT ROWID"),
        "WITHOUT ROWID tables are not supported"
    );

    let mut definitions = Vec::new();
    let (mut depth, mut start) = (0usize, open + 1);
    let mut quote = None;
    for (i, c) in sql[..close].char_indices().skip_while(|&(i, _)| i <= open) {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'' | '`') => quote = Some(c),
            (None, '[') => quote = Some(']'),
            (None, '(') => depth += 1,
            (None, ')') => depth = depth.saturating_sub(1),
            (None, ',') if depth == 0 => {
                definitions.push(&sql[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    definitions.push(&sql[start..close]);

    let mut columns = Vec::new();
    let mut rowid_alias = None;
    for definition in definitions {
        let definition = definition.trim();
        let upper = definition.to_ascii_uppercase();
        let first_word = upper.split_whitespace().next().unwrap_or_default();
        if ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"].contains(&first_word) {
            continue;
        }
        let (name, rest) = split_name(definition);
        ensure!(!name.is_empty(), "a column without a name");
        let rest = rest.to_ascii_uppercase();
        if rest.split_whitespace().next() == Some("INTEGER") && rest.contains("PRIMARY KEY") {
            rowid_alias = Some(columns.len());
        }
        columns.push(name);
    }
    ensure!(!columns.is_empty(), "no columns");
    Ok((columns, rowid_alias))
}

/// A column definition's name, unquoted, and whatever follows it.
fn split_name(definition: &str) -> (String, &str) {
    let close = match definition.chars().next() {
        Some('"') => '"',
        Some('`') => '`',
        Some('[') => ']',
        Some('\'') => '\'',
        _ => {
            let end = definition.find(char::is_whitespace).unwrap_or(definition.len());
            return (definition[..end].to_string(), &definition[end..]);
        }
    };
    match definition[1..].find(close) {
        Some(end) => (definition[1..1 + end].to_string(), &definition[end + 2..]),
        None => (definition[1..].to_string(), ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints() {
        let decode = |bytes: &[u8]| read_varint(bytes, &mut 0).unwrap();
        assert_eq!(decode(&[0x00]), 0);
        assert_eq!(decode(&[0x7f]), 127);
        assert_eq!(decode(&[0x81, 0x00]), 128);
        assert_eq!(decode(&[0xff; 9]), u64::MAX);
    }

    #[test]
    fn records_decode_every_serial_type() {
        // Header: its own length, then NULL, i8, i16, i24, i48, f64, 0, 1,
        // "hi" (text of 2: 13 + 2*2), a 1-byte blob (12 + 2*1).
        let mut payload = vec![11, 0, 1, 2, 3, 5, 7, 8, 9, 17, 14];
        payload.extend_from_slice(&[0xff]); // -1
        payload.extend_from_slice(&300i16.to_be_bytes());
        payload.extend_from_slice(&[0xff, 0xff, 0xfe]); // -2
        payload.extend_from_slice(&(1i64 << 40).to_be_bytes()[2..]);
        payload.extend_from_slice(&1.5f64.to_be_bytes());
        payload.extend_from_slice(b"hi");
        payload.push(0xab);
        let mut values = Vec::new();
        decode_record(&payload, &mut values).unwrap();
        assert_eq!(
            values,
            [
                Value::Null,
                Value::Integer(-1),
                Value::Integer(300),
                Value::Integer(-2),
                Value::Integer(1 << 40),
                Value::Real(1.5),
                Value::Integer(0),
                Value::Integer(1),
                Value::Text("hi"),
                Value::Blob(&[0xab]),
            ]
        );
    }

    #[test]
    fn column_names_come_out_of_the_schema() {
        // The example log's own definition, whitespace and all.
        let sql = "CREATE TABLE sensor_data\n    (\n        timestamp\n        REAL,\n        sensor_name\n        TEXT,\n        value\n        REAL\n    )";
        assert_eq!(parse_columns(sql).unwrap(), (vec!["timestamp".into(), "sensor_name".into(), "value".into()], None));

        let sql = r#"CREATE TABLE t (id INTEGER PRIMARY KEY, "odd, name" DECIMAL(10, 2), [x] TEXT CHECK (x IN ('a,b')), PRIMARY KEY (id))"#;
        assert_eq!(parse_columns(sql).unwrap(), (vec!["id".into(), "odd, name".into(), "x".into()], Some(0)));

        assert!(parse_columns("CREATE TABLE t (a, b) WITHOUT ROWID").is_err());
    }

    #[test]
    fn garbage_is_refused_not_panicked_on() {
        assert!(Database::open(b"hello").is_err());
        let mut header = vec![0u8; 4096];
        header[..16].copy_from_slice(MAGIC);
        header[16..18].copy_from_slice(&4096u16.to_be_bytes());
        // Page 1 claims to be an interior page pointing at itself.
        header[100] = INTERIOR_TABLE;
        header[108..112].copy_from_slice(&1u32.to_be_bytes());
        let db = Database::open(&header).unwrap();
        assert!(db.table("sensor_data").is_err());
    }
}

/// Against the real thing: databases written by SQLite itself, read back by
/// both `rusqlite` and this reader.
#[cfg(all(test, feature = "sqlite"))]
mod against_sqlite {
    use rusqlite::Connection;

    use super::*;

    fn write(setup: impl FnOnce(&Connection)) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!(
            "rapid-analyzer-sqlite-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.sqlite");
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            setup(&conn);
        }
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        bytes
    }

    fn rows(bytes: &[u8], table: &str) -> Vec<Vec<String>> {
        let db = Database::open(bytes).unwrap();
        let table = db.table(table).unwrap();
        let mut out = Vec::new();
        db.for_each_row(&table, |row| {
            out.push(row.iter().map(|v| format!("{v:?}")).collect());
            Ok(())
        })
        .unwrap();
        out
    }

    /// Enough rows for a b-tree several levels deep, with the whole-number
    /// REALs SQLite stores as integers.
    #[test]
    fn a_deep_table_reads_back_in_order() {
        let n = 20_000;
        let bytes = write(|conn| {
            conn.execute_batch(
                "PRAGMA page_size = 1024;
                 CREATE TABLE sensor_data (timestamp REAL, sensor_name TEXT, value REAL);",
            )
            .unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            let mut insert = tx.prepare("INSERT INTO sensor_data VALUES (?1, ?2, ?3)").unwrap();
            for i in 0..n {
                let value = if i % 3 == 0 { f64::from(i) } else { f64::from(i) + 0.25 };
                insert.execute((1.78e9 + f64::from(i) * 0.01, format!("sensor{}", i % 7), value)).unwrap();
            }
            drop(insert);
            tx.commit().unwrap();
        });

        let db = Database::open(&bytes).unwrap();
        let table = db.table("SENSOR_DATA").unwrap();
        assert_eq!(table.columns, ["timestamp", "sensor_name", "value"]);
        let mut i = 0;
        db.for_each_row(&table, |row| {
            assert_eq!(row[0].as_f64(), Some(1.78e9 + f64::from(i) * 0.01));
            assert_eq!(row[1].as_str(), Some(format!("sensor{}", i % 7).as_str()));
            let value = if i % 3 == 0 { f64::from(i) } else { f64::from(i) + 0.25 };
            assert_eq!(row[2].as_f64(), Some(value));
            i += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(i, n);
    }

    /// Rows bigger than a page continue on overflow pages.
    #[test]
    fn long_rows_follow_their_overflow_chain() {
        let long = |i: usize| format!("{i}:").repeat(700 + 97 * i);
        let bytes = write(|conn| {
            conn.execute_batch("PRAGMA page_size = 512; CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, data BLOB);")
                .unwrap();
            for i in 0..12 {
                conn.execute("INSERT INTO t (name, data) VALUES (?1, ?2)", (long(i), vec![i as u8; 1000 + i]))
                    .unwrap();
            }
        });
        let db = Database::open(&bytes).unwrap();
        let table = db.table("t").unwrap();
        let mut i = 0;
        db.for_each_row(&table, |row| {
            // The rowid alias is filled in from the rowid.
            assert_eq!(row[0], Value::Integer(i as i64 + 1));
            assert_eq!(row[1].as_str(), Some(long(i).as_str()));
            assert_eq!(row[2], Value::Blob(&vec![i as u8; 1000 + i]));
            i += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(i, 12);
    }

    /// A column added after rows were written: those rows are short.
    #[test]
    fn old_rows_are_padded_to_new_columns() {
        let bytes = write(|conn| {
            conn.execute_batch(
                "CREATE TABLE t (a INTEGER);
                 INSERT INTO t VALUES (1);
                 ALTER TABLE t ADD COLUMN b TEXT;
                 INSERT INTO t VALUES (2, 'two');",
            )
            .unwrap();
        });
        assert_eq!(
            rows(&bytes, "t"),
            [vec!["Integer(1)", "Null"], vec!["Integer(2)", "Text(\"two\")"]]
        );
    }

    /// The format the logger actually writes: WAL mode, with an index next
    /// to the table (which the reader has to step over in the schema).
    #[test]
    fn a_wal_mode_database_with_an_index() {
        let bytes = write(|conn| {
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE sensor_data (timestamp REAL, sensor_name TEXT, value REAL);
                 CREATE INDEX idx_ts ON sensor_data (timestamp);
                 INSERT INTO sensor_data VALUES (1.5, 'p', 2.0), (2.5, 'p', -3.25);",
            )
            .unwrap();
        });
        assert_eq!(
            rows(&bytes, "sensor_data"),
            [
                vec!["Real(1.5)", "Text(\"p\")", "Integer(2)"],
                vec!["Real(2.5)", "Text(\"p\")", "Real(-3.25)"]
            ]
        );
    }
}
