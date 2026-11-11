//! # `sql` — a small, synchronous, runtime-free façade over Turso.
//!
//! Turso's native Rust API is `async` (it yields while performing I/O), but
//! `kb` is a synchronous CLI. Rather than pull in an async runtime (tokio),
//! this module drives each Turso future to completion with
//! [`futures::executor::block_on`] — no executor thread, no reactor, no
//! tokio. The public surface deliberately mirrors the handful of
//! `rusqlite` APIs the rest of the tree already uses (`Connection`,
//! `Statement`, `Row`, `params!`, `OptionalExtension`, …), so swapping the
//! engine is mostly an import change at each call site.
//!
//! FTS note: Turso does **not** implement SQLite's FTS5. It has its own
//! Tantivy-backed index (`CREATE INDEX … USING fts`, `fts_match`,
//! `fts_score`, `fts_highlight`). The schema/queries in this crate are
//! written against that engine.

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

pub use turso::Value;

pub type Result<T> = std::result::Result<T, Error>;

/// Error type shaped like the `rusqlite` error surface this codebase uses.
#[derive(Debug)]
pub enum Error {
    /// Any error surfaced by Turso itself.
    Turso(turso::Error),
    /// `query_row` found no row.
    QueryReturnedNoRows,
    /// A filesystem path could not be used.
    InvalidPath(PathBuf),
    /// Compatibility variant for the few call sites that synthesize an
    /// engine failure (e.g. mapping an `IndexError` into a SQL error).
    SqliteFailure(ffi::Error, Option<String>),
    /// A column value did not match the requested Rust type.
    ConversionFailure(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Turso(e) => write!(f, "{e}"),
            Error::QueryReturnedNoRows => f.write_str("Query returned no rows"),
            Error::InvalidPath(p) => write!(f, "Invalid path: {}", p.display()),
            Error::SqliteFailure(code, msg) => match msg {
                Some(msg) => write!(f, "SQL error (code {}): {msg}", code.code),
                None => write!(f, "SQL error (code {})", code.code),
            },
            Error::ConversionFailure(msg) => write!(f, "Conversion failure: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Turso(e) => Some(e),
            _ => None,
        }
    }
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Error::QueryReturnedNoRows, Error::QueryReturnedNoRows) => true,
            (Error::InvalidPath(a), Error::InvalidPath(b)) => a == b,
            (Error::SqliteFailure(a, am), Error::SqliteFailure(b, bm)) => {
                a.code == b.code && am == bm
            }
            (Error::ConversionFailure(a), Error::ConversionFailure(b)) => a == b,
            (Error::Turso(a), Error::Turso(b)) => a.to_string() == b.to_string(),
            _ => false,
        }
    }
}

impl From<turso::Error> for Error {
    fn from(value: turso::Error) -> Self {
        Error::Turso(value)
    }
}

/// Minimal stand-in for `rusqlite::ffi::Error`.
pub mod ffi {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Error {
        pub code: i32,
    }

    impl Error {
        pub fn new(code: i32) -> Self {
            Self { code }
        }
    }
}

// ---------------------------------------------------------------------------
// Parameters: converting Rust values into Turso `Value`s.
// ---------------------------------------------------------------------------

/// A value that can be bound as a positional SQL parameter.
pub trait ToSql {
    fn to_sql(&self) -> Value;
}

impl ToSql for Value {
    fn to_sql(&self) -> Value {
        self.clone()
    }
}

macro_rules! to_sql_int {
    ($($t:ty),* $(,)?) => {$(
        impl ToSql for $t {
            fn to_sql(&self) -> Value { Value::Integer(*self as i64) }
        }
    )*};
}
to_sql_int!(i8, i16, i32, i64, u8, u16, u32);

impl ToSql for usize {
    fn to_sql(&self) -> Value {
        Value::Integer(*self as i64)
    }
}

impl ToSql for bool {
    fn to_sql(&self) -> Value {
        Value::Integer(*self as i64)
    }
}

impl ToSql for f32 {
    fn to_sql(&self) -> Value {
        Value::Real(*self as f64)
    }
}

impl ToSql for f64 {
    fn to_sql(&self) -> Value {
        Value::Real(*self)
    }
}

impl ToSql for &str {
    fn to_sql(&self) -> Value {
        Value::Text((*self).to_owned())
    }
}

impl ToSql for String {
    fn to_sql(&self) -> Value {
        Value::Text(self.clone())
    }
}

impl ToSql for &String {
    fn to_sql(&self) -> Value {
        Value::Text((*self).clone())
    }
}

impl ToSql for &[u8] {
    fn to_sql(&self) -> Value {
        Value::Blob(self.to_vec())
    }
}

impl ToSql for Vec<u8> {
    fn to_sql(&self) -> Value {
        Value::Blob(self.clone())
    }
}

impl<T: ToSql> ToSql for Option<T> {
    fn to_sql(&self) -> Value {
        match self {
            Some(v) => v.to_sql(),
            None => Value::Null,
        }
    }
}

/// What can be passed as a statement's parameter list.
pub trait IntoParams {
    fn into_values(self) -> Vec<Value>;
}

impl IntoParams for () {
    fn into_values(self) -> Vec<Value> {
        Vec::new()
    }
}

// `[]` at a call site infers this unique zero-length array type, so the
// existing `query_row(sql, (), …)` style keeps working unchanged.
impl IntoParams for [Value; 0] {
    fn into_values(self) -> Vec<Value> {
        Vec::new()
    }
}

macro_rules! array_into_params {
    ($($n:literal),* $(,)?) => {$(
        impl<T: ToSql> IntoParams for [T; $n] {
            fn into_values(self) -> Vec<Value> {
                self.into_iter().map(|v| v.to_sql()).collect()
            }
        }
    )*};
}
array_into_params!(1, 2, 3, 4, 5, 6, 7, 8);

impl<T: ToSql> IntoParams for Vec<T> {
    fn into_values(self) -> Vec<Value> {
        self.into_iter().map(|v| v.to_sql()).collect()
    }
}

/// Collect an iterator of values into a positional parameter list.
pub fn params_from_iter<I>(iter: I) -> Vec<Value>
where
    I: IntoIterator,
    I::Item: ToSql,
{
    iter.into_iter().map(|v| v.to_sql()).collect()
}

/// Positional parameter list macro, mirroring `rusqlite::params!`.
#[macro_export]
macro_rules! sql_params {
    () => { ::std::vec::Vec::<$crate::sql::Value>::new() };
    ($($value:expr),+ $(,)?) => {
        {
            use $crate::sql::ToSql as _;
            ::std::vec![$($value.to_sql()),+]
        }
    };
}

pub use crate::sql_params as params;

// Tuples up to 16 elements, matching `rusqlite`/`turso`.
macro_rules! tuple_into_params {
    ($($idx:tt $ty:ident),+ $(,)?) => {
        impl<$($ty: ToSql,)+> IntoParams for ($($ty,)+) {
            fn into_values(self) -> Vec<Value> {
                ::std::vec![$(self.$idx.to_sql()),+]
            }
        }
    };
}

tuple_into_params!(0 A);
tuple_into_params!(0 A, 1 B);
tuple_into_params!(0 A, 1 B, 2 C);
tuple_into_params!(0 A, 1 B, 2 C, 3 D);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J, 10 K);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J, 10 K, 11 L);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J, 10 K, 11 L, 12 M);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J, 10 K, 11 L, 12 M, 13 N);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J, 10 K, 11 L, 12 M, 13 N, 14 O);
tuple_into_params!(0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J, 10 K, 11 L, 12 M, 13 N, 14 O, 15 P);

// ---------------------------------------------------------------------------
// Reading columns: converting Turso `Value`s into Rust types.
// ---------------------------------------------------------------------------

/// A Rust type that can be read from a result column.
pub trait FromSql: Sized {
    fn from_sql(value: Value) -> Result<Self>;
}

impl FromSql for Value {
    fn from_sql(value: Value) -> Result<Self> {
        Ok(value)
    }
}

impl FromSql for String {
    fn from_sql(value: Value) -> Result<Self> {
        match value {
            Value::Text(s) => Ok(s),
            other => Err(Error::ConversionFailure(format!(
                "expected TEXT, got {other:?}"
            ))),
        }
    }
}

impl FromSql for i64 {
    fn from_sql(value: Value) -> Result<Self> {
        match value {
            Value::Integer(i) => Ok(i),
            Value::Real(r) => Ok(r as i64),
            other => Err(Error::ConversionFailure(format!(
                "expected INTEGER, got {other:?}"
            ))),
        }
    }
}

impl FromSql for i32 {
    fn from_sql(value: Value) -> Result<Self> {
        Ok(i64::from_sql(value)? as i32)
    }
}

impl FromSql for f64 {
    fn from_sql(value: Value) -> Result<Self> {
        match value {
            Value::Real(r) => Ok(r),
            Value::Integer(i) => Ok(i as f64),
            other => Err(Error::ConversionFailure(format!(
                "expected REAL, got {other:?}"
            ))),
        }
    }
}

impl FromSql for bool {
    fn from_sql(value: Value) -> Result<Self> {
        Ok(i64::from_sql(value)? != 0)
    }
}

impl FromSql for Vec<u8> {
    fn from_sql(value: Value) -> Result<Self> {
        match value {
            Value::Blob(b) => Ok(b),
            other => Err(Error::ConversionFailure(format!(
                "expected BLOB, got {other:?}"
            ))),
        }
    }
}

impl<T: FromSql> FromSql for Option<T> {
    fn from_sql(value: Value) -> Result<Self> {
        match value {
            Value::Null => Ok(None),
            other => Ok(Some(T::from_sql(other)?)),
        }
    }
}

/// Column index accepted by [`Row::get`]: `usize`, integer literals, or a
/// column name.
pub trait RowIndex {
    fn index(self) -> RowRef;
}

pub enum RowRef {
    Pos(usize),
    Name(Cow<'static, str>),
}

impl RowIndex for usize {
    fn index(self) -> RowRef {
        RowRef::Pos(self)
    }
}

impl RowIndex for i32 {
    fn index(self) -> RowRef {
        RowRef::Pos(self as usize)
    }
}

impl RowIndex for i64 {
    fn index(self) -> RowRef {
        RowRef::Pos(self as usize)
    }
}

impl RowIndex for &str {
    fn index(self) -> RowRef {
        RowRef::Name(Cow::Owned(self.to_owned()))
    }
}

pub struct Row {
    inner: turso::Row,
    column_names: Vec<String>,
}

impl Row {
    pub fn get<I: RowIndex, T: FromSql>(&self, idx: I) -> Result<T> {
        let pos = match idx.index() {
            RowRef::Pos(p) => p,
            RowRef::Name(name) => self
                .column_names
                .iter()
                .position(|c| c == name.as_ref())
                .ok_or_else(|| Error::ConversionFailure(format!("no column named {name}")))?,
        };
        T::from_sql(self.inner.get_value(pos)?)
    }
}

// ---------------------------------------------------------------------------
// Statements / rows / connection / transaction.
// ---------------------------------------------------------------------------

fn run_query(conn: &turso::Connection, sql: &str, values: Vec<Value>) -> Result<turso::Rows> {
    let rows = futures::executor::block_on(conn.query(sql, turso::params_from_iter(values)))?;
    Ok(rows)
}

fn run_execute(conn: &turso::Connection, sql: &str, values: Vec<Value>) -> Result<usize> {
    let n = futures::executor::block_on(conn.execute(sql, turso::params_from_iter(values)))?;
    Ok(n as usize)
}

fn prepare_on(conn: &turso::Connection, sql: &str, cached: bool) -> Result<Statement> {
    let inner = if cached {
        futures::executor::block_on(conn.prepare_cached(sql))?
    } else {
        futures::executor::block_on(conn.prepare(sql))?
    };
    Ok(Statement { inner })
}

fn query_row_on<P, F, T>(conn: &turso::Connection, sql: &str, params: P, f: F) -> Result<T>
where
    P: IntoParams,
    F: FnOnce(&Row) -> Result<T>,
{
    let mut rows = run_query(conn, sql, params.into_values())?;
    match futures::executor::block_on(rows.next())? {
        Some(row) => {
            let names = rows.column_names();
            let row = Row {
                inner: row,
                column_names: names,
            };
            f(&row)
        }
        None => Err(Error::QueryReturnedNoRows),
    }
}

pub struct Statement {
    inner: turso::Statement,
}

impl Statement {
    /// Run the statement and collect the mapped rows eagerly. Turso's row
    /// iterator is `async` and borrowed, so the synchronous façade buffers
    /// the (typically small) result set into a plain iterator.
    pub fn query_map<P, F, T>(&self, params: P, mut f: F) -> Result<MappedRows<T>>
    where
        P: IntoParams,
        F: FnMut(&Row) -> Result<T>,
    {
        let mut stmt = self.inner.clone();
        let mut rows =
            futures::executor::block_on(stmt.query(turso::params_from_iter(params.into_values())))?;
        let names = rows.column_names();
        let mut out = Vec::new();
        while let Some(row) = futures::executor::block_on(rows.next())? {
            let row = Row {
                inner: row,
                column_names: names.clone(),
            };
            out.push(f(&row));
        }
        Ok(MappedRows {
            iter: out.into_iter(),
        })
    }

    pub fn query_row<P, F, T>(&self, params: P, f: F) -> Result<T>
    where
        P: IntoParams,
        F: FnOnce(&Row) -> Result<T>,
    {
        let mut stmt = self.inner.clone();
        let mut rows =
            futures::executor::block_on(stmt.query(turso::params_from_iter(params.into_values())))?;
        let names = rows.column_names();
        match futures::executor::block_on(rows.next())? {
            Some(row) => f(&Row {
                inner: row,
                column_names: names,
            }),
            None => Err(Error::QueryReturnedNoRows),
        }
    }

    pub fn execute<P: IntoParams>(&self, params: P) -> Result<usize> {
        let mut stmt = self.inner.clone();
        let n = futures::executor::block_on(
            stmt.execute(turso::params_from_iter(params.into_values())),
        )?;
        Ok(n as usize)
    }
}

pub struct MappedRows<T> {
    iter: std::vec::IntoIter<Result<T>>,
}

impl<T> Iterator for MappedRows<T> {
    type Item = Result<T>;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

pub struct Connection {
    // Keep the `Database` alive for as long as any connection derived from
    // it is around.
    _db: turso::Database,
    inner: turso::Connection,
}

impl Connection {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_str(&path.to_string_lossy())
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::open_str(":memory:")
    }

    fn open_str(path: &str) -> Result<Self> {
        let mut builder = turso::Builder::new_local(path)
            // Turso's native FTS lives behind "index methods".
            .experimental_index_method(true);
        // Several call sites open a second connection to the same file
        // in-process (WAL / concurrency probes). The flag is rejected for
        // in-memory databases, so only enable it for on-disk files.
        if path != ":memory:" {
            builder = builder.experimental_multiprocess_wal(true);
        }
        let db = futures::executor::block_on(builder.build())?;
        let inner = db.connect()?;
        Ok(Self { _db: db, inner })
    }

    pub fn execute<P: IntoParams>(&self, sql: impl AsRef<str>, params: P) -> Result<usize> {
        run_execute(&self.inner, sql.as_ref(), params.into_values())
    }

    pub fn execute_batch(&self, sql: impl AsRef<str>) -> Result<()> {
        futures::executor::block_on(self.inner.execute_batch(sql.as_ref()))?;
        Ok(())
    }

    pub fn query_row<P, F, T>(&self, sql: impl AsRef<str>, params: P, f: F) -> Result<T>
    where
        P: IntoParams,
        F: FnOnce(&Row) -> Result<T>,
    {
        query_row_on(&self.inner, sql.as_ref(), params, f)
    }

    pub fn prepare(&self, sql: impl AsRef<str>) -> Result<Statement> {
        prepare_on(&self.inner, sql.as_ref(), false)
    }

    pub fn prepare_cached(&self, sql: impl AsRef<str>) -> Result<Statement> {
        prepare_on(&self.inner, sql.as_ref(), true)
    }

    pub fn pragma_update(
        &self,
        _schema: Option<&str>,
        name: &str,
        value: impl fmt::Display,
    ) -> Result<()> {
        futures::executor::block_on(self.inner.pragma_update(name, value))?;
        Ok(())
    }

    pub fn last_insert_rowid(&self) -> i64 {
        self.inner.last_insert_rowid()
    }

    pub fn unchecked_transaction(&self) -> Result<Transaction<'_>> {
        let inner = futures::executor::block_on(self.inner.unchecked_transaction())?;
        Ok(Transaction { inner })
    }

    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        let inner = futures::executor::block_on(self.inner.transaction())?;
        Ok(Transaction { inner })
    }
}

pub struct Transaction<'a> {
    inner: turso::transaction::Transaction<'a>,
}

impl<'a> Transaction<'a> {
    fn conn(&self) -> &turso::Connection {
        &self.inner
    }

    pub fn execute<P: IntoParams>(&self, sql: impl AsRef<str>, params: P) -> Result<usize> {
        run_execute(self.conn(), sql.as_ref(), params.into_values())
    }

    pub fn prepare(&self, sql: impl AsRef<str>) -> Result<Statement> {
        prepare_on(self.conn(), sql.as_ref(), false)
    }

    pub fn prepare_cached(&self, sql: impl AsRef<str>) -> Result<Statement> {
        prepare_on(self.conn(), sql.as_ref(), true)
    }

    pub fn query_row<P, F, T>(&self, sql: impl AsRef<str>, params: P, f: F) -> Result<T>
    where
        P: IntoParams,
        F: FnOnce(&Row) -> Result<T>,
    {
        query_row_on(self.conn(), sql.as_ref(), params, f)
    }

    pub fn commit(self) -> Result<()> {
        futures::executor::block_on(self.inner.commit())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Optional extension (`.optional()`), matching rusqlite's helper.
// ---------------------------------------------------------------------------

pub trait OptionalExtension<T> {
    fn optional(self) -> Result<Option<T>>;
}

impl<T> OptionalExtension<T> for Result<T> {
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
