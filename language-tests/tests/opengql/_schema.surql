/**
[test]
run = false
*/

-- Seed for the OpenGQL (.gql) conformance corpus: people (one without age,
-- one without active), a non-person node, and deterministic knows edges —
-- including one person→city edge and one edge without a since property.
-- The person-only edge graph over 1/2/3 is 1→2, 2→1, 2→3, 3→1, giving 2-hop
-- chains and a 1↔2 cycle for the variable-length tests.
CREATE person:1 SET name = 'A', age = 30, active = true, city = 'London' RETURN NONE;
CREATE person:2 SET name = 'B', age = 20, active = false, city = 'Paris' RETURN NONE;
CREATE person:3 SET name = 'C', city = 'London' RETURN NONE;
CREATE city:1 SET name = 'London' RETURN NONE;
INSERT RELATION INTO knows [
	{ id: knows:k12, in: person:1, out: person:2, since: 2021 },
	{ id: knows:k21, in: person:2, out: person:1, since: 2018 },
	{ id: knows:k23, in: person:2, out: person:3, since: 2020 },
	{ id: knows:k1c, in: person:1, out: city:1, since: 2019 },
	{ id: knows:k31, in: person:3, out: person:1 }
] RETURN NONE;

-- A clean directed triangle cycle 4→5→6→4 over a SEPARATE `hub` node table
-- joined by `link` edges, fully disjoint from the person/knows/city component
-- above. Keeping it on its own tables means `(n:person)` scans in the existing
-- corpus are unaffected (their expectations stay byte-identical), while the
-- per-path quantifier tests get a single simple cycle whose path counts at each
-- depth are hand-derivable without interference from the denser 1/2/3 graph.
-- Deterministic `since` values let edge-group / path-variable tests pin exact
-- edge records.
CREATE hub:4 SET name = 'D', age = 40 RETURN NONE;
CREATE hub:5 SET name = 'E', age = 50 RETURN NONE;
CREATE hub:6 SET name = 'F', age = 60 RETURN NONE;
INSERT RELATION INTO link [
	{ id: link:l45, in: hub:4, out: hub:5, since: 2014 },
	{ id: link:l56, in: hub:5, out: hub:6, since: 2015 },
	{ id: link:l64, in: hub:6, out: hub:4, since: 2016 }
] RETURN NONE;
