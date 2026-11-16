#[derive(Debug)]
pub enum Operations {
    GT,
    GTE,
    LT,
    LTE,
}

#[derive(Debug)]
pub struct Filter {
    pub col_name: String,
    pub op: Operations,
    pub val: i64,
}
