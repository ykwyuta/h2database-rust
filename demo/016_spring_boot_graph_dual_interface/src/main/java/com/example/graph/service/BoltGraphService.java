package com.example.graph.service;

import org.neo4j.driver.Driver;
import org.neo4j.driver.Record;
import org.neo4j.driver.Result;
import org.neo4j.driver.Session;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.stereotype.Service;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * [Interface 1: Bolt プロトコル / Cypher 経由のグラフDBアクセス]
 * 公式 Neo4j Java Driver (bolt://) を使用して、Cypher によるノード・エッジの登録や
 * パス探索・トラバーサルクエリを実行するサービスクラス。
 */
@Service
public class BoltGraphService {

    private static final Logger log = LoggerFactory.getLogger(BoltGraphService.class);
    private final Driver driver;

    public BoltGraphService(Driver driver) {
        this.driver = driver;
    }

    /**
     * Bolt 経由で初期グラフデータ（社員ノード、レポートライン、共同プロジェクト関係）を作成
     */
    public void initGraphData() {
        log.info("==> [Bolt] 初期グラフデータを登録中 (CREATE)...");
        try (Session session = driver.session()) {
            // 社員ノードの作成
            session.run("CREATE (e:Employee {id: 101, name: 'Alice', role: 'Staff Engineer', dept_id: 1})");
            session.run("CREATE (e:Employee {id: 102, name: 'Bob', role: 'Principal Architect', dept_id: 1})");
            session.run("CREATE (e:Employee {id: 103, name: 'Charlie', role: 'Account Executive', dept_id: 2})");
            session.run("CREATE (e:Employee {id: 104, name: 'Diana', role: 'Sales Director', dept_id: 2})");
            session.run("CREATE (e:Employee {id: 105, name: 'Eve', role: 'AI Researcher', dept_id: 3})");

            // レポート関係 (REPORTS_TO) の接続
            session.run("MATCH (a:Employee {id: 101}), (b:Employee {id: 102}) " +
                    "CREATE (a)-[:REPORTS_TO {since: 2022}]->(b)");
            session.run("MATCH (c:Employee {id: 103}), (d:Employee {id: 104}) " +
                    "CREATE (c)-[:REPORTS_TO {since: 2021}]->(d)");

            // 部門横断コラボレーション関係 (COLLABORATES) の接続
            session.run("MATCH (a:Employee {id: 101}), (e:Employee {id: 105}) " +
                    "CREATE (a)-[:COLLABORATES {project: 'Graph-AI'}]->(e)");

            log.info("    [Bolt] 5個のノードと3本のエッジを正常に登録しました。");
        }
    }

    /**
     * Bolt 経由でコラボレーション関係を Cypher パターンマッチングで検索
     */
    public List<Map<String, Object>> queryCollaborations() {
        String cypher = "MATCH (a:Employee)-[r:COLLABORATES]->(b:Employee) " +
                "RETURN a.name AS initiator, r.project AS project, b.name AS collaborator";

        List<Map<String, Object>> results = new ArrayList<>();
        try (Session session = driver.session()) {
            Result result = session.run(cypher);
            while (result.hasNext()) {
                Record record = result.next();
                Map<String, Object> row = new HashMap<>();
                row.put("initiator", record.get("initiator").asString());
                row.put("project", record.get("project").asString());
                row.put("collaborator", record.get("collaborator").asString());
                results.add(row);
            }
        }
        return results;
    }

    /**
     * Bolt 経由で上司・部下のレポートラインを Cypher で検索
     */
    public List<Map<String, Object>> queryReportingLines() {
        String cypher = "MATCH (sub:Employee)-[r:REPORTS_TO]->(mgr:Employee) " +
                "RETURN sub.name AS subordinate, sub.role AS role, mgr.name AS manager";

        List<Map<String, Object>> results = new ArrayList<>();
        try (Session session = driver.session()) {
            Result result = session.run(cypher);
            while (result.hasNext()) {
                Record record = result.next();
                Map<String, Object> row = new HashMap<>();
                row.put("subordinate", record.get("subordinate").asString());
                row.put("role", record.get("role").asString());
                row.put("manager", record.get("manager").asString());
                results.add(row);
            }
        }
        return results;
    }

    /**
     * Bolt 経由で新規社員ノードを追加
     */
    public void addEmployee(int id, String name, String role, int deptId) {
        String cypher = String.format(
                "CREATE (e:Employee {id: %d, name: '%s', role: '%s', dept_id: %d})",
                id, name, role, deptId
        );
        try (Session session = driver.session()) {
            session.run(cypher);
        }
    }
}
