use h2::Connection;

#[test]
fn test_insert_into_select() {
    let conn = Connection::open_in_memory().unwrap();

    // 1. テーブルの準備
    conn.execute(
        "CREATE TABLE source_users (id INT PRIMARY KEY, name VARCHAR(50), score INT, city VARCHAR(50))"
    ).unwrap();
    conn.execute("INSERT INTO source_users VALUES (1, 'Alice', 95, 'Tokyo')").unwrap();
    conn.execute("INSERT INTO source_users VALUES (2, 'Bob', 80, 'Osaka')").unwrap();
    conn.execute("INSERT INTO source_users VALUES (3, 'Charlie', 60, 'Tokyo')").unwrap();
    conn.execute("INSERT INTO source_users VALUES (4, 'David', 90, 'Kyoto')").unwrap();

    // 2. INSERT INTO ... SELECT ... 全カラムの直接転送
    conn.execute(
        "CREATE TABLE dest_honors (id INT PRIMARY KEY, name VARCHAR(50), score INT, city VARCHAR(50))"
    ).unwrap();
    let res = conn.execute(
        "INSERT INTO dest_honors SELECT * FROM source_users WHERE score >= 90"
    ).unwrap();
    assert_eq!(res, 2);

    let rows = conn.query("SELECT id, name, score, city FROM dest_honors ORDER BY id").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Alice");
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "David");

    // 3. INSERT INTO <table> (cols...) SELECT ... 部分カラム指定 & 式計算の転送
    conn.execute(
        "CREATE TABLE dest_summary (user_name VARCHAR(50) PRIMARY KEY, bonus INT, note VARCHAR(50))"
    ).unwrap();
    let res = conn.execute(
        "INSERT INTO dest_summary (user_name, bonus) SELECT name, score * 10 FROM source_users WHERE city = 'Tokyo'"
    ).unwrap();
    assert_eq!(res, 2);

    let rows = conn.query("SELECT user_name, bonus, note FROM dest_summary ORDER BY user_name").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Alice");
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 950);
    assert!(rows[0].get(2).unwrap().is_null()); // 未指定カラムは NULL

    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Charlie");
    assert_eq!(rows[1].get_as::<i32>(1).unwrap(), 600);

    // 4. 集約クエリ結果を INSERT INTO ... SELECT ... で集計テーブルに登録
    conn.execute(
        "CREATE TABLE city_stats (city VARCHAR(50) PRIMARY KEY, count INT, avg_score INT)"
    ).unwrap();
    conn.execute(
        "INSERT INTO city_stats SELECT city, COUNT(*), AVG(score) FROM source_users GROUP BY city"
    ).unwrap();

    let rows = conn.query("SELECT city, count, avg_score FROM city_stats ORDER BY city").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Kyoto");
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 1);
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Osaka");
    assert_eq!(rows[1].get_as::<i32>(1).unwrap(), 1);
    assert_eq!(rows[2].get_as::<String>(0).unwrap(), "Tokyo");
    assert_eq!(rows[2].get_as::<i32>(1).unwrap(), 2);
}

#[test]
fn test_common_table_expressions_cte() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR(50), category VARCHAR(50), price INT)"
    ).unwrap();
    conn.execute("INSERT INTO products VALUES (1, 'Apple', 'Fruit', 150)").unwrap();
    conn.execute("INSERT INTO products VALUES (2, 'Banana', 'Fruit', 100)").unwrap();
    conn.execute("INSERT INTO products VALUES (3, 'Carrot', 'Vegetable', 80)").unwrap();
    conn.execute("INSERT INTO products VALUES (4, 'Daikon', 'Vegetable', 120)").unwrap();
    conn.execute("INSERT INTO products VALUES (5, 'Eggplant', 'Vegetable', 200)").unwrap();

    // 1. 基本的な WITH 句 (単一 CTE)
    let rows = conn.query(
        "WITH cheap_items AS (SELECT id, name, price FROM products WHERE price <= 120) \
         SELECT name, price FROM cheap_items ORDER BY price DESC"
    ).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Daikon");
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Banana");
    assert_eq!(rows[2].get_as::<String>(0).unwrap(), "Carrot");

    // 2. CTE 内での計算・別名と、メインクエリでの集計
    let rows = conn.query(
        "WITH taxed_products AS (SELECT category, price * 1.1 AS tax_price FROM products) \
         SELECT category, COUNT(*), SUM(tax_price) FROM taxed_products GROUP BY category ORDER BY category"
    ).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Fruit");
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 2);
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Vegetable");
    assert_eq!(rows[1].get_as::<i32>(1).unwrap(), 3);

    // 3. 複数 CTE (先行する CTE を参照するチェイン CTE)
    let rows = conn.query(
        "WITH veg AS (SELECT * FROM products WHERE category = 'Vegetable'), \
              expensive_veg AS (SELECT name, price FROM veg WHERE price > 100) \
         SELECT name, price FROM expensive_veg ORDER BY price"
    ).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Daikon");
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Eggplant");

    // 4. CTE と通常テーブルの JOIN
    conn.execute(
        "CREATE TABLE sales (id INT PRIMARY KEY, product_id INT, quantity INT)"
    ).unwrap();
    conn.execute("INSERT INTO sales VALUES (1, 1, 10)").unwrap();
    conn.execute("INSERT INTO sales VALUES (2, 3, 5)").unwrap();
    conn.execute("INSERT INTO sales VALUES (3, 5, 2)").unwrap();

    let rows = conn.query(
        "WITH top_sales AS (SELECT product_id, quantity FROM sales WHERE quantity >= 5) \
         SELECT p.name, s.quantity \
         FROM top_sales s \
         JOIN products p ON s.product_id = p.id \
         ORDER BY s.quantity DESC"
    ).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Apple");
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 10);
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Carrot");
    assert_eq!(rows[1].get_as::<i32>(1).unwrap(), 5);

    // 5. CTE を組み合わせた INSERT INTO ... SELECT
    conn.execute("CREATE TABLE fruit_archive (id INT PRIMARY KEY, name VARCHAR(50), price INT)").unwrap();
    let res = conn.execute(
        "INSERT INTO fruit_archive \
         WITH target_fruit AS (SELECT id, name, price FROM products WHERE category = 'Fruit') \
         SELECT * FROM target_fruit"
    ).unwrap();
    assert_eq!(res, 2);

    let rows = conn.query("SELECT name FROM fruit_archive ORDER BY id").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Apple");
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Banana");
}

#[test]
fn test_foreign_key_constraints() {
    let conn = Connection::open_in_memory().unwrap();

    // 1. 親テーブルの作成
    conn.execute("CREATE TABLE departments (id INT PRIMARY KEY, name VARCHAR(50))").unwrap();
    conn.execute("INSERT INTO departments VALUES (10, 'Development')").unwrap();
    conn.execute("INSERT INTO departments VALUES (20, 'Marketing')").unwrap();

    // 2. カラムレベル制約 REFERENCES による子テーブル作成 (デフォルト RESTRICT)
    conn.execute(
        "CREATE TABLE employees (id INT PRIMARY KEY, name VARCHAR(50), dept_id INT REFERENCES departments(id))"
    ).unwrap();

    // 正常挿入
    conn.execute("INSERT INTO employees VALUES (1, 'Alice', 10)").unwrap();
    conn.execute("INSERT INTO employees VALUES (2, 'Bob', 20)").unwrap();

    // NULL の外部キーは許容される (SQL標準)
    conn.execute("INSERT INTO employees VALUES (3, 'Charlie', NULL)").unwrap();

    // 存在しない親キーの挿入はエラーになること
    let err = conn.execute("INSERT INTO employees VALUES (4, 'Dave', 999)");
    assert!(err.is_err(), "Non-existent foreign key insert should fail");
    let err_msg = err.unwrap_err().to_string();
    assert!(err_msg.contains("Foreign key constraint violation"), "Got: {}", err_msg);

    // 外部キー更新時の制約検証
    let update_err = conn.execute("UPDATE employees SET dept_id = 888 WHERE id = 1");
    assert!(update_err.is_err(), "Updating to non-existent foreign key should fail");

    // 3. 親行の削除制限 (RESTRICT)
    // dept 10 は Alice に参照されているため削除できない
    let del_err = conn.execute("DELETE FROM departments WHERE id = 10");
    assert!(del_err.is_err(), "Deleting referenced parent row should fail under RESTRICT");

    // 被参照テーブルの DROP TABLE / TRUNCATE TABLE も制限されること
    let drop_err = conn.execute("DROP TABLE departments");
    assert!(drop_err.is_err(), "Dropping referenced parent table should fail");

    let trunc_err = conn.execute("TRUNCATE TABLE departments");
    assert!(trunc_err.is_err(), "Truncating referenced parent table should fail");

    // 4. テーブル制約レベル & ON DELETE CASCADE
    conn.execute(
        "CREATE TABLE projects (id INT PRIMARY KEY, name VARCHAR(50), dept_id INT, \
         FOREIGN KEY (dept_id) REFERENCES departments(id) ON DELETE CASCADE)"
    ).unwrap();
    conn.execute("INSERT INTO projects VALUES (100, 'Cloud Migration', 20)").unwrap();
    conn.execute("INSERT INTO projects VALUES (101, 'Ad Campaign', 20)").unwrap();

    // dept 20 には Bob (employees) がいるので、そのままでは departments id=20 の削除は employees 側の RESTRICT で防がれる
    let del_err2 = conn.execute("DELETE FROM departments WHERE id = 20");
    assert!(del_err2.is_err());

    // Bob を別部署または削除
    conn.execute("DELETE FROM employees WHERE id = 2").unwrap();

    // これで dept 20 を削除すると、projects の 100, 101 が CASCADE 削除される！
    conn.execute("DELETE FROM departments WHERE id = 20").unwrap();

    let dept_rows = conn.query("SELECT * FROM departments WHERE id = 20").unwrap();
    assert_eq!(dept_rows.len(), 0);

    let proj_rows = conn.query("SELECT * FROM projects WHERE dept_id = 20").unwrap();
    assert_eq!(proj_rows.len(), 0, "Projects should have been cascaded and deleted");

    // 5. ON DELETE SET NULL
    conn.execute("INSERT INTO departments VALUES (30, 'Human Resources')").unwrap();
    conn.execute(
        "CREATE TABLE audits (id INT PRIMARY KEY, action VARCHAR(50), dept_id INT, \
         FOREIGN KEY (dept_id) REFERENCES departments(id) ON DELETE SET NULL)"
    ).unwrap();
    conn.execute("INSERT INTO audits VALUES (1, 'Security Review', 30)").unwrap();

    conn.execute("DELETE FROM departments WHERE id = 30").unwrap();

    let audit_rows = conn.query("SELECT id, action, dept_id FROM audits WHERE id = 1").unwrap();
    assert_eq!(audit_rows.len(), 1);
    assert!(audit_rows[0].get(2).unwrap().is_null(), "dept_id should have been set to NULL");

    // 6. ON UPDATE CASCADE / RESTRICT
    conn.execute("INSERT INTO departments VALUES (40, 'Finance')").unwrap();
    conn.execute(
        "CREATE TABLE budgets (id INT PRIMARY KEY, amount INT, dept_id INT, \
         FOREIGN KEY (dept_id) REFERENCES departments(id) ON UPDATE CASCADE ON DELETE CASCADE)"
    ).unwrap();
    conn.execute("INSERT INTO budgets VALUES (1, 50000, 40)").unwrap();

    // 親のキーを 40 -> 400 に更新すると子行の dept_id も 400 に連動更新される
    conn.execute("UPDATE departments SET id = 400 WHERE id = 40").unwrap();
    let budget_rows = conn.query("SELECT id, dept_id FROM budgets WHERE id = 1").unwrap();
    assert_eq!(budget_rows.len(), 1);
    assert_eq!(budget_rows[0].get_as::<i32>(1).unwrap(), 400);

    // employees (RESTRICT) で参照されている dept 10 の親キーを更新しようとするとエラー
    let update_parent_err = conn.execute("UPDATE departments SET id = 100 WHERE id = 10");
    assert!(update_parent_err.is_err(), "Updating referenced parent key should fail under RESTRICT");
}

