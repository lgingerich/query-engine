//! Registered tables and the entry point for running SQL

use crate::{errors::EngineError, parquet::ParquetSource};
use crate::query::parse;
use crate::types::{Batch, Schema};
use std::collections::HashMap;

/// Output of a query: its schema and all result batches
#[derive(Debug)]
pub struct QueryResult {
    schema: Schema,
    results: Vec<Batch>
}

/// Tables registered by name
#[derive(Debug, Default)]
pub struct Session {
    tables: HashMap<String, ParquetSource>,
}

impl Session {
    /// Empty session
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `source` under `name`
    ///
    /// # Errors
    ///
    /// Fails if a table is already registered under `name`
    pub fn register(
        &mut self,
        name: impl Into<String>,
        source: ParquetSource,
    ) -> Result<(), EngineError> {
        let name = name.into();
        if self.tables.contains_key(&name) {
            return Err(EngineError::TableExists(name));
        }
        self.tables.insert(name, source);
        Ok(())
    }

    /// Runs `sql` and returns all results
    ///
    /// # Errors
    ///
    /// Fails if the SQL is invalid or unsupported
    pub fn execute(&self, sql: &str) -> Result<QueryResult, EngineError> {
        // 1. parse query
        let query = parse(sql)?;
        println!("{query:#?}");

        // 2. bind query
        
        // 3. plan query
        
        // 4. scan + execute


        Ok(QueryResult {
            schema: Schema::new(Vec::new()),
            results: Vec::new(),
        })
    }
}
