use h2::Connection;

#[test]
fn test_recursive_cte_sequence() {
    let conn = Connection::open_in_memory().unwrap();

    let rows = conn.query(
        "WITH RECURSIVE cnt(x) AS (
            SELECT 1
            UNION ALL
            SELECT x + 1 FROM cnt WHERE x < 5
        )
        SELECT x FROM cnt ORDER BY x"
    ).unwrap();

    assert_eq!(rows.len(), 5);
    let values: Vec<i32> = rows.iter().map(|r| r.get_as::<i32>(0).unwrap()).collect();
    assert_eq!(values, vec![1, 2, 3, 4, 5]);
}

#[test]
fn test_recursive_cte_hierarchy() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE employees (id INT PRIMARY KEY, name VARCHAR, manager_id INT)").unwrap();
    conn.execute("INSERT INTO employees VALUES (1, 'CEO', NULL)").unwrap();
    conn.execute("INSERT INTO employees VALUES (2, 'VP', 1)").unwrap();
    conn.execute("INSERT INTO employees VALUES (3, 'Manager', 2)").unwrap();
    conn.execute("INSERT INTO employees VALUES (4, 'Engineer', 3)").unwrap();

    let rows = conn.query(
        "WITH RECURSIVE org(id, name, level) AS (
            SELECT id, name, 1 AS level FROM employees WHERE manager_id IS NULL
            UNION ALL
            SELECT e.id, e.name, o.level + 1
            FROM employees e
            JOIN org o ON e.manager_id = o.id
        )
        SELECT id, name, level FROM org ORDER BY level"
    ).unwrap();

    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "CEO");
    assert_eq!(rows[0].get_as::<i32>(2).unwrap(), 1);

    assert_eq!(rows[3].get_as::<String>(1).unwrap(), "Engineer");
    assert_eq!(rows[3].get_as::<i32>(2).unwrap(), 4);
}

#[test]
fn test_recursive_cte_cycle_union_distinct() {
    let conn = Connection::open_in_memory().unwrap();
    // 循環エッジ: 1 -> 2 -> 1
    conn.execute("CREATE TABLE edges (source INT, target INT)").unwrap();
    conn.execute("INSERT INTO edges VALUES (1, 2), (2, 1)").unwrap();

    // UNION (DISTINCT) により重複ノードへの到達で反復が停止すること
    let rows = conn.query(
        "WITH RECURSIVE reachable(node) AS (
            SELECT 1
            UNION
            SELECT e.target
            FROM edges e
            JOIN reachable r ON e.source = r.node
        )
        SELECT node FROM reachable ORDER BY node"
    ).unwrap();

    assert_eq!(rows.len(), 2);
    let nodes: Vec<i32> = rows.iter().map(|r| r.get_as::<i32>(0).unwrap()).collect();
    assert_eq!(nodes, vec![1, 2]);
}

#[test]
fn test_non_recursive_cte_column_aliases() {
    let conn = Connection::open_in_memory().unwrap();
    let rows = conn.query(
        "WITH val_table(col_a, col_b) AS (
            SELECT 10, 20
        )
        SELECT col_a + col_b AS sum_ab FROM val_table"
    ).unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 30);
}
