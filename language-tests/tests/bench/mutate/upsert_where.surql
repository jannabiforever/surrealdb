/**
[env]
imports = ["bench/util/records-100000.surql"]

[bench]
reason = "Upsert all records matching a predicate"
run = true
rebuild = true
warmup = "2s"
sample-size = 50
measurement-time = "20s"

[test]
run = false
*/

UPSERT record SET active = true WHERE number = 21;
