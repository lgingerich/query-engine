-- DuckDB's version of `clickbench-datafusion.sql`, for the cross-check in
-- `scripts/perf bounds`, which first creates `hits_raw` as a view over the
-- file. DuckDB can't cast an integer to a date; adding the day number to the
-- epoch gives the same dates.
CREATE VIEW hits AS
SELECT * REPLACE (DATE '1970-01-01' + CAST("EventDate" AS INTEGER) AS "EventDate")
FROM hits_raw;
