-- From ClickBench `datafusion-partitioned/create.sql`. The partitioned files
-- store `EventDate` as a UInt16 day number; the queries compare it to dates.
-- `scripts/perf` registers the file as `hits_raw` for DataFusion.
CREATE VIEW hits AS
SELECT * EXCEPT ("EventDate"),
       CAST(CAST("EventDate" AS INTEGER) AS DATE) AS "EventDate"
FROM hits_raw;
