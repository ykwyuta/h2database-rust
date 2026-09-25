package com.example;

import java.math.BigDecimal;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.Statement;

/**
 * H2 Database in Rust に対して標準 PostgreSQL JDBC ドライバ経由で接続・操作するデモプログラム。
 */
public class H2JdbcDemo {

    private static final String JDBC_URL = "jdbc:postgresql://localhost:5432/mydb?preferQueryMode=simple";
    private static final String DB_USER = "postgres";
    private static final String DB_PASS = ""; // パスワード不要

    public static void main(String[] args) {
        System.out.println("============================================================");
        System.out.println("  H2 Database in Rust - Java JDBC Connection Demo");
        System.out.println("============================================================");

        try {
            // 1. PostgreSQL JDBC ドライバのロード
            Class.forName("org.postgresql.Driver");
            System.out.println("[1] PostgreSQL JDBC Driver loaded successfully.");

            // 2. 接続の確立
            System.out.println("[2] Connecting to " + JDBC_URL + " ...");
            try (Connection conn = DriverManager.getConnection(JDBC_URL, DB_USER, DB_PASS)) {
                System.out.println("  Connected! Server Product: " + conn.getMetaData().getDatabaseProductName() 
                    + ", Version: " + conn.getMetaData().getDatabaseProductVersion());

                try (Statement stmt = conn.createStatement()) {
                    // 3. テーブルの作成 (DDL)
                    stmt.execute("CREATE TABLE IF NOT EXISTS inventory ("
                            + "item_id INT PRIMARY KEY, "
                            + "item_name VARCHAR(100) NOT NULL, "
                            + "price DECIMAL(10, 2) NOT NULL, "
                            + "stock INT NOT NULL"
                            + ")");
                    System.out.println("[3] Table 'inventory' created or verified.");

                    // 4. PreparedStatement によるパラメータ付き INSERT
                    String insertSql = "INSERT INTO inventory VALUES (?, ?, ?, ?)";
                    try (PreparedStatement pstmt = conn.prepareStatement(insertSql)) {
                        pstmt.setInt(1, 101);
                        pstmt.setString(2, "Gaming Laptop");
                        pstmt.setBigDecimal(3, new BigDecimal("1499.99"));
                        pstmt.setInt(4, 15);
                        pstmt.executeUpdate();

                        pstmt.setInt(1, 102);
                        pstmt.setString(2, "Wireless Mouse");
                        pstmt.setBigDecimal(3, new BigDecimal("49.50"));
                        pstmt.setInt(4, 50);
                        pstmt.executeUpdate();
                    }
                    System.out.println("[4] Inserted sample inventory items.");

                    // 5. ResultSet によるクエリ結果のフェッチとメタデータ確認
                    System.out.println("\n[5] Fetching query results (SELECT * FROM inventory):");
                    try (ResultSet rs = stmt.executeQuery("SELECT item_id, item_name, price, stock FROM inventory ORDER BY price DESC")) {
                        ResultSetMetaData meta = rs.getMetaData();
                        int colCount = meta.getColumnCount();

                        for (int i = 1; i <= colCount; i++) {
                            System.out.printf("%-18s", meta.getColumnName(i));
                        }
                        System.out.println("\n------------------------------------------------------------");

                        while (rs.next()) {
                            int id = rs.getInt("item_id");
                            String name = rs.getString("item_name");
                            BigDecimal price = rs.getBigDecimal("price");
                            int stock = rs.getInt("stock");
                            System.out.printf("%-18d %-18s $%-17s %-18d%n", id, name, price, stock);
                        }
                    }

                    // 6. トランザクション処理 (Commit & Rollback)
                    System.out.println("\n[6] Testing Transactions with JDBC:");
                    conn.setAutoCommit(false);

                    stmt.executeUpdate("UPDATE inventory SET stock = stock - 5 WHERE item_id = 101");
                    System.out.println("  Subtracted 5 laptops from stock inside transaction.");

                    // ロールバック
                    conn.rollback();
                    System.out.println("  Rolled back transaction.");

                    // 元に戻っていることを確認
                    try (ResultSet rs = stmt.executeQuery("SELECT stock FROM inventory WHERE item_id = 101")) {
                        if (rs.next()) {
                            System.out.println("  Stock after rollback is: " + rs.getInt(1) + " (Expected: 15)");
                        }
                    }

                    // コミット
                    stmt.executeUpdate("UPDATE inventory SET stock = stock + 10 WHERE item_id = 102");
                    conn.commit();
                    System.out.println("  Added 10 mice to stock and committed.");

                    conn.setAutoCommit(true);
                }

                System.out.println("\n[SUCCESS] Java JDBC demo finished cleanly!");
            }
        } catch (Exception e) {
            System.err.println("\n[ERROR] JDBC connection or execution failed: " + e.getMessage());
            System.err.println("Make sure the H2 PG-Wire server is running on port 5432!");
            System.err.println("You can start it with: cargo run -p demo-psql-server");
            e.printStackTrace();
        }
    }
}
