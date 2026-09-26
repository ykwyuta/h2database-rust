pub mod model;
pub mod store;
pub mod parser;
pub mod engine;

pub use model::{Edge, GraphResult, GraphStats, GraphValue, Node, Path};
pub use store::GraphStore;
pub use parser::{Direction, Expr, Query, QueryParser};
pub use engine::GraphEngine;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use h2_mvstore::MVStore;
    use crate::engine::GraphEngine;
    use crate::model::GraphValue;

    fn setup_engine() -> GraphEngine {
        let store = Arc::new(MVStore::open_in_memory());
        GraphEngine::new(store, "test_graph").expect("Failed to initialize GraphEngine")
    }

    #[test]
    fn test_create_and_match_node() {
        let engine = setup_engine();

        // 1. Create nodes
        let res = engine
            .execute("CREATE (a:Person {name: 'Alice', age: 30})")
            .unwrap();
        assert_eq!(res.stats.nodes_created, 1);
        assert_eq!(res.stats.properties_set, 2);

        let res = engine
            .execute("CREATE (b:Person {name: 'Bob', age: 25})")
            .unwrap();
        assert_eq!(res.stats.nodes_created, 1);

        // 2. Match nodes with property filter
        let res = engine
            .execute("MATCH (p:Person {name: 'Alice'}) RETURN p.name, p.age")
            .unwrap();
        assert_eq!(res.columns, vec!["p.name", "p.age"]);
        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0][0], GraphValue::String("Alice".to_string()));
        assert_eq!(res.rows[0][1], GraphValue::Integer(30));

        // 3. Match all nodes with WHERE clause
        let res = engine
            .execute("MATCH (p:Person) WHERE p.age > 26 RETURN p.name ORDER BY p.age ASC")
            .unwrap();
        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0][0], GraphValue::String("Alice".to_string()));
    }

    #[test]
    fn test_relationships_and_traversal() {
        let engine = setup_engine();

        // Create graph: Alice -> KNOWS -> Bob -> KNOWS -> Charlie
        engine
            .execute("CREATE (a:Person {name: 'Alice'})-[:KNOWS {since: 2021}]->(b:Person {name: 'Bob'})")
            .unwrap();
        engine
            .execute("MATCH (b:Person {name: 'Bob'}) CREATE (b)-[:KNOWS {since: 2023}]->(c:Person {name: 'Charlie'})")
            .unwrap();

        // 1-hop traversal
        let res = engine
            .execute("MATCH (a:Person)-[r:KNOWS]->(b:Person) RETURN a.name, type(r), r.since, b.name ORDER BY a.name")
            .unwrap();
        assert_eq!(res.rows.len(), 2);
        assert_eq!(res.rows[0][0], GraphValue::String("Alice".to_string()));
        assert_eq!(res.rows[0][1], GraphValue::String("KNOWS".to_string()));
        assert_eq!(res.rows[0][2], GraphValue::Integer(2021));
        assert_eq!(res.rows[0][3], GraphValue::String("Bob".to_string()));

        assert_eq!(res.rows[1][0], GraphValue::String("Bob".to_string()));
        assert_eq!(res.rows[1][3], GraphValue::String("Charlie".to_string()));
    }

    #[test]
    fn test_variable_length_path() {
        let engine = setup_engine();

        engine.execute("CREATE (a:City {name: 'Tokyo'})").unwrap();
        engine.execute("CREATE (b:City {name: 'Nagoya'})").unwrap();
        engine.execute("CREATE (c:City {name: 'Kyoto'})").unwrap();
        engine.execute("CREATE (d:City {name: 'Osaka'})").unwrap();

        engine.execute("MATCH (a:City {name: 'Tokyo'}), (b:City {name: 'Nagoya'}) CREATE (a)-[:CONNECTS]->(b)").unwrap();
        engine.execute("MATCH (b:City {name: 'Nagoya'}), (c:City {name: 'Kyoto'}) CREATE (b)-[:CONNECTS]->(c)").unwrap();
        engine.execute("MATCH (c:City {name: 'Kyoto'}), (d:City {name: 'Osaka'}) CREATE (c)-[:CONNECTS]->(d)").unwrap();

        // 2-hop to 3-hop traversal
        let res = engine
            .execute("MATCH (start:City {name: 'Tokyo'})-[:CONNECTS*2..3]->(target:City) RETURN target.name")
            .unwrap();

        let target_names: Vec<String> = res
            .rows
            .iter()
            .map(|r| r[0].as_str().unwrap().to_string())
            .collect();

        assert_eq!(target_names.len(), 2);
        assert!(target_names.contains(&"Kyoto".to_string()));
        assert!(target_names.contains(&"Osaka".to_string()));
    }

    #[test]
    fn test_shortest_path() {
        let engine = setup_engine();

        // Diamond topology:
        // A -> B -> D (length 2)
        // A -> C1 -> C2 -> D (length 3)
        engine.execute("CREATE (a:Point {id: 'A'}), (b:Point {id: 'B'}), (c1:Point {id: 'C1'}), (c2:Point {id: 'C2'}), (d:Point {id: 'D'})").unwrap();
        engine.execute("MATCH (a:Point {id: 'A'}), (b:Point {id: 'B'}) CREATE (a)-[:ROUTE]->(b)").unwrap();
        engine.execute("MATCH (b:Point {id: 'B'}), (d:Point {id: 'D'}) CREATE (b)-[:ROUTE]->(d)").unwrap();
        engine.execute("MATCH (a:Point {id: 'A'}), (c1:Point {id: 'C1'}) CREATE (a)-[:ROUTE]->(c1)").unwrap();
        engine.execute("MATCH (c1:Point {id: 'C1'}), (c2:Point {id: 'C2'}) CREATE (c1)-[:ROUTE]->(c2)").unwrap();
        engine.execute("MATCH (c2:Point {id: 'C2'}), (d:Point {id: 'D'}) CREATE (c2)-[:ROUTE]->(d)").unwrap();

        let res = engine
            .execute("MATCH p = shortestPath((a:Point {id: 'A'})-[:ROUTE*]->(d:Point {id: 'D'})) RETURN length(p) AS len")
            .unwrap();

        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0][0], GraphValue::Integer(2));
    }

    #[test]
    fn test_merge_upsert() {
        let engine = setup_engine();

        // 1. Initial MERGE should create
        let res1 = engine
            .execute("MERGE (c:Country {code: 'JP'}) ON CREATE SET c.name = 'Japan', c.created = 1 RETURN c.code, c.name, c.created")
            .unwrap();
        assert_eq!(res1.stats.nodes_created, 1);
        assert_eq!(res1.rows[0][1], GraphValue::String("Japan".to_string()));
        assert_eq!(res1.rows[0][2], GraphValue::Integer(1));

        // 2. Second MERGE should match and update
        let res2 = engine
            .execute("MERGE (c:Country {code: 'JP'}) ON MATCH SET c.updated = 2 RETURN c.code, c.name, c.updated")
            .unwrap();
        assert_eq!(res2.stats.nodes_created, 0);
        assert_eq!(res2.rows[0][0], GraphValue::String("JP".to_string()));
        assert_eq!(res2.rows[0][1], GraphValue::String("Japan".to_string()));
        assert_eq!(res2.rows[0][2], GraphValue::Integer(2));
    }

    #[test]
    fn test_aggregations() {
        let engine = setup_engine();

        engine.execute("CREATE (:Dev {name: 'Alice', score: 100})").unwrap();
        engine.execute("CREATE (:Dev {name: 'Bob', score: 80})").unwrap();
        engine.execute("CREATE (:Dev {name: 'Charlie', score: 60})").unwrap();

        let res = engine
            .execute("MATCH (d:Dev) RETURN count(*), sum(d.score), avg(d.score), min(d.score), max(d.score)")
            .unwrap();

        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0][0], GraphValue::Integer(3)); // count
        assert_eq!(res.rows[0][1], GraphValue::Integer(240)); // sum
        assert_eq!(res.rows[0][2], GraphValue::Float(80.0)); // avg
        assert_eq!(res.rows[0][3], GraphValue::Integer(60)); // min
        assert_eq!(res.rows[0][4], GraphValue::Integer(100)); // max
    }

    #[test]
    fn test_set_and_detach_delete() {
        let engine = setup_engine();

        engine.execute("CREATE (u:User {name: 'Dave', status: 'pending'})").unwrap();

        // SET property and label
        let res = engine
            .execute("MATCH (u:User {name: 'Dave'}) SET u.status = 'active', u:Verified RETURN u.name, u.status, labels(u)")
            .unwrap();
        assert_eq!(res.rows[0][1], GraphValue::String("active".to_string()));
        let labels = &res.rows[0][2];
        if let GraphValue::List(lbls) = labels {
            assert!(lbls.contains(&GraphValue::String("User".to_string())));
            assert!(lbls.contains(&GraphValue::String("Verified".to_string())));
        } else {
            panic!("Expected list of labels");
        }

        // DETACH DELETE
        let del_res = engine.execute("MATCH (u:User {name: 'Dave'}) DETACH DELETE u").unwrap();
        assert_eq!(del_res.stats.nodes_deleted, 1);

        let check = engine.execute("MATCH (u:User) RETURN count(*)").unwrap();
        assert_eq!(check.rows[0][0], GraphValue::Integer(0));
    }

    #[test]
    fn test_parameterized_query() {
        let engine = setup_engine();

        let mut params = HashMap::new();
        params.insert("target_role".to_string(), GraphValue::String("admin".to_string()));
        params.insert("min_level".to_string(), GraphValue::Integer(5));

        engine.execute("CREATE (:Admin {role: 'admin', level: 10})").unwrap();
        engine.execute("CREATE (:Admin {role: 'admin', level: 2})").unwrap();
        engine.execute("CREATE (:Admin {role: 'user', level: 8})").unwrap();

        let res = engine
            .execute_with_params(
                "MATCH (a:Admin) WHERE a.role = $target_role AND a.level >= $min_level RETURN a.level",
                &params,
            )
            .unwrap();

        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0][0], GraphValue::Integer(10));
    }
}
