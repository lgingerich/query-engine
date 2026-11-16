use crate::{errors::EngineError, parquet::ParquetSource};
use crate::query::parse;
use crate::types::{Batch, Schema};
use std::collections::HashMap;

#[derive(Debug)]
pub struct QueryResult {
    schema: Schema,
    results: Vec<Batch>
}

#[derive(Debug, Default)]
pub struct Session {
    tables: HashMap<String, ParquetSource>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

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

    pub fn execute(&self, sql: &str) -> Result<QueryResult, EngineError> {
        // 1. parse query
        let query = parse(sql)?;
        println!("{query:#?}");

        // 2. ...
        
        // 3. ...
        
        // 4. ...




        Ok(QueryResult {
            schema: Schema::new(Vec::new()),
            results: Vec::new(),
        })
    }
}
