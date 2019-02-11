use lazy_static::lazy_static;
use log::{debug, log_enabled, trace};
pub use odbc;
use odbc::{
    ColumnDescriptor, Connection, DriverInfo, Environment, NoResult, OdbcType, Allocated, Prepared,
    ResultSetState, SqlDate, SqlSsTime2, SqlTime, SqlTimestamp, Statement, Version3, DiagnosticRecord
};
use regex::Regex;
pub use serde_json::value::Value;
use std::cell::{Ref, RefCell};
use std::fmt::Debug;
use std::marker::PhantomData;
use error_context::prelude::*;
use std::fmt;
use std::error::Error;
use std::string::FromUtf16Error;

/// TODO
/// * Use custom Value type but provide From traits for JSON behind feature
/// * Make tests somehow runable?
/// * Provide affected_row_count()
/// * Provide tables()
/// * Prepared statment .schema()/.num_result_cold()
/// * Prepared statement cache:
/// ** db.with_statment_cache() -> StatmentCache
/// ** sc.query(str) - direct query
/// ** sc.query_prepared(impl ToString + Hash) - hash fist and look up in cache if found execute; .to_string otherwise and prepre + execute; 
///    this is to avoid building query strings where we know hash e.g. from some other value than query string itself
/// ** sc.clear() - try close the statments and clear the cache
/// * Replace unit errors with never type when stable

// https://github.com/rust-lang/rust/issues/49431
pub trait Captures<'a> {}
impl<'a, T: ?Sized> Captures<'a> for T {}

pub trait Captures2<'a> {}
impl<'a, T: ?Sized> Captures2<'a> for T {}

#[derive(Debug)]
pub enum OdbcIterError {
    OdbcError(Option<DiagnosticRecord>, &'static str),
    NotConnectedError,
}

impl fmt::Display for OdbcIterError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            OdbcIterError::OdbcError(_, context) => write!(f, "ODBC call failed while {}", context),
            OdbcIterError::NotConnectedError => write!(f, "not connected to database"),
        }
    }
}

fn to_dyn(diag: &Option<DiagnosticRecord>) -> Option<&(dyn Error + 'static)> {
    diag.as_ref().map(|e| e as &(dyn Error + 'static))
}

impl Error for OdbcIterError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            OdbcIterError::OdbcError(diag, _) => to_dyn(diag),
            OdbcIterError::NotConnectedError => None,
        }
    }  
}

impl From<ErrorContext<Option<DiagnosticRecord>, &'static str>> for OdbcIterError {
    fn from(err: ErrorContext<Option<DiagnosticRecord>, &'static str>) -> OdbcIterError {
        OdbcIterError::OdbcError(err.error, err.context)
    }
}

impl From<ErrorContext<DiagnosticRecord, &'static str>> for OdbcIterError {
    fn from(err: ErrorContext<DiagnosticRecord, &'static str>) -> OdbcIterError {
        OdbcIterError::OdbcError(Some(err.error), err.context)
    }
}

//TODO: remove OdbcIter prefix
#[derive(Debug)]
pub enum OdbcIterQueryError<R, S> {
    MultipleQueriesError(SplitQueriesError),
    FromRowError(R),
    FromSchemaError(S),
    OdbcError(DiagnosticRecord, &'static str),
    DataAccessError(DataAccessError, &'static str),
}

impl<R, S> fmt::Display for OdbcIterQueryError<R, S> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            OdbcIterQueryError::MultipleQueriesError(_) => write!(f, "failed to execute multiple queries"),
            OdbcIterQueryError::FromRowError(_) => write!(f, "failed to convert table row to target type"),
            OdbcIterQueryError::FromSchemaError(_) => write!(f, "failed to convert table schema to target type"),
            OdbcIterQueryError::OdbcError(_, context) => write!(f, "ODBC call failed while {}", context),
            OdbcIterQueryError::DataAccessError(_, context) => write!(f, "failed to access result data while {}", context),
        }
    }
}

impl<R, S> Error for OdbcIterQueryError<R, S> where R: Error + 'static, S: Error + 'static {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            OdbcIterQueryError::MultipleQueriesError(err) => Some(err),
            OdbcIterQueryError::FromRowError(err) => Some(err),
            OdbcIterQueryError::FromSchemaError(err) => Some(err),
            OdbcIterQueryError::OdbcError(err, _) => Some(err),
            OdbcIterQueryError::DataAccessError(err, _) => Some(err),
        }
    }  
}

impl<R, S> From<SplitQueriesError> for OdbcIterQueryError<R, S> {
    fn from(err: SplitQueriesError) -> OdbcIterQueryError<R, S> {
        OdbcIterQueryError::MultipleQueriesError(err)
    }
}

impl<R, S> From<ErrorContext<DiagnosticRecord, &'static str>> for OdbcIterQueryError<R, S> {
    fn from(err: ErrorContext<DiagnosticRecord, &'static str>) -> OdbcIterQueryError<R, S> {
        OdbcIterQueryError::OdbcError(err.error, err.context)
    }
}

impl<R, S> From<ErrorContext<DataAccessError, &'static str>> for OdbcIterQueryError<R, S> {
    fn from(err: ErrorContext<DataAccessError, &'static str>) -> OdbcIterQueryError<R, S> {
        OdbcIterQueryError::DataAccessError(err.error, err.context)
    }
}

#[derive(Debug)]
pub enum DataAccessError {
    OdbcCursorError(DiagnosticRecord),
    FromUtf16Error(FromUtf16Error, &'static str),
}

impl fmt::Display for DataAccessError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DataAccessError::OdbcCursorError(_) => write!(f, "failed to access data in ODBC cursor"),
            DataAccessError::FromUtf16Error(_, context) => write!(f, "failed to create String from UTF-16 column data while {}", context),
        }
    }
}

impl WithContext<&'static str> for DataAccessError {
    type ContextError = ErrorContext<DataAccessError, &'static str>;
    fn with_context(self, context: &'static str) -> ErrorContext<DataAccessError, &'static str> {
        ErrorContext {
            error: self, 
            context 
        }
    }
}

impl Error for DataAccessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            DataAccessError::OdbcCursorError(err) => Some(err),
            DataAccessError::FromUtf16Error(err, _) => Some(err),
        }
    }
}

impl From<DiagnosticRecord> for DataAccessError {
    fn from(err: DiagnosticRecord) -> DataAccessError {
        DataAccessError::OdbcCursorError(err)
    }
}

impl From<ErrorContext<FromUtf16Error, &'static str>> for DataAccessError {
    fn from(err: ErrorContext<FromUtf16Error, &'static str>) -> DataAccessError {
        DataAccessError::FromUtf16Error(err.error, err.context)
    }
}

pub type EnvironmentV3 = Environment<Version3>;
pub type Values = Vec<Value>;
pub type Schema = Vec<ColumnDescriptor>;

// TODO: move SchemaAccess to submodule
// pub struct SchemaAccess<'v> {
//     value: Vec<Value>,
//     schema: &'v Schema,
// }

// pub trait WithSchemaAccess {
//     fn with_schema_access<'i>(self, schema: &'i Schema) -> SchemaAccess<'i>;
// }

// impl WithSchemaAccess for Values {
//     fn with_schema_access<'i>(self, schema: &'i Schema) -> SchemaAccess<'i> {
//         SchemaAccess {
//             value: self,
//             schema,
//         }
//     }
// }

// pub trait SchemaIndex {
//     fn column_index(self, name: &str) -> Result<usize, Problem>;
// }

// impl<'i> SchemaIndex for &'i Schema {
//     fn column_index(self, name: &str) -> Result<usize, Problem> {
//         self.iter()
//             .position(|desc| desc.name == name)
//             .ok_or_problem("column not found")
//             .problem_while_with(|| {
//                 format!("accessing column {} in data with schema: {:?}", name, self)
//             })
//     }
// }

// impl<'i> SchemaAccess<'i> {
//     pub fn get(&self, column_name: &str) -> Result<&Value, Problem> {
//         let index = self.schema.column_index(column_name)?;
//         Ok(self
//             .value
//             .get(index)
//             .expect("index out of range while getting value by column name"))
//     }

//     pub fn take(&mut self, column_name: &str) -> Result<Value, Problem> {
//         let index = self.schema.column_index(column_name)?;
//         Ok(self
//             .value
//             .get_mut(index)
//             .expect("index out of range while taking value by column name")
//             .take())
//     }
// }

/// Convert from ODBC schema to other type of schema
pub trait TryFromSchema: Sized {
    type Error;
    fn try_from_schema(schema: &Schema) -> Result<Self, Self::Error>;
}

impl TryFromSchema for () {
    type Error = ();
    fn try_from_schema(_schema: &Schema) -> Result<Self, Self::Error> {
        Ok(())
    }
}

impl TryFromSchema for Schema {
    type Error = ();
    fn try_from_schema(schema: &Schema) -> Result<Self, Self::Error> {
        Ok(schema.clone())
    }
}

/// Convert from ODBC row to other type of value
pub trait TryFromRow: Sized {
    /// Type of shema for the target value
    type Schema: TryFromSchema;
    type Error;
    fn try_from_row(values: Values, schema: &Self::Schema) -> Result<Self, Self::Error>;
}

impl TryFromRow for Values {
    type Schema = Schema;
    type Error = ();
    fn try_from_row(values: Values, _schema: &Self::Schema) -> Result<Self, Self::Error> {
        Ok(values)
    }
}

impl TryFromRow for Value {
    type Schema = Schema;
    type Error = ();
    fn try_from_row(values: Values, _schema: &Self::Schema) -> Result<Self, Self::Error> {
        Ok(values.into())
    }
}

/// Iterate rows converting them to given value type
pub struct RowIter<'odbc, V, S>
where
    V: TryFromRow
{
    statement: Option<odbc::Statement<'odbc, 'odbc, S, odbc::HasResult>>,
    no_results_statement: Option<odbc::Statement<'odbc, 'odbc, S, odbc::NoResult>>,
    odbc_schema: Vec<ColumnDescriptor>,
    schema: V::Schema,
    phantom: PhantomData<V>,
    utf_16_strings: bool,
}

impl<'odbc, V, S> RowIter<'odbc, V, S>
where
    V: TryFromRow,
{
    fn from_result<'t>(result: ResultSetState<'odbc, 't, S>, utf_16_strings: bool) -> Result<RowIter<'odbc, V, S>, OdbcIterQueryError<V::Error, <<V as TryFromRow>::Schema as TryFromSchema>::Error>> {
        let (odbc_schema, statement, no_results_statement) = match result {
            ResultSetState::Data(statement) => {
                let num_cols = statement.num_result_cols().wrap_error_while("getting number of result columns")?;
                let odbc_schema = (1..num_cols + 1)
                        .map(|i| statement.describe_col(i as u16))
                        .collect::<Result<Vec<ColumnDescriptor>, _>>().wrap_error_while("getting column descriptiors")?;
                let statement = statement.reset_parameters().wrap_error_while("reseting bound parameters on statement")?; // don't refrence parameter data any more

                if log_enabled!(::log::Level::Debug) {
                    if odbc_schema.len() == 0 {
                        debug!("Got empty data set");
                    } else {
                        debug!("Got data with columns: {}", odbc_schema.iter().map(|cd| cd.name.clone()).collect::<Vec<String>>().join(", "));
                    }
                }

                if num_cols == 0 {
                    // Invalid cursor state.
                    (odbc_schema, None, None)
                } else {
                    (odbc_schema, Some(statement), None)
                }
            }
            ResultSetState::NoData(statement) => {
                debug!("No data");
                let statement = statement.reset_parameters().wrap_error_while("reseting bound parameters on statement")?; // don't refrence parameter data any more
                (Vec::new(), None, Some(statement))
            }
        };

        if log_enabled!(::log::Level::Trace) {
            for cd in &odbc_schema {
                trace!("ODBC query result schema: {} [{:?}] size: {:?} nullable: {:?} decimal_digits: {:?}", cd.name, cd.data_type, cd.column_size, cd.nullable, cd.decimal_digits);
            }
        }

        let schema = V::Schema::try_from_schema(&odbc_schema).map_err(OdbcIterQueryError::FromSchemaError)?;

        Ok(RowIter {
            statement,
            no_results_statement,
            odbc_schema,
            schema,
            phantom: PhantomData,
            utf_16_strings,
        })
    }

    pub fn schema(&self) -> &V::Schema {
        &self.schema
    }
}

impl<'odbc, V> RowIter<'odbc, V, Prepared>
where
    V: TryFromRow,
{
    pub fn close(
        self,
    ) -> Result<PreparedStatement<'odbc>, OdbcIterError> {
        if let Some(statement) = self.statement {
            Ok(PreparedStatement(statement.close_cursor().wrap_error_while("clocing cursor")?))
        } else {
            Ok(PreparedStatement(self.no_results_statement.expect("statment or no_results_statement")))
        }
    }
}

impl<'odbc, V> RowIter<'odbc, V, Allocated>
where
    V: TryFromRow,
{
    pub fn close(
        self,
    ) -> Result<(), OdbcIterError> {
        if let Some(statement) = self.statement {
            statement.close_cursor().wrap_error_while("closing cursor")?;
        }
        Ok(())
    }
}

impl<'odbc, V, S> Iterator for RowIter<'odbc, V, S>
where
    V: TryFromRow,
{
    type Item = Result<V, OdbcIterQueryError<V::Error, <<V as TryFromRow>::Schema as TryFromSchema>::Error>>;

    fn next(&mut self) -> Option<Self::Item> {
        use odbc_sys::SqlDataType::*;

        fn cursor_get_data<'i, S, T: odbc::OdbcType<'i>>(
            cursor: &'i mut odbc::Cursor<S>,
            index: u16,
        ) -> Result<Option<T>, DiagnosticRecord> {
            cursor.get_data::<T>(index + 1)
        }

        fn into_value<T: Into<Value>>(value: Option<T>) -> Value {
            value.map(Into::into).unwrap_or(Value::Null)
        }

        fn cursor_get_value<'i, S, T: odbc::OdbcType<'i> + Into<Value>>(
            cursor: &'i mut odbc::Cursor<S>,
            index: u16,
        ) -> Result<Value, DiagnosticRecord> {
            cursor_get_data::<S, T>(cursor, index).map(into_value)
        }

        if self.statement.is_none() {
            return None;
        }

        let utf_16_strings = self.utf_16_strings;

        match self.statement.as_mut().unwrap().fetch().wrap_error_while("fetching row") {
            Err(err) => Some(Err(err.into())),
            Ok(Some(mut cursor)) => {
                Some(
                    self.odbc_schema
                        .iter()
                        .enumerate()
                        .map(|(index, column_descriptor)| {
                            trace!("Parsing column {}: {:?}", index, column_descriptor);
                            // https://docs.microsoft.com/en-us/sql/odbc/reference/appendixes/c-data-types?view=sql-server-2017
                            in_context_of::<Value, DataAccessError, _, _, _>("getting value from cursor", || {
                                Ok(match column_descriptor.data_type {
                                    SQL_EXT_TINYINT => {
                                        cursor_get_value::<S, i8>(&mut cursor, index as u16)?
                                    }
                                    SQL_SMALLINT => cursor_get_value::<S, i16>(&mut cursor, index as u16)?,
                                    SQL_INTEGER => cursor_get_value::<S, i32>(&mut cursor, index as u16)?,
                                    SQL_EXT_BIGINT => {
                                        cursor_get_value::<S, i64>(&mut cursor, index as u16)?
                                    }
                                    SQL_FLOAT => cursor_get_value::<S, f32>(&mut cursor, index as u16)?,
                                    SQL_REAL => cursor_get_value::<S, f32>(&mut cursor, index as u16)?,
                                    SQL_DOUBLE => cursor_get_value::<S, f64>(&mut cursor, index as u16)?,
                                    SQL_CHAR | SQL_VARCHAR | SQL_EXT_LONGVARCHAR => {
                                        cursor_get_value::<S, String>(&mut cursor, index as u16)?
                                    }
                                    SQL_EXT_WCHAR | SQL_EXT_WVARCHAR | SQL_EXT_WLONGVARCHAR => {
                                        if utf_16_strings {
                                            if let Some(bytes) = cursor_get_data::<S, &[u16]>(&mut cursor, index as u16)? {
                                                Value::String(String::from_utf16(bytes)
                                                    .wrap_error_while("getting UTF-16 string (SQL_EXT_WCHAR | SQL_EXT_WVARCHAR | SQL_EXT_WLONGVARCHAR)")?)
                                            } else {
                                                Value::Null
                                            }
                                        } else {
                                            cursor_get_value::<S, String>(&mut cursor, index as u16)?
                                        }
                                    }
                                    SQL_TIMESTAMP => {
                                        if let Some(timestamp) = cursor_get_data::<S, SqlTimestamp>(&mut cursor, index as u16)? {
                                            trace!("{:?}", timestamp);
                                            Value::String(format!(
                                                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
                                                timestamp.year,
                                                timestamp.month,
                                                timestamp.day,
                                                timestamp.hour,
                                                timestamp.minute,
                                                timestamp.second,
                                                timestamp.fraction / 1_000_000
                                            ))
                                        } else {
                                            Value::Null
                                        }
                                    }
                                    SQL_DATE => {
                                        if let Some(date) = cursor_get_data::<S, SqlDate>(&mut cursor, index as u16)? {
                                            trace!("{:?}", date);
                                            Value::String(format!(
                                                "{:04}-{:02}-{:02}",
                                                date.year, date.month, date.day
                                            ))
                                        } else {
                                            Value::Null
                                        }
                                    },
                                    SQL_TIME => {
                                            if let Some(time) = cursor_get_data::<S, SqlTime>(&mut cursor, index as u16)? {
                                                trace!("{:?}", time);
                                                Value::String(format!(
                                                    "{:02}:{:02}:{:02}",
                                                    time.hour, time.minute, time.second
                                                ))
                                            } else {
                                                Value::Null
                                            }
                                    }
                                    SQL_SS_TIME2 => {
                                        if let Some(time) = cursor_get_data::<S, SqlSsTime2>(&mut cursor, index as u16)? {
                                            trace!("{:?}", time);
                                            Value::String(format!(
                                                "{:02}:{:02}:{:02}.{:07}",
                                                time.hour,
                                                time.minute,
                                                time.second,
                                                time.fraction / 100
                                            ))
                                        } else {
                                            Value::Null
                                        }
                                    }
                                    SQL_EXT_BIT => {
                                        if let Some(byte) = cursor_get_data::<S, u8>(&mut cursor, index as u16)? {
                                            Value::Bool(if byte == 0 { false } else { true })
                                        } else {
                                            Value::Null
                                        }
                                    }
                                    _ => panic!(format!(
                                        "got unimplemented SQL data type: {:?}",
                                        column_descriptor.data_type
                                    )),
                                })
                            }).map_err(Into::into)
                        })
                        .collect::<Result<Vec<Value>, OdbcIterQueryError<_, _>>>(),
                )
            }
            Ok(None) => None,
        }
        .map(|v| v.and_then(|v| TryFromRow::try_from_row(v, &self.schema).map_err(OdbcIterQueryError::FromRowError)))
    }
}

pub struct Binder<'odbc, 't, S> {
    statement: Statement<'odbc, 't, S, NoResult>,
    index: u16,
}

impl<'odbc, 't, S> Binder<'odbc, 't, S> {
    pub fn bind<'new_t, T>(self, value: &'new_t T) -> Result<Binder<'odbc, 'new_t, S>, DiagnosticRecord>
    where
        T: OdbcType<'new_t> + Debug,
        't: 'new_t,
    {
        let index = self.index + 1;
        if log_enabled!(::log::Level::Trace) {
            trace!("Parameter {}: {:?}", index, value);
        }
        let statement = self.statement.bind_parameter(index, value)?;

        Ok(Binder { statement, index })
    }

    fn into_inner(self) -> Statement<'odbc, 't, S, NoResult> {
        self.statement
    }
}

impl<'odbc, 't, S> From<Statement<'odbc, 'odbc, S, NoResult>> for Binder<'odbc, 'odbc, S> {
    fn from(statement: Statement<'odbc, 'odbc, S, NoResult>) -> Binder<'odbc, 'odbc, S> {
        Binder {
            statement,
            index: 0,
        }
    }
}

pub struct Odbc<'env> {
    connection: Connection<'env>,
    utf_16_strings: bool,
}

pub struct Options {
    utf_16_strings: bool,
}

/// Wrapper around ODBC prepared statement
pub struct PreparedStatement<'odbc>(Statement<'odbc, 'odbc, odbc::Prepared, odbc::NoResult>);

impl<'env> Odbc<'env> {
    pub fn env() -> Result<EnvironmentV3, OdbcIterError> {
        odbc::create_environment_v3().wrap_error_while("creating v3 environment").map_err(Into::into)
    }

    pub fn list_drivers(odbc: &mut Environment<Version3>) -> Result<Vec<DriverInfo>, OdbcIterError> {
        odbc.drivers().wrap_error_while("listing drivers").map_err(Into::into)
    }

    pub fn connect(
        env: &'env Environment<Version3>,
        connection_string: &str,
    ) -> Result<Odbc<'env>, OdbcIterError> {
        Self::connect_with_options(
            env,
            connection_string,
            Options {
                utf_16_strings: false,
            },
        )
    }

    pub fn connect_with_options(
        env: &'env Environment<Version3>,
        connection_string: &str,
        options: Options,
    ) -> Result<Odbc<'env>, OdbcIterError> {
        let connection = env
            .connect_with_connection_string(connection_string)
            .wrap_error_while("connecting to database")?;
        Ok(Odbc {
            connection,
            utf_16_strings: options.utf_16_strings,
        })
    }

    pub fn prepare<'odbc>(&'odbc self, query: &str) -> Result<PreparedStatement<'odbc>, OdbcIterError> {
        debug!("Preparing ODBC query: {}", &query);

        let statement = Statement::with_parent(&self.connection)
            .wrap_error_while("pairing statement with connection")?
            .prepare(query)
            .wrap_error_while("preparing query")?;

        Ok(PreparedStatement(statement))
    }

    pub fn query<V>(&self, query: &str) -> Result<RowIter<V, Allocated>, OdbcIterQueryError<V::Error, <<V as TryFromRow>::Schema as TryFromSchema>::Error>>
    where
        V: TryFromRow,
    {
        self.query_with_parameters(query, |b| Ok(b))
    }

    pub fn query_with_parameters<'t, 'odbc: 't, V, F>(
        &'odbc self,
        query: &str,
        bind: F,
    ) -> Result<RowIter<V, Allocated>, OdbcIterQueryError<V::Error, <<V as TryFromRow>::Schema as TryFromSchema>::Error>>
    where
        V: TryFromRow,
        F: FnOnce(Binder<'odbc, 'odbc, Allocated>) -> Result<Binder<'odbc, 't, Allocated>, DiagnosticRecord>,
    {
        debug!("Direct ODBC query: {}", &query);

        let statement = Statement::with_parent(&self.connection)
            .wrap_error_while("pairing statement with connection")?;

        let statement: Statement<'odbc, 't, Allocated, NoResult> = bind(statement.into())
            .wrap_error_while("binding parameter to statement")?
            .into_inner();

        RowIter::from_result(statement.exec_direct(query).wrap_error_while("executing direct statement")?, self.utf_16_strings)
    }

    pub fn execute<'odbc, V>(
        &'odbc self,
        statement: PreparedStatement<'odbc>,
    ) -> Result<RowIter<'odbc, V, Prepared>, OdbcIterQueryError<V::Error, <<V as TryFromRow>::Schema as TryFromSchema>::Error>>
    where
        V: TryFromRow,
    {
        self.execute_with_parameters(statement, |b| Ok(b))
    }

    pub fn execute_with_parameters<'t, 'odbc: 't, V, F>(
        &'odbc self,
        statement: PreparedStatement<'odbc>,
        bind: F,
    ) -> Result<RowIter<V, Prepared>, OdbcIterQueryError<V::Error, <<V as TryFromRow>::Schema as TryFromSchema>::Error>>
    where
        V: TryFromRow,
        F: FnOnce(Binder<'odbc, 'odbc, Prepared>) -> Result<Binder<'odbc, 't, Prepared>, DiagnosticRecord>,
    {
        let statement: Statement<'odbc, 't, Prepared, NoResult> = bind(statement.0.into())
            .wrap_error_while("binding parameter to statement")?
            .into_inner();

        RowIter::from_result(statement.execute().wrap_error_while("executing statement")?, self.utf_16_strings)
    }

    pub fn query_multiple<'odbc, 'q, 't, V>(
        &'odbc self,
        queries: &'q str,
    ) -> impl Iterator<Item = Result<RowIter<V, Allocated>, OdbcIterQueryError<V::Error, <<V as TryFromRow>::Schema as TryFromSchema>::Error>>> + Captures<'t> + Captures<'env>
    where
        'env: 'odbc,
        'env: 't,
        'odbc: 't,
        'q: 't,
        V: TryFromRow,
    {
        split_queries(queries).map(move |query| query.map_err(Into::into).and_then(|query| self.query(query)))
    }
}

#[derive(Debug)]
pub struct SplitQueriesError;

impl fmt::Display for SplitQueriesError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "failed to split queries")
    }
}

impl Error for SplitQueriesError {}

pub fn split_queries(queries: &str) -> impl Iterator<Item = Result<&str, SplitQueriesError>> {
    lazy_static! {
        // https://regex101.com/r/6YTuVG/4
        static ref RE: Regex = Regex::new(r#"(?:[\t \n]|--.*\n|!.*\n)*((?:[^;"']+(?:'(?:[^'\\]*(?:\\.)?)*')?(?:"(?:[^"\\]*(?:\\.)?)*")?)*;) *"#).unwrap();
    }
    RE.captures_iter(queries)
        .map(|c| c.get(1).ok_or(SplitQueriesError))
        .map(|r| r.map(|m| m.as_str()))
}

// Note: odbc-sys stuff is not Sent and therfore we need to create objects per thread
thread_local! {
    // Leaking ODBC handle per thread should be OK...ish assuming a thread pool is used?
    static ODBC: &'static EnvironmentV3 = Box::leak(Box::new(Odbc::env().expect("Failed to initialize ODBC")));
    static DB: RefCell<Result<Odbc<'static>, OdbcIterError>> = RefCell::new(Err(OdbcIterError::NotConnectedError));
}

/// Access to thread local connection
/// Connection will be astablished only once if successful or any time this function is called again after it failed to connect prevously
pub fn thread_local_connection_with<O>(
    connection_string: &str,
    f: impl Fn(Ref<Result<Odbc<'static>, OdbcIterError>>) -> O,
) -> O {
    DB.with(|db| {
        {
            let mut db = db.borrow_mut();
            if db.is_err() {
                let id = std::thread::current().id();
                debug!("[{:?}] Connecting to database: {}", id, &connection_string);

                *db = ODBC.with(|odbc| Odbc::connect(odbc, &connection_string));
            }
        };

        f(db.borrow())
    })
}

#[cfg(test)]
mod query {
    use super::*;
    #[allow(unused_imports)]
    use assert_matches::assert_matches;

    // 600 chars
    #[cfg(any(
        feature = "test-sql-server",
        feature = "test-hive",
        feature = "test-monetdb"
    ))]
    const LONG_STRING: &'static str = "Lórem ipsum dołor sit amet, cońsectetur adipiścing elit. Fusce risus ipsum, ultricies ac odio ut, vestibulum hendrerit leo. Nunc cursus dapibus mattis. Donec quis est arcu. Sed a tortor sit amet erat euismod pulvinar. Etiam eu erat eget turpis semper finibus. Etiam lobortis egestas diam a consequat. Morbi iaculis lorem sed erat iaculis vehicula. Praesent at porttitor eros. Quisque tincidunt congue ornare. Donec sed nulla a ex sollicitudin lacinia. Fusce ut fermentum tellus, id pretium libero. Donec dapibus faucibus sapien at semper. In id felis sollicitudin, luctus doloź sit amet orci aliquam.";

    #[cfg(feature = "test-sql-server")]
    pub fn sql_server_connection_string() -> String {
        std::env::var("SQL_SERVER_ODBC_CONNECTION")
            .expect("SQL_SERVER_ODBC_CONNECTION not set")
    }

    #[cfg(feature = "test-hive")]
    pub fn hive_connection_string() -> String {
        std::env::var("HIVE_ODBC_CONNECTION").expect("HIVE_ODBC_CONNECTION not set")
    }

    #[cfg(feature = "test-monetdb")]
    pub fn monetdb_connection_string() -> String {
        std::env::var("MONETDB_ODBC_CONNECTION").expect("HIVE_ODBC_CONNECTION not set")
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_multiple_rows() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let data = hive
            .query::<Value>("SELECT explode(x) AS n FROM (SELECT array(42, 24) AS x) d;")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(42)));
        assert_matches!(data[1][0], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(24)));
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_multiple_columns() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let data = hive
            .query::<Value>("SELECT 42, 24;")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(42)));
        assert_matches!(data[0][1], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(24)));
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_types_integer() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let data = hive.query::<Value>("SELECT cast(127 AS TINYINT), cast(32767 AS SMALLINT), cast(2147483647 AS INTEGER), cast(9223372036854775807 AS BIGINT);")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(127)));
        assert_matches!(data[0][1], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(32767)));
        assert_matches!(data[0][2], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(2147483647)));
        assert_matches!(data[0][3], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(9223372036854775807)));
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_types_boolean() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let data = hive
            .query::<Value>("SELECT true, false, CAST(NULL AS BOOLEAN)")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_eq!(data[0][0], Value::Bool(true));
        assert_eq!(data[0][1], Value::Bool(false));
        assert_eq!(data[0][2], Value::Null);
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_types_string() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let data = hive
            .query::<Value>("SELECT cast('foo' AS STRING), cast('bar' AS VARCHAR);")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::String(ref string) => assert_eq!(string.as_str(), "foo"));
        assert_matches!(data[0][1], Value::String(ref string) => assert_eq!(string.as_str(), "bar"));
    }

    #[cfg(feature = "test-sql-server")]
    #[test]
    fn test_sql_server_types_string() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive = Odbc::connect(&odbc, sql_server_connection_string().as_str())
            .expect("connect to Hive");
        let data = hive.query::<Value>("SELECT 'foo', cast('bar' AS NVARCHAR), cast('baz' AS TEXT), cast('quix' AS NTEXT);")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::String(ref string) => assert_eq!(string.as_str(), "foo"));
        assert_matches!(data[0][1], Value::String(ref string) => assert_eq!(string.as_str(), "bar"));
        assert_matches!(data[0][2], Value::String(ref string) => assert_eq!(string.as_str(), "baz"));
        assert_matches!(data[0][3], Value::String(ref string) => assert_eq!(string.as_str(), "quix"));
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_types_float() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let data = hive
            .query::<Value>("SELECT cast(1.5 AS FLOAT), cast(2.5 AS DOUBLE);")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::Number(ref number) => assert!(number.as_f64().unwrap() > 1.0 && number.as_f64().unwrap() < 2.0));
        assert_matches!(data[0][1], Value::Number(ref number) => assert!(number.as_f64().unwrap() > 2.0 && number.as_f64().unwrap() < 3.0));
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_types_null() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let data = hive
            .query::<Value>("SELECT cast(NULL AS FLOAT), cast(NULL AS DOUBLE);")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert!(data[0][0].is_null());
        assert!(data[0][1].is_null());
    }

    #[cfg(feature = "test-sql-server")]
    #[test]
    fn test_sql_server_date() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive = Odbc::connect(&odbc, sql_server_connection_string().as_str())
            .expect("connect to Hive");
        let data = hive
            .query::<Value>("SELECT cast('2018-08-24' AS DATE) AS date")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::String(ref string) => assert_eq!(string.as_str(), "2018-08-24"));
    }

    #[cfg(feature = "test-sql-server")]
    #[test]
    fn test_sql_server_time() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive = Odbc::connect(&odbc, sql_server_connection_string().as_str())
            .expect("connect to Hive");
        let data = hive
            .query::<Value>("SELECT cast('10:22:33.7654321' AS TIME) AS date")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::String(ref string) => assert_eq!(string.as_str(), "10:22:33.7654321"));
    }

    #[derive(Debug)]
    struct Foo {
        val: i64,
    }

    impl TryFromRow for Foo {
        type Schema = Schema;
        type Error = ();
        fn try_from_row(mut values: Values, _schema: &Schema) -> Result<Self, ()> {
            Ok(values.pop().map(|val| Foo {
                val: val.as_i64().expect("val to be a number"),
            }).expect("value"))
        }
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_custom_type() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let foo = hive
            .query::<Foo>("SELECT 42 AS val;")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_eq!(foo[0].val, 42);
    }

    #[cfg(feature = "test-sql-server")]
    #[test]
    fn test_sql_server_query_with_parameters() {
        let odbc = Odbc::env().expect("open ODBC");
        let db = Odbc::connect(&odbc, sql_server_connection_string().as_str())
            .expect("connect to SQL Server");

        let val = 42;

        let foo: Vec<Foo> = db
            .query_with_parameters("SELECT ? AS val;", |q| q.bind(&val))
            .expect("failed to run query")
            .collect::<Result<_, _>>()
            .expect("fetch data");

        assert_eq!(foo[0].val, 42);
    }

    #[cfg(feature = "test-sql-server")]
    #[test]
    fn test_sql_server_query_with_many_parameters() {
        let odbc = Odbc::env().expect("open ODBC");
        let db = Odbc::connect(&odbc, sql_server_connection_string().as_str())
            .expect("connect to SQL Server");

        let val = [42, 24, 32, 666];

        let data: Vec<Value> = db
            .query_with_parameters("SELECT ?, ?, ?, ? AS val;", |q| {
                val.iter().fold(Ok(q), |q, v| q.and_then(|q| q.bind(v)))
            })
            .expect("failed to run query")
            .collect::<Result<_, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(42)));
        assert_matches!(data[0][1], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(24)));
        assert_matches!(data[0][2], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(32)));
        assert_matches!(data[0][3], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(666)));
    }

    #[cfg(feature = "test-sql-server")]
    #[test]
    fn test_sql_server_query_with_many_parameters_prepared() {
        let odbc = Odbc::env().expect("open ODBC");
        let db = Odbc::connect(&odbc, sql_server_connection_string().as_str())
            .expect("connect to SQL Server");

        let val = [42, 24, 32, 666];

        let statement = db.prepare("SELECT ?, ?, ?, ? AS val;").expect("prepare statement");

        let data: Vec<Value> = db
            .execute_with_parameters(statement, |q| {
                val.iter().fold(Ok(q), |q, v| q.and_then(|q| q.bind(v)))
            })
            .expect("failed to run query")
            .collect::<Result<_, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(42)));
        assert_matches!(data[0][1], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(24)));
        assert_matches!(data[0][2], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(32)));
        assert_matches!(data[0][3], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(666)));
    }


    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_empty_data_set() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let data = hive
            .query::<Value>("USE default;")
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert!(data.is_empty());
    }

    #[cfg(feature = "test-sql-server")]
    #[test]
    fn test_sql_server_long_string_fetch_utf_8() {
        let odbc = Odbc::env().expect("open ODBC");
        let sql_server = Odbc::connect(&odbc, sql_server_connection_string().as_str())
            .expect("connect to SQL Server");
        let data = sql_server
            .query::<Value>(&format!("SELECT N'{}'", LONG_STRING))
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::String(ref string) => assert_eq!(string, LONG_STRING));
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_long_string_fetch_utf_8() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let data = hive
            .query::<Value>(&format!("SELECT '{}'", LONG_STRING))
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::String(ref string) => assert_eq!(string, LONG_STRING));
    }

    #[cfg(feature = "test-monetdb")]
    #[test]
    fn test_moentdb_long_string_fetch_utf_8() {
        let odbc = Odbc::env().expect("open ODBC");
        let monetdb = Odbc::connect(&odbc, monetdb_connection_string().as_str())
            .expect("connect to MonetDB");
        let data = monetdb
            .query::<Value>(&format!("SELECT '{}'", LONG_STRING))
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::String(ref string) => assert_eq!(string, LONG_STRING));
    }

    #[cfg(feature = "test-sql-server")]
    #[test]
    fn test_sql_server_long_string_fetch_utf_16() {
        let odbc = Odbc::env().expect("open ODBC");
        let sql_server = Odbc::connect_with_options(
            &odbc,
            sql_server_connection_string().as_str(),
            Options {
                utf_16_strings: true,
            },
        )
        .expect("connect to SQL Server");
        let data = sql_server
            .query::<Value>(&format!("SELECT N'{}'", LONG_STRING))
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::String(ref string) => assert_eq!(string, LONG_STRING));
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_long_string_fetch_utf_16() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive = Odbc::connect_with_options(
            &odbc,
            hive_connection_string().as_str(),
            Options {
                utf_16_strings: true,
            },
        )
        .expect("connect to Hive");
        let data = hive
            .query::<Value>(&format!("SELECT '{}'", LONG_STRING))
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::String(ref string) => assert_eq!(string, LONG_STRING));
    }

    #[cfg(feature = "test-monetdb")]
    #[test]
    fn test_moentdb_long_string_fetch_utf_16() {
        let odbc = Odbc::env().expect("open ODBC");
        let monetdb = Odbc::connect_with_options(
            &odbc,
            monetdb_connection_string().as_str(),
            Options {
                utf_16_strings: true,
            },
        )
        .expect("connect to MonetDB");
        let data = monetdb
            .query::<Value>(&format!("SELECT '{}'", LONG_STRING))
            .expect("failed to run query")
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::String(ref string) => assert_eq!(string, LONG_STRING));
    }
    #[test]
    fn test_split_queries() {
        let queries = split_queries(
            r#"-- Foo
---
CREATE DATABASE IF NOT EXISTS daily_reports;
USE daily_reports;

SELECT *;"#,
        )
        .collect::<Result<Vec<_>, _>>()
        .expect("failed to parse");
        assert_eq!(
            queries,
            [
                "CREATE DATABASE IF NOT EXISTS daily_reports;",
                "USE daily_reports;",
                "SELECT *;"
            ]
        );
    }

    #[test]
    fn test_split_queries_end_white() {
        let queries = split_queries(
            r#"USE daily_reports;
SELECT *;

"#,
        )
        .collect::<Result<Vec<_>, _>>()
        .expect("failed to parse");
        assert_eq!(queries, ["USE daily_reports;", "SELECT *;"]);
    }

    #[test]
    fn test_split_queries_simple() {
        let queries = split_queries("SELECT 42;\nSELECT 24;\nSELECT 'foo';")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to parse");
        assert_eq!(queries, ["SELECT 42;", "SELECT 24;", "SELECT 'foo';"]);
    }

    #[test]
    fn test_split_queries_semicolon() {
        let queries = split_queries("SELECT 'foo; bar';\nSELECT 1;")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to parse");
        assert_eq!(queries, [r#"SELECT 'foo; bar';"#, "SELECT 1;"]);
    }

    #[test]
    fn test_split_queries_semicolon2() {
        let queries = split_queries(r#"foo "bar" baz "quix; but" foo "bar" baz "quix; but" fsad; foo "bar" baz "quix; but" foo "bar" baz "quix; but" fsad; select foo; foo "bar" baz 'quix; but' foo "bar" baz "quix; but" fsad; foo "bar" baz "quix; but" foo "bar" baz "quix; but" fsad; select foo;"#).collect::<Result<Vec<_>, _>>().expect("failed to parse");
        assert_eq!(
            queries,
            [
                r#"foo "bar" baz "quix; but" foo "bar" baz "quix; but" fsad;"#,
                r#"foo "bar" baz "quix; but" foo "bar" baz "quix; but" fsad;"#,
                r#"select foo;"#,
                r#"foo "bar" baz 'quix; but' foo "bar" baz "quix; but" fsad;"#,
                r#"foo "bar" baz "quix; but" foo "bar" baz "quix; but" fsad;"#,
                r#"select foo;"#,
            ]
        );
    }

    #[test]
    fn test_split_queries_escaped_quote() {
        let queries = split_queries("SELECT 'foo; b\\'ar';\nSELECT 1;")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to parse");
        assert_eq!(queries, [r#"SELECT 'foo; b\'ar';"#, "SELECT 1;"]);
    }

    #[test]
    fn test_split_queries_escaped_quote2() {
        let queries = split_queries("SELECT 'foo; b\\'ar';\nSELECT 'foo\\'bar';")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to parse");
        assert_eq!(
            queries,
            [r#"SELECT 'foo; b\'ar';"#, r#"SELECT 'foo\'bar';"#]
        );
    }

    #[test]
    fn test_split_queries_escaped_doublequote() {
        let queries = split_queries(r#"SELECT "foo; b\"ar";SELECT "foo\"bar";"#)
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to parse");
        assert_eq!(
            queries,
            [r#"SELECT "foo; b\"ar";"#, r#"SELECT "foo\"bar";"#]
        );
    }

    #[test]
    fn test_split_queries_comments() {
        let queries =
            split_queries("SELECT 1;\n-- SELECT x;\n---- SELECT x;\nSELECT 2;\nSELECT 3;")
                .collect::<Result<Vec<_>, _>>()
                .expect("failed to parse");
        assert_eq!(queries, ["SELECT 1;", "SELECT 2;", "SELECT 3;"]);
    }

    #[test]
    fn test_split_queries_comments2() {
        let queries = split_queries("-- TODO: add last_search_or_brochure_logentry_id\n-- TODO: DISTRIBUTE BY analytics_record_id SORT BY analytics_record_id ASC;\n-- TODO: check previous day for landing logentry detail\nSELECT '1' LEFT JOIN source_wcc.domain d ON regexp_extract(d.domain, '.*\\\\.([^\\.]+)$', 1) = c.domain AND d.snapshot_day = c.index;").collect::<Result<Vec<_>, _>>().expect("failed to parse");
        assert_eq!(queries, [r#"SELECT '1' LEFT JOIN source_wcc.domain d ON regexp_extract(d.domain, '.*\\.([^\.]+)$', 1) = c.domain AND d.snapshot_day = c.index;"#]);
    }

    #[test]
    fn test_split_queries_control() {
        let queries = split_queries(
            "!outputformat vertical\nSELECT 1;\n-- SELECT x;\n---- SELECT x;\nSELECT 2;\nSELECT 3;",
        )
        .collect::<Result<Vec<_>, _>>()
        .expect("failed to parse");
        assert_eq!(queries, ["SELECT 1;", "SELECT 2;", "SELECT 3;"]);
    }

    #[test]
    fn test_split_queries_white() {
        let queries = split_queries(" \n  SELECT 1;\n  \nSELECT 2;\n \nSELECT 3;\n\n ")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to parse");
        assert_eq!(queries, ["SELECT 1;", "SELECT 2;", "SELECT 3;"]);
    }

    #[test]
    fn test_split_queries_white2() {
        let queries = split_queries("SELECT 1; \t \nSELECT 2; \n \nSELECT 3; ")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to parse");
        assert_eq!(queries, ["SELECT 1;", "SELECT 2;", "SELECT 3;"]);
    }

    #[test]
    fn test_split_queries_white_comment() {
        let queries = split_queries("SELECT 1; \t \nSELECT 2; -- foo bar\n \nSELECT 3; ")
            .collect::<Result<Vec<_>, _>>()
            .expect("failed to parse");
        assert_eq!(queries, ["SELECT 1;", "SELECT 2;", "SELECT 3;"]);
    }

    #[cfg(feature = "test-hive")]
    #[test]
    fn test_hive_multiple_queries() {
        let odbc = Odbc::env().expect("open ODBC");
        let hive =
            Odbc::connect(&odbc, hive_connection_string().as_str()).expect("connect to Hive");
        let data = hive
            .query_multiple::<Value>("SELECT 42;\nSELECT 24;\nSELECT 'foo';")
            .flat_map(|i| i.expect("failed to run query"))
            .collect::<Result<Vec<_>, _>>()
            .expect("fetch data");

        assert_matches!(data[0][0], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(42)));
        assert_matches!(data[1][0], Value::Number(ref number) => assert_eq!(number.as_i64(), Some(24)));
        assert_matches!(data[2][0], Value::String(ref string) => assert_eq!(string, "foo"));
    }
}
