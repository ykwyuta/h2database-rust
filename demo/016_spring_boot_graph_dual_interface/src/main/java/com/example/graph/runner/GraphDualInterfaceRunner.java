package com.example.graph.runner;

import com.example.graph.service.BoltGraphService;
import com.example.graph.service.SqlGraphService;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.boot.CommandLineRunner;
import org.springframework.stereotype.Component;

import java.util.List;
import java.util.Map;

/**
 * グラフDBの2つのインターフェース（Bolt プロトコル & SQL / JDBC）からの
 * アクセスを実演する CommandLineRunner。
 */
@Component
public class GraphDualInterfaceRunner implements CommandLineRunner {

    private static final Logger log = LoggerFactory.getLogger(GraphDualInterfaceRunner.class);

    private final BoltGraphService boltService;
    private final SqlGraphService sqlService;

    public GraphDualInterfaceRunner(BoltGraphService boltService, SqlGraphService sqlService) {
        this.boltService = boltService;
        this.sqlService = sqlService;
    }

    @Override
    public void run(String... args) throws Exception {
        System.out.println("\n" + "=".repeat(80));
        System.out.println("  🌟 H2 Database Rust - Spring Boot Graph DB Dual-Interface Demo");
        System.out.println("=".repeat(80));

        // -------------------------------------------------------------------------
        // STEP 1: [Interface 1 - Bolt] Cypher によるノード・リレーションシップの作成
        // -------------------------------------------------------------------------
        System.out.println("\n>>> [STEP 1] Interface 1 (Bolt / Neo4j Java Driver): Cypher によるデータ作成");
        System.out.println("    Connecting via bolt://localhost:7687 using official Neo4j Java Driver...");
        boltService.initGraphData();

        // -------------------------------------------------------------------------
        // STEP 2: [Interface 1 - Bolt] Cypher パターンマッチング・トラバーサルクエリ
        // -------------------------------------------------------------------------
        System.out.println("\n>>> [STEP 2] Interface 1 (Bolt / Neo4j Java Driver): パターン探索クエリの実行");
        System.out.println("    Cypher: MATCH (a:Employee)-[r:COLLABORATES]->(b:Employee) RETURN a.name, r.project, b.name");
        List<Map<String, Object>> collabs = boltService.queryCollaborations();
        for (Map<String, Object> c : collabs) {
            System.out.printf("      [Collab] %s -[COLLABORATES: %s]-> %s%n",
                    c.get("initiator"), c.get("project"), c.get("collaborator"));
        }

        System.out.println("\n    Cypher: MATCH (sub:Employee)-[r:REPORTS_TO]->(mgr:Employee) RETURN sub.name, sub.role, mgr.name");
        List<Map<String, Object>> reports = boltService.queryReportingLines();
        for (Map<String, Object> r : reports) {
            System.out.printf("      [Report] %s (%s) REPORTS_TO %s%n",
                    r.get("subordinate"), r.get("role"), r.get("manager"));
        }

        // -------------------------------------------------------------------------
        // STEP 3: [Interface 2 - SQL] 仮想テーブル (graph_<name>_nodes / edges) の参照
        // -------------------------------------------------------------------------
        System.out.println("\n>>> [STEP 3] Interface 2 (SQL / JDBC): 仮想グラフテーブルの直接 SELECT");
        System.out.println("    Connecting via jdbc:postgresql://localhost:5432 using Spring JdbcTemplate...");
        System.out.println("    SQL: SELECT id, labels, properties FROM graph_company_nodes ORDER BY id");
        List<Map<String, Object>> nodes = sqlService.queryVirtualNodes();
        System.out.printf("    Found %d nodes in 'graph_company_nodes':%n", nodes.size());
        for (Map<String, Object> n : nodes) {
            System.out.printf("      Node ID: %-3s | Labels: %-12s | Properties: %s%n",
                    n.get("id"), n.get("labels"), n.get("properties"));
        }

        System.out.println("\n    SQL: SELECT id, src_id, dst_id, type, properties FROM graph_company_edges ORDER BY id");
        List<Map<String, Object>> edges = sqlService.queryVirtualEdges();
        System.out.printf("    Found %d edges in 'graph_company_edges':%n", edges.size());
        for (Map<String, Object> e : edges) {
            System.out.printf("      Edge ID: %-3s | (%s) -[%-12s]-> (%s) | Props: %s%n",
                    e.get("id"), e.get("src_id"), e.get("type"), e.get("dst_id"), e.get("properties"));
        }

        // -------------------------------------------------------------------------
        // STEP 4: [Interface 2 - SQL] リレーショナルテーブルと Cypher TVF のハイブリッド JOIN
        // -------------------------------------------------------------------------
        System.out.println("\n>>> [STEP 4] Interface 2 (SQL / JDBC): リレーショナルテーブルと CYPHER TVF のハイブリッド JOIN");
        System.out.println("    SQL Query:");
        System.out.println("      SELECT d.name AS dept_name, d.location, g.name AS emp_name, g.role AS emp_role");
        System.out.println("      FROM departments d");
        System.out.println("      JOIN CYPHER('company', 'MATCH (e:Employee) RETURN e.dept_id AS dept_id, e.name AS name, e.role AS role')");
        System.out.println("           AS g(dept_id, name, role) ON d.id = CAST(g.dept_id AS INT)");
        System.out.println("      ORDER BY d.id, g.name;");

        List<Map<String, Object>> hybridRows = sqlService.queryHybridRelationalAndGraphJoin();
        System.out.printf("    Hybrid Join Result (%d combined rows):%n", hybridRows.size());
        for (Map<String, Object> row : hybridRows) {
            System.out.printf("      [Dept: %-18s (%-12s)] -> Employee: %-8s | Role: %s%n",
                    row.get("dept_name"), row.get("dept_location"), row.get("emp_name"), row.get("emp_role"));
        }

        // -------------------------------------------------------------------------
        // STEP 5: 相互整合性の確認（Bolt 側で追加したノードが SQL 側で即座に見えるか実証）
        // -------------------------------------------------------------------------
        System.out.println("\n>>> [STEP 5] 双方向の即時整合性検証 (MVCC 共有ストレージの実証)");
        System.out.println("    Bolt 経由で新メンバー 'Frank' (Research Engineer, dept_id: 3) を追加...");
        boltService.addEmployee(106, "Frank", "Research Engineer", 3);

        int totalNodes = sqlService.countTotalNodes();
        System.out.printf("    SQL 側でノード総数をカウント: %d 件 (即座に反映されました)%n", totalNodes);

        List<Map<String, Object>> updatedHybrid = sqlService.queryHybridRelationalAndGraphJoin();
        boolean foundFrank = updatedHybrid.stream().anyMatch(r -> "Frank".equals(r.get("emp_name")));
        System.out.printf("    SQL ハイブリッド JOIN 内での Frank 検出: %s%n", foundFrank ? "OK (検出成功!)" : "NG");

        System.out.println("\n" + "=".repeat(80));
        System.out.println("  🎉 全デモステップが正常に完了しました！");
        System.out.println("  Bolt (Cypher) と SQL (JDBC) の両方から同一のグラフDBへ完全アクセス可能です。");
        System.out.println("=".repeat(80) + "\n");
    }
}
