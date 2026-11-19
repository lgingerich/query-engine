//! Tests for the bounds bench, `benches/bounds/`. A bench with its own `main`
//! can't hold `#[test]`s, so this binary compiles the bench's modules from
//! their files and tests them here.

#[path = "../../benches/bounds/file.rs"]
mod file;
#[path = "../../benches/bounds/measure.rs"]
mod measure;
#[path = "../../benches/bounds/oracle.rs"]
mod oracle;
#[path = "../../benches/bounds/scan.rs"]
mod scan;
#[path = "../../benches/bounds/stats.rs"]
mod stats;

mod fixture;
#[cfg(test)]
mod masks;
#[cfg(test)]
mod scan_bounds;
#[cfg(test)]
mod statistics;
