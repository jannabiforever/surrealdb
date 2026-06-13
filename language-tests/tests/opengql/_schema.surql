/**
[test]
run = false
*/

-- Seed for the OpenGQL (.gql) conformance corpus: people (one without age,
-- one without active), a non-person node, and deterministic knows edges —
-- including one person→city edge and one edge without a since property.
-- The person-only edge graph is 1→2, 2→1, 2→3, 3→1, giving 2-hop chains
-- and cycles for the variable-length tests.
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
