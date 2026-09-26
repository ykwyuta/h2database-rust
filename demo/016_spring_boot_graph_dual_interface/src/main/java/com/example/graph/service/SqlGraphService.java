package com.example.graph.service;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jdbc.core.JdbcTemplate;
import org.springframework.stereotype.Service;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * [Interface 2: SQL / JDBC 経由のグラフアクセス & ハイブリッド結合]
 * 標準の PostgreSQL JDBC ドライバと Spring JdbcTemplate を使用して、
 * 1. 仮想テーブル (graph_company_nodes, graph_company_edges) へのアクセス
 * 2. テーブル値関数 CYPHER('company', '...') による Cypher 実行とリレーショナルテーブルとの JOIN
 * を実演するサービスクラス。
 */
@Service
public class SqlGraphService {

    private static final Logger log = LoggerFactory.getLogger(SqlGraphService.class);
    private final JdbcTemplate jdbcTemplate;
    private final ObjectMapper objectMapper = new ObjectMapper();

    public SqlGraphService(JdbcTemplate jdbcTemplate) {
        this.jdbcTemplate = jdbcTemplate;
    }

    /**
     * 1. 仮想テーブル graph_company_nodes を直接 SQL SELECT で参照
     */
    public List<Map<String, Object>> queryVirtualNodes() {
        String sql = "SELECT id, labels, properties FROM graph_company_nodes ORDER BY id";
        return jdbcTemplate.query(sql, (rs, rowNum) -> {
            Map<String, Object> map = new HashMap<>();
            map.put("id", rs.getLong("id"));
            map.put("labels", rs.getString("labels"));
            String propsJson = rs.getString("properties");
            try {
                Map<String, Object> props = objectMapper.readValue(propsJson, new TypeReference<Map<String, Object>>() {});
                map.put("properties", props);
            } catch (Exception e) {
                map.put("properties", propsJson);
            }
            return map;
        });
    }

    /**
     * 2. 仮想テーブル graph_company_edges を直接 SQL SELECT で参照
     */
    public List<Map<String, Object>> queryVirtualEdges() {
        String sql = "SELECT id, src_id, dst_id, type, properties FROM graph_company_edges ORDER BY id";
        return jdbcTemplate.query(sql, (rs, rowNum) -> {
            Map<String, Object> map = new HashMap<>();
            map.put("id", rs.getLong("id"));
            map.put("src_id", rs.getLong("src_id"));
            map.put("dst_id", rs.getLong("dst_id"));
            map.put("type", rs.getString("type"));
            map.put("properties", rs.getString("properties"));
            return map;
        });
    }

    /**
     * 3. リレーショナルテーブル (departments) と Cypher TVF (CYPHER(...)) のハイブリッド結合クエリ
     */
    public List<Map<String, Object>> queryHybridRelationalAndGraphJoin() {
        String sql = "SELECT " +
                "    d.name AS dept_name, " +
                "    d.location AS dept_location, " +
                "    g.name AS emp_name, " +
                "    g.role AS emp_role " +
                "FROM departments d " +
                "JOIN CYPHER('company', 'MATCH (e:Employee) RETURN e.dept_id AS dept_id, e.name AS name, e.role AS role') " +
                "     AS g(dept_id, name, role) " +
                "  ON d.id = CAST(g.dept_id AS INT) " +
                "ORDER BY d.id, g.name";

        return jdbcTemplate.query(sql, (rs, rowNum) -> {
            Map<String, Object> map = new HashMap<>();
            map.put("dept_name", rs.getString("dept_name"));
            map.put("dept_location", rs.getString("dept_location"));
            map.put("emp_name", rs.getString("emp_name"));
            map.put("emp_role", rs.getString("emp_role"));
            return map;
        });
    }

    /**
     * 4. SQL 側から CYPHER(...) テーブル値関数を直接実行し、SQL の WHERE 条件でフィルタ
     */
    public List<Map<String, Object>> queryCypherTableFunctionWithSqlFilter(String projectFilter) {
        String sql = "SELECT " +
                "    g.initiator, " +
                "    g.project, " +
                "    g.collaborator " +
                "FROM CYPHER('company', 'MATCH (a:Employee)-[r:COLLABORATES]->(b:Employee) RETURN a.name AS initiator, r.project AS project, b.name AS collaborator') " +
                "     AS g(initiator, project, collaborator) " +
                "WHERE g.project = '" + projectFilter + "'";

        return jdbcTemplate.query(sql, (rs, rowNum) -> {
            Map<String, Object> map = new HashMap<>();
            map.put("initiator", rs.getString("initiator"));
            map.put("project", rs.getString("project"));
            map.put("collaborator", rs.getString("collaborator"));
            return map;
        });
    }

    /**
     * ノード総数の取得
     */
    public int countTotalNodes() {
        Integer count = jdbcTemplate.queryForObject("SELECT count(*) FROM graph_company_nodes", Integer.class);
        return count != null ? count : 0;
    }
}
