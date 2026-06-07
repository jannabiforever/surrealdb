/**
[env]
imports = ["bench/util/records-100000.surql"]

[bench]
reason = "Select record ids from the first 100 records"
run = true
rebuild = false
warmup = "2s"
sample-size = 50
measurement-time = "20s"

[test]
run = false
*/

SELECT id FROM record LIMIT 100;
