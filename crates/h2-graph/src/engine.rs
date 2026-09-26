use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use h2_mvstore::{MVStore, Transaction, TransactionStore};
use h2_types::{H2Error, H2Result};

use crate::model::{Edge, GraphResult, GraphStats, GraphValue, Node, Path};
use crate::parser::{
    BinaryOperator, CreateClause, DeleteClause, Direction, EdgePattern, Expr, MatchClause,
    MergeClause, NodePattern, Pattern, Query, QueryParser, ReturnClause, SetClause,
    SetItem, UnaryOperator,
};
use crate::store::GraphStore;

/// Cypher クエリ実行エンジン
pub struct GraphEngine {
    store: Arc<GraphStore>,
    mvstore: Arc<MVStore>,
    tx_store: Arc<TransactionStore>,
}

impl GraphEngine {
    pub fn new(mvstore: Arc<MVStore>, graph_name: impl Into<String>) -> H2Result<Self> {
        let tx_store = TransactionStore::new(mvstore.clone());
        let store = Arc::new(GraphStore::new(mvstore.clone(), graph_name)?);
        Ok(Self {
            store,
            mvstore,
            tx_store,
        })
    }

    pub fn store(&self) -> &GraphStore {
        &self.store
    }

    pub fn mvstore(&self) -> &Arc<MVStore> {
        &self.mvstore
    }

    pub fn execute(&self, cypher: &str) -> H2Result<GraphResult> {
        let empty_params = HashMap::new();
        self.execute_with_params(cypher, &empty_params)
    }

    pub fn execute_with_params(
        &self,
        cypher: &str,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<GraphResult> {
        let query = QueryParser::parse(cypher)?;
        let tx = self.tx_store.begin();
        let mut stats = GraphStats::default();

        let result = self.execute_query(&tx, &query, params, &mut stats);

        match result {
            Ok(mut res) => {
                tx.commit()?;
                res.stats = stats;
                Ok(res)
            }
            Err(e) => {
                tx.rollback()?;
                Err(e)
            }
        }
    }

    fn execute_query(
        &self,
        tx: &Transaction,
        query: &Query,
        params: &HashMap<String, GraphValue>,
        stats: &mut GraphStats,
    ) -> H2Result<GraphResult> {
        let mut rows: Vec<HashMap<String, GraphValue>> = vec![HashMap::new()];

        // 1. MATCH clauses
        for match_clause in &query.matches {
            rows = self.execute_match(tx, match_clause, rows, params)?;
            if rows.is_empty() && !match_clause.optional {
                // No matches, and not optional
                break;
            }
        }

        // If no matches clauses and creates/merges exist, ensure at least one row
        if rows.is_empty() && query.matches.is_empty() {
            rows.push(HashMap::new());
        }

        // 2. MERGE clauses
        for merge_clause in &query.merges {
            rows = self.execute_merge(tx, merge_clause, rows, params, stats)?;
        }

        // 3. CREATE clauses
        for create_clause in &query.creates {
            rows = self.execute_create(tx, create_clause, rows, params, stats)?;
        }

        // 4. SET clauses
        for set_clause in &query.sets {
            rows = self.execute_set(tx, set_clause, rows, params, stats)?;
        }

        // 5. DELETE clauses
        for delete_clause in &query.deletes {
            self.execute_delete(tx, delete_clause, &rows, stats)?;
        }

        // 6. RETURN clause
        if let Some(ref return_clause) = query.return_clause {
            self.execute_return(return_clause, rows, params)
        } else {
            Ok(GraphResult::empty())
        }
    }

    // ================= MATCH 実行 =================

    fn execute_match(
        &self,
        tx: &Transaction,
        match_clause: &MatchClause,
        current_rows: Vec<HashMap<String, GraphValue>>,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<Vec<HashMap<String, GraphValue>>> {
        let mut new_rows = Vec::new();

        for row in current_rows {
            let matched_subpaths = self.match_pattern(tx, &match_clause.pattern, &row, params)?;

            if matched_subpaths.is_empty() {
                if match_clause.optional {
                    let mut opt_row = row.clone();
                    if let Some(ref path_var) = match_clause.path_var {
                        opt_row.insert(path_var.clone(), GraphValue::Null);
                    }
                    if let Some(ref var) = match_clause.pattern.start.variable {
                        opt_row.insert(var.clone(), GraphValue::Null);
                    }
                    for (edge_pat, node_pat) in &match_clause.pattern.chain {
                        if let Some(ref evar) = edge_pat.variable {
                            opt_row.insert(evar.clone(), GraphValue::Null);
                        }
                        if let Some(ref nvar) = node_pat.variable {
                            opt_row.insert(nvar.clone(), GraphValue::Null);
                        }
                    }
                    new_rows.push(opt_row);
                }
            } else {
                for (bindings, path) in matched_subpaths {
                    let mut combined_row = row.clone();
                    for (k, v) in bindings {
                        combined_row.insert(k, v);
                    }
                    if let Some(ref path_var) = match_clause.path_var {
                        combined_row.insert(path_var.clone(), GraphValue::Path(path));
                    }

                    // Apply WHERE filter
                    if let Some(ref where_expr) = match_clause.where_clause {
                        let cond_val = self.eval_expr(where_expr, &combined_row, params)?;
                        if cond_val.as_bool().unwrap_or(false) {
                            new_rows.push(combined_row);
                        }
                    } else {
                        new_rows.push(combined_row);
                    }
                }
            }
        }

        Ok(new_rows)
    }

    fn match_pattern(
        &self,
        tx: &Transaction,
        pattern: &Pattern,
        current_row: &HashMap<String, GraphValue>,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<Vec<(HashMap<String, GraphValue>, Path)>> {
        // 1. Find candidate start nodes
        let start_nodes = self.find_candidate_nodes(tx, &pattern.start, current_row, params)?;
        let mut results = Vec::new();

        for start_node in start_nodes {
            let mut bindings = HashMap::new();
            if let Some(ref var) = pattern.start.variable {
                bindings.insert(var.clone(), GraphValue::Node(start_node.clone()));
            }

            let initial_path = Path::new(vec![start_node.clone()], vec![]);
            let mut active_paths = vec![(start_node, bindings, initial_path)];

            // 2. Expand chain steps
            for (edge_pat, node_pat) in &pattern.chain {
                let mut next_paths = Vec::new();

                for (curr_node, curr_bindings, curr_path) in active_paths {
                    let expansions = self.expand_step(
                        tx,
                        &curr_node,
                        edge_pat,
                        node_pat,
                        &curr_bindings,
                        &curr_path,
                        params,
                    )?;

                    for (next_node, step_bindings, updated_path) in expansions {
                        let mut merged = curr_bindings.clone();
                        for (k, v) in step_bindings {
                            merged.insert(k, v);
                        }
                        next_paths.push((next_node, merged, updated_path));
                    }
                }

                active_paths = next_paths;
                if active_paths.is_empty() {
                    break;
                }
            }

            for (_, b, p) in active_paths {
                results.push((b, p));
            }
        }

        Ok(results)
    }

    fn find_candidate_nodes(
        &self,
        tx: &Transaction,
        pattern: &NodePattern,
        current_row: &HashMap<String, GraphValue>,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<Vec<Node>> {
        // If variable is already bound in current_row, use that node
        if let Some(ref var) = pattern.variable {
            if let Some(GraphValue::Node(node)) = current_row.get(var) {
                if self.node_matches_pattern(node, pattern, current_row, params)? {
                    return Ok(vec![node.clone()]);
                } else {
                    return Ok(Vec::new());
                }
            }
        }

        // Try using label and property index if available
        let candidates = if let Some(first_label) = pattern.labels.first() {
            if let Some((prop_k, prop_expr)) = pattern.properties.first() {
                let prop_val = self.eval_expr(prop_expr, current_row, params)?.to_value();
                self.store
                    .find_nodes_by_label_and_prop(tx, first_label, prop_k, &prop_val)?
            } else {
                self.store.find_nodes_by_label(tx, first_label)?
            }
        } else {
            self.store.all_nodes(tx)?
        };

        // Filter candidates by remaining pattern requirements
        let mut matched = Vec::new();
        for node in candidates {
            if self.node_matches_pattern(&node, pattern, current_row, params)? {
                matched.push(node);
            }
        }

        Ok(matched)
    }

    fn node_matches_pattern(
        &self,
        node: &Node,
        pattern: &NodePattern,
        row: &HashMap<String, GraphValue>,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<bool> {
        // Check labels
        for label in &pattern.labels {
            if !node.has_label(label) {
                return Ok(false);
            }
        }

        // Check properties
        for (prop_name, prop_expr) in &pattern.properties {
            let expected_val = self.eval_expr(prop_expr, row, params)?.to_value();
            match node.get_property(prop_name) {
                Some(actual_val) if actual_val == &expected_val => {}
                _ => return Ok(false),
            }
        }

        Ok(true)
    }

    fn expand_step(
        &self,
        tx: &Transaction,
        start_node: &Node,
        edge_pat: &EdgePattern,
        node_pat: &NodePattern,
        curr_bindings: &HashMap<String, GraphValue>,
        curr_path: &Path,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<Vec<(Node, HashMap<String, GraphValue>, Path)>> {
        // Shortest path traversal
        if edge_pat.shortest_path {
            return self.expand_shortest_path(
                tx,
                start_node,
                edge_pat,
                node_pat,
                curr_bindings,
                curr_path,
                params,
            );
        }

        // Variable length traversal (*min..max)
        if let Some((min_hops, max_hops)) = edge_pat.var_length {
            return self.expand_var_length(
                tx,
                start_node,
                edge_pat,
                node_pat,
                min_hops.unwrap_or(1),
                max_hops.unwrap_or(15),
                curr_bindings,
                curr_path,
                params,
            );
        }

        // Single hop traversal (1-hop)
        let edges = self.get_matching_edges(tx, start_node.id, edge_pat)?;
        let mut results = Vec::new();

        for edge in edges {
            // Cypher relationship uniqueness per path
            if curr_path.edges.iter().any(|e| e.id == edge.id) {
                continue;
            }

            let next_node_id = if edge.src_id == start_node.id {
                edge.dst_id
            } else {
                edge.src_id
            };

            let next_node = match self.store.get_node(tx, next_node_id)? {
                Some(n) => n,
                None => continue,
            };

            let mut test_bindings = curr_bindings.clone();
            if let Some(ref evar) = edge_pat.variable {
                test_bindings.insert(evar.clone(), GraphValue::Edge(edge.clone()));
            }
            if let Some(ref nvar) = node_pat.variable {
                test_bindings.insert(nvar.clone(), GraphValue::Node(next_node.clone()));
            }

            if !self.node_matches_pattern(&next_node, node_pat, &test_bindings, params)? {
                continue;
            }

            let mut new_nodes = curr_path.nodes.clone();
            new_nodes.push(next_node.clone());
            let mut new_edges = curr_path.edges.clone();
            new_edges.push(edge.clone());
            let new_path = Path::new(new_nodes, new_edges);

            let mut step_bindings = HashMap::new();
            if let Some(ref evar) = edge_pat.variable {
                step_bindings.insert(evar.clone(), GraphValue::Edge(edge));
            }
            if let Some(ref nvar) = node_pat.variable {
                step_bindings.insert(nvar.clone(), GraphValue::Node(next_node.clone()));
            }

            results.push((next_node, step_bindings, new_path));
        }

        Ok(results)
    }

    fn expand_var_length(
        &self,
        tx: &Transaction,
        start_node: &Node,
        edge_pat: &EdgePattern,
        node_pat: &NodePattern,
        min_hops: usize,
        max_hops: usize,
        curr_bindings: &HashMap<String, GraphValue>,
        curr_path: &Path,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<Vec<(Node, HashMap<String, GraphValue>, Path)>> {
        let mut results = Vec::new();

        // Queue holds: (current_node, current_path, hops)
        let mut queue = VecDeque::new();
        queue.push_back((start_node.clone(), curr_path.clone(), 0));

        while let Some((curr_n, p, hops)) = queue.pop_front() {
            if hops >= max_hops {
                continue;
            }

            let edges = self.get_matching_edges(tx, curr_n.id, edge_pat)?;
            for edge in edges {
                if p.edges.iter().any(|e| e.id == edge.id) {
                    continue; // Relationship uniqueness
                }

                let next_id = if edge.src_id == curr_n.id {
                    edge.dst_id
                } else {
                    edge.src_id
                };

                let next_node = match self.store.get_node(tx, next_id)? {
                    Some(n) => n,
                    None => continue,
                };

                let mut next_nodes = p.nodes.clone();
                next_nodes.push(next_node.clone());
                let mut next_edges = p.edges.clone();
                next_edges.push(edge.clone());
                let next_path = Path::new(next_nodes, next_edges);
                let next_hops = hops + 1;

                if next_hops >= min_hops {
                    let mut test_bindings = curr_bindings.clone();
                    if let Some(ref nvar) = node_pat.variable {
                        test_bindings.insert(nvar.clone(), GraphValue::Node(next_node.clone()));
                    }

                    if self.node_matches_pattern(&next_node, node_pat, &test_bindings, params)? {
                        let mut step_bindings = HashMap::new();
                        if let Some(ref nvar) = node_pat.variable {
                            step_bindings.insert(nvar.clone(), GraphValue::Node(next_node.clone()));
                        }
                        if let Some(ref evar) = edge_pat.variable {
                            // In Cypher, variable-length path edge binding is a list of edges
                            let edge_list: Vec<GraphValue> = next_path
                                .edges
                                .iter()
                                .skip(curr_path.edges.len())
                                .map(|e| GraphValue::Edge(e.clone()))
                                .collect();
                            step_bindings.insert(evar.clone(), GraphValue::List(edge_list));
                        }
                        results.push((next_node.clone(), step_bindings, next_path.clone()));
                    }
                }

                if next_hops < max_hops {
                    queue.push_back((next_node, next_path, next_hops));
                }
            }
        }

        Ok(results)
    }

    fn expand_shortest_path(
        &self,
        tx: &Transaction,
        start_node: &Node,
        edge_pat: &EdgePattern,
        node_pat: &NodePattern,
        curr_bindings: &HashMap<String, GraphValue>,
        curr_path: &Path,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<Vec<(Node, HashMap<String, GraphValue>, Path)>> {
        // BFS for shortest path
        let mut queue = VecDeque::new();
        let mut visited_nodes = HashSet::new();

        queue.push_back((start_node.clone(), curr_path.clone()));
        visited_nodes.insert(start_node.id);

        while let Some((curr_n, p)) = queue.pop_front() {
            let edges = self.get_matching_edges(tx, curr_n.id, edge_pat)?;
            for edge in edges {
                let next_id = if edge.src_id == curr_n.id {
                    edge.dst_id
                } else {
                    edge.src_id
                };

                if visited_nodes.contains(&next_id) {
                    continue;
                }

                let next_node = match self.store.get_node(tx, next_id)? {
                    Some(n) => n,
                    None => continue,
                };

                let mut next_nodes = p.nodes.clone();
                next_nodes.push(next_node.clone());
                let mut next_edges = p.edges.clone();
                next_edges.push(edge.clone());
                let next_path = Path::new(next_nodes, next_edges);

                let mut test_bindings = curr_bindings.clone();
                if let Some(ref nvar) = node_pat.variable {
                    test_bindings.insert(nvar.clone(), GraphValue::Node(next_node.clone()));
                }

                if self.node_matches_pattern(&next_node, node_pat, &test_bindings, params)? {
                    // Found shortest path!
                    let mut step_bindings = HashMap::new();
                    if let Some(ref nvar) = node_pat.variable {
                        step_bindings.insert(nvar.clone(), GraphValue::Node(next_node.clone()));
                    }
                    if let Some(ref evar) = edge_pat.variable {
                        let edge_list: Vec<GraphValue> = next_path
                            .edges
                            .iter()
                            .skip(curr_path.edges.len())
                            .map(|e| GraphValue::Edge(e.clone()))
                            .collect();
                        step_bindings.insert(evar.clone(), GraphValue::List(edge_list));
                    }
                    return Ok(vec![(next_node, step_bindings, next_path)]);
                }

                visited_nodes.insert(next_id);
                queue.push_back((next_node, next_path));
            }
        }

        Ok(Vec::new())
    }

    fn get_matching_edges(
        &self,
        tx: &Transaction,
        node_id: u64,
        edge_pat: &EdgePattern,
    ) -> H2Result<Vec<Edge>> {
        let type_filter = edge_pat.edge_types.first().map(|s| s.as_str());

        let mut edges = match edge_pat.direction {
            Direction::Outgoing => self.store.get_out_edges(tx, node_id, type_filter)?,
            Direction::Incoming => self.store.get_in_edges(tx, node_id, type_filter)?,
            Direction::Both => {
                let mut out = self.store.get_out_edges(tx, node_id, type_filter)?;
                let mut inc = self.store.get_in_edges(tx, node_id, type_filter)?;
                out.append(&mut inc);
                // Deduplicate by edge.id in case of self-loops
                out.sort_by_key(|e| e.id);
                out.dedup_by_key(|e| e.id);
                out
            }
        };

        // If multiple types specified in edge pattern (e.g., :A|B)
        if edge_pat.edge_types.len() > 1 {
            edges.retain(|e| {
                edge_pat
                    .edge_types
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(&e.edge_type))
            });
        }

        // Filter by properties if specified
        if !edge_pat.properties.is_empty() {
            edges.retain(|e| {
                edge_pat.properties.iter().all(|(k, expr)| {
                    if let Ok(expected) = self.eval_expr(expr, &HashMap::new(), &HashMap::new()) {
                        e.get_property(k) == Some(&expected.to_value())
                    } else {
                        false
                    }
                })
            });
        }

        Ok(edges)
    }

    // ================= CREATE 実行 =================

    fn execute_create(
        &self,
        tx: &Transaction,
        clause: &CreateClause,
        rows: Vec<HashMap<String, GraphValue>>,
        params: &HashMap<String, GraphValue>,
        stats: &mut GraphStats,
    ) -> H2Result<Vec<HashMap<String, GraphValue>>> {
        let mut new_rows = Vec::new();

        for row in rows {
            let mut updated_row = row.clone();

            // 1. Create or get start node
            let start_node =
                self.instantiate_node(tx, &clause.pattern.start, &mut updated_row, params, stats)?;

            let mut prev_node = start_node;

            // 2. Create chain of edges and target nodes
            for (edge_pat, node_pat) in &clause.pattern.chain {
                let target_node =
                    self.instantiate_node(tx, node_pat, &mut updated_row, params, stats)?;

                let edge_type = edge_pat
                    .edge_types
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "RELATED_TO".to_string());

                let mut edge_props = HashMap::new();
                for (k, expr) in &edge_pat.properties {
                    let v = self.eval_expr(expr, &updated_row, params)?;
                    edge_props.insert(k.clone(), v.to_value());
                }

                let (src_id, dst_id) = match edge_pat.direction {
                    Direction::Outgoing | Direction::Both => (prev_node.id, target_node.id),
                    Direction::Incoming => (target_node.id, prev_node.id),
                };

                let edge = self.store.create_edge(
                    tx,
                    edge_type,
                    src_id,
                    dst_id,
                    edge_props.clone(),
                )?;
                stats.relationships_created += 1;
                stats.properties_set += edge_props.len();

                if let Some(ref evar) = edge_pat.variable {
                    updated_row.insert(evar.clone(), GraphValue::Edge(edge));
                }

                prev_node = target_node;
            }

            new_rows.push(updated_row);
        }

        Ok(new_rows)
    }

    fn instantiate_node(
        &self,
        tx: &Transaction,
        pattern: &NodePattern,
        row: &mut HashMap<String, GraphValue>,
        params: &HashMap<String, GraphValue>,
        stats: &mut GraphStats,
    ) -> H2Result<Node> {
        // If variable is already bound to a Node, reuse it
        if let Some(ref var) = pattern.variable {
            if let Some(GraphValue::Node(existing)) = row.get(var) {
                return Ok(existing.clone());
            }
        }

        let mut props = HashMap::new();
        for (k, expr) in &pattern.properties {
            let v = self.eval_expr(expr, row, params)?;
            props.insert(k.clone(), v.to_value());
        }

        let node = self
            .store
            .create_node(tx, pattern.labels.clone(), props.clone())?;
        stats.nodes_created += 1;
        stats.labels_added += pattern.labels.len();
        stats.properties_set += props.len();

        if let Some(ref var) = pattern.variable {
            row.insert(var.clone(), GraphValue::Node(node.clone()));
        }

        Ok(node)
    }

    // ================= MERGE 実行 =================

    fn execute_merge(
        &self,
        tx: &Transaction,
        clause: &MergeClause,
        rows: Vec<HashMap<String, GraphValue>>,
        params: &HashMap<String, GraphValue>,
        stats: &mut GraphStats,
    ) -> H2Result<Vec<HashMap<String, GraphValue>>> {
        let mut new_rows = Vec::new();

        for row in rows {
            let matches = self.match_pattern(tx, &clause.pattern, &row, params)?;

            if let Some((first_bindings, _)) = matches.into_iter().next() {
                // MATCHED! Apply ON MATCH SET
                let mut matched_row = row.clone();
                for (k, v) in first_bindings {
                    matched_row.insert(k, v);
                }

                for set_item in &clause.on_match_sets {
                    self.apply_set_item(tx, set_item, &mut matched_row, params, stats)?;
                }

                new_rows.push(matched_row);
            } else {
                // NOT MATCHED! Create pattern and apply ON CREATE SET
                let create_clause = CreateClause {
                    pattern: clause.pattern.clone(),
                };
                let created_rows =
                    self.execute_create(tx, &create_clause, vec![row], params, stats)?;

                for mut created_row in created_rows {
                    for set_item in &clause.on_create_sets {
                        self.apply_set_item(tx, set_item, &mut created_row, params, stats)?;
                    }
                    new_rows.push(created_row);
                }
            }
        }

        Ok(new_rows)
    }

    // ================= SET 実行 =================

    fn execute_set(
        &self,
        tx: &Transaction,
        clause: &SetClause,
        rows: Vec<HashMap<String, GraphValue>>,
        params: &HashMap<String, GraphValue>,
        stats: &mut GraphStats,
    ) -> H2Result<Vec<HashMap<String, GraphValue>>> {
        let mut new_rows = Vec::new();

        for row in rows {
            let mut updated_row = row;
            for item in &clause.items {
                self.apply_set_item(tx, item, &mut updated_row, params, stats)?;
            }
            new_rows.push(updated_row);
        }

        Ok(new_rows)
    }

    fn apply_set_item(
        &self,
        tx: &Transaction,
        item: &SetItem,
        row: &mut HashMap<String, GraphValue>,
        params: &HashMap<String, GraphValue>,
        stats: &mut GraphStats,
    ) -> H2Result<()> {
        match item {
            SetItem::Property {
                variable,
                property,
                value,
            } => {
                let new_val = self.eval_expr(value, row, params)?;
                if let Some(gv) = row.get_mut(variable) {
                    match gv {
                        GraphValue::Node(ref mut node) => {
                            node.properties
                                .insert(property.clone(), new_val.to_value());
                            self.store.save_node(tx, node)?;
                            stats.properties_set += 1;
                        }
                        GraphValue::Edge(ref mut edge) => {
                            edge.properties
                                .insert(property.clone(), new_val.to_value());
                            self.store.save_edge(tx, edge)?;
                            stats.properties_set += 1;
                        }
                        _ => {
                            return Err(H2Error::Execution(format!(
                                "Variable '{variable}' is not a Node or Edge"
                            )))
                        }
                    }
                }
            }
            SetItem::Label { variable, label } => {
                if let Some(GraphValue::Node(ref mut node)) = row.get_mut(variable) {
                    if !node.has_label(label) {
                        node.labels.push(label.clone());
                        self.store.save_node(tx, node)?;
                        stats.labels_added += 1;
                    }
                }
            }
        }
        Ok(())
    }

    // ================= DELETE 実行 =================

    fn execute_delete(
        &self,
        tx: &Transaction,
        clause: &DeleteClause,
        rows: &[HashMap<String, GraphValue>],
        stats: &mut GraphStats,
    ) -> H2Result<()> {
        let mut deleted_node_ids = HashSet::new();
        let mut deleted_edge_ids = HashSet::new();

        for row in rows {
            for var in &clause.variables {
                if let Some(val) = row.get(var) {
                    match val {
                        GraphValue::Node(node) => {
                            if deleted_node_ids.insert(node.id) {
                                if self.store.delete_node(tx, node.id, clause.detach)? {
                                    stats.nodes_deleted += 1;
                                }
                            }
                        }
                        GraphValue::Edge(edge) => {
                            if deleted_edge_ids.insert(edge.id) {
                                if self.store.delete_edge(tx, edge.id)? {
                                    stats.relationships_deleted += 1;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }

    // ================= RETURN 実行 =================

    fn execute_return(
        &self,
        clause: &ReturnClause,
        rows: Vec<HashMap<String, GraphValue>>,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<GraphResult> {
        let mut columns = Vec::new();
        for item in &clause.items {
            let col_name = item.alias.clone().unwrap_or_else(|| self.expr_to_name(&item.expr));
            columns.push(col_name);
        }

        let has_aggregates = clause
            .items
            .iter()
            .any(|item| self.expr_has_aggregate(&item.expr));

        let mut projected_rows = if has_aggregates {
            self.execute_aggregated_return(clause, rows, params)?
        } else {
            let mut out = Vec::new();
            for row in &rows {
                let mut projected_row = Vec::with_capacity(clause.items.len());
                for item in &clause.items {
                    let val = self.eval_expr(&item.expr, row, params)?;
                    projected_row.push(val);
                }
                out.push(projected_row);
            }
            out
        };

        // DISTINCT
        if clause.distinct {
            let mut seen = HashSet::new();
            projected_rows.retain(|r| seen.insert(format!("{:?}", r)));
        }

        // ORDER BY
        if !clause.order_by.is_empty() {
            // Sort rows based on ORDER BY expressions
            projected_rows.sort_by(|a, b| {
                // Default comparison
                for (expr, asc) in &clause.order_by {
                    let name = self.expr_to_name(expr);
                    let idx = columns.iter().position(|c| c == &name).unwrap_or(0);
                    let val_a = a.get(idx).unwrap_or(&GraphValue::Null);
                    let val_b = b.get(idx).unwrap_or(&GraphValue::Null);

                    let cmp = self.compare_graph_values(val_a, val_b);
                    if cmp != std::cmp::Ordering::Equal {
                        return if *asc { cmp } else { cmp.reverse() };
                    }
                }
                std::cmp::Ordering::Equal
            });
        }

        // SKIP
        if let Some(skip) = clause.skip {
            if skip < projected_rows.len() {
                projected_rows = projected_rows.split_off(skip);
            } else {
                projected_rows.clear();
            }
        }

        // LIMIT
        if let Some(limit) = clause.limit {
            projected_rows.truncate(limit);
        }

        Ok(GraphResult::with_columns_and_rows(columns, projected_rows))
    }

    fn execute_aggregated_return(
        &self,
        clause: &ReturnClause,
        rows: Vec<HashMap<String, GraphValue>>,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<Vec<Vec<GraphValue>>> {
        // Grouping keys vs Aggregate expressions
        let mut group_indices = Vec::new();
        let mut agg_indices = Vec::new();

        for (i, item) in clause.items.iter().enumerate() {
            if self.expr_has_aggregate(&item.expr) {
                agg_indices.push(i);
            } else {
                group_indices.push(i);
            }
        }

        // Group rows: Map<GroupKey, Vec<Row>>
        let mut groups: HashMap<String, (Vec<GraphValue>, Vec<&HashMap<String, GraphValue>>)> =
            HashMap::new();

        for row in &rows {
            let mut group_vals = Vec::new();
            for &idx in &group_indices {
                let val = self.eval_expr(&clause.items[idx].expr, row, params)?;
                group_vals.push(val);
            }

            let key_str = format!("{:?}", group_vals);
            let entry = groups.entry(key_str).or_insert((group_vals, Vec::new()));
            entry.1.push(row);
        }

        // If rows were empty and we have only aggregates (e.g. RETURN count(*))
        if groups.is_empty() && group_indices.is_empty() {
            groups.insert("empty".to_string(), (Vec::new(), Vec::new()));
        }

        let mut results = Vec::new();
        for (_, (group_vals, group_rows)) in groups {
            let mut result_row = vec![GraphValue::Null; clause.items.len()];

            // Place group values
            for (i, &idx) in group_indices.iter().enumerate() {
                result_row[idx] = group_vals[i].clone();
            }

            // Compute aggregates
            for &idx in &agg_indices {
                let agg_val =
                    self.eval_aggregate_expr(&clause.items[idx].expr, &group_rows, params)?;
                result_row[idx] = agg_val;
            }

            results.push(result_row);
        }

        Ok(results)
    }

    fn expr_has_aggregate(&self, expr: &Expr) -> bool {
        match expr {
            Expr::FunctionCall { name, .. } => matches!(
                name.to_lowercase().as_str(),
                "count" | "sum" | "avg" | "min" | "max" | "collect"
            ),
            Expr::BinaryOp { left, right, .. } => {
                self.expr_has_aggregate(left) || self.expr_has_aggregate(right)
            }
            Expr::UnaryOp { expr, .. } => self.expr_has_aggregate(expr),
            _ => false,
        }
    }

    fn eval_aggregate_expr(
        &self,
        expr: &Expr,
        rows: &[&HashMap<String, GraphValue>],
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<GraphValue> {
        match expr {
            Expr::FunctionCall { name, args } => match name.to_lowercase().as_str() {
                "count" => {
                    if let Some(first_arg) = args.first() {
                        if let Expr::Literal(GraphValue::String(s)) = first_arg {
                            if s == "*" {
                                return Ok(GraphValue::Integer(rows.len() as i64));
                            }
                        }
                        let mut cnt = 0;
                        for r in rows {
                            let v = self.eval_expr(first_arg, r, params)?;
                            if !v.is_null() {
                                cnt += 1;
                            }
                        }
                        Ok(GraphValue::Integer(cnt))
                    } else {
                        Ok(GraphValue::Integer(rows.len() as i64))
                    }
                }
                "sum" => {
                    let first_arg = args.first().ok_or_else(|| {
                        H2Error::Execution("sum() requires an argument".to_string())
                    })?;
                    let mut sum_f = 0.0;
                    let mut is_float = false;
                    for r in rows {
                        let v = self.eval_expr(first_arg, r, params)?;
                        match v {
                            GraphValue::Integer(i) => sum_f += i as f64,
                            GraphValue::Float(f) => {
                                sum_f += f;
                                is_float = true;
                            }
                            _ => {}
                        }
                    }
                    if is_float {
                        Ok(GraphValue::Float(sum_f))
                    } else {
                        Ok(GraphValue::Integer(sum_f as i64))
                    }
                }
                "avg" => {
                    let first_arg = args.first().ok_or_else(|| {
                        H2Error::Execution("avg() requires an argument".to_string())
                    })?;
                    let mut sum_f = 0.0;
                    let mut cnt = 0;
                    for r in rows {
                        let v = self.eval_expr(first_arg, r, params)?;
                        match v {
                            GraphValue::Integer(i) => {
                                sum_f += i as f64;
                                cnt += 1;
                            }
                            GraphValue::Float(f) => {
                                sum_f += f;
                                cnt += 1;
                            }
                            _ => {}
                        }
                    }
                    if cnt == 0 {
                        Ok(GraphValue::Null)
                    } else {
                        Ok(GraphValue::Float(sum_f / cnt as f64))
                    }
                }
                "min" => {
                    let first_arg = args.first().ok_or_else(|| {
                        H2Error::Execution("min() requires an argument".to_string())
                    })?;
                    let mut min_val: Option<GraphValue> = None;
                    for r in rows {
                        let v = self.eval_expr(first_arg, r, params)?;
                        if !v.is_null() {
                            min_val = match min_val {
                                None => Some(v),
                                Some(curr) => {
                                    if self.compare_graph_values(&v, &curr)
                                        == std::cmp::Ordering::Less
                                    {
                                        Some(v)
                                    } else {
                                        Some(curr)
                                    }
                                }
                            };
                        }
                    }
                    Ok(min_val.unwrap_or(GraphValue::Null))
                }
                "max" => {
                    let first_arg = args.first().ok_or_else(|| {
                        H2Error::Execution("max() requires an argument".to_string())
                    })?;
                    let mut max_val: Option<GraphValue> = None;
                    for r in rows {
                        let v = self.eval_expr(first_arg, r, params)?;
                        if !v.is_null() {
                            max_val = match max_val {
                                None => Some(v),
                                Some(curr) => {
                                    if self.compare_graph_values(&v, &curr)
                                        == std::cmp::Ordering::Greater
                                    {
                                        Some(v)
                                    } else {
                                        Some(curr)
                                    }
                                }
                            };
                        }
                    }
                    Ok(max_val.unwrap_or(GraphValue::Null))
                }
                "collect" => {
                    let first_arg = args.first().ok_or_else(|| {
                        H2Error::Execution("collect() requires an argument".to_string())
                    })?;
                    let mut list = Vec::new();
                    for r in rows {
                        let v = self.eval_expr(first_arg, r, params)?;
                        if !v.is_null() {
                            list.push(v);
                        }
                    }
                    Ok(GraphValue::List(list))
                }
                other => Err(H2Error::Execution(format!(
                    "Unknown aggregate function: {other}"
                ))),
            },
            other => Err(H2Error::Execution(format!(
                "Unsupported aggregation expression: {:?}",
                other
            ))),
        }
    }

    // ================= 式評価 (Expression Evaluator) =================

    pub fn eval_expr(
        &self,
        expr: &Expr,
        row: &HashMap<String, GraphValue>,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<GraphValue> {
        match expr {
            Expr::Literal(val) => Ok(val.clone()),
            Expr::Parameter(name) => Ok(params.get(name).cloned().unwrap_or(GraphValue::Null)),
            Expr::Variable(name) => Ok(row.get(name).cloned().unwrap_or(GraphValue::Null)),
            Expr::PropertyAccess(var, prop) => {
                if let Some(val) = row.get(var) {
                    match val {
                        GraphValue::Node(node) => {
                            if let Some(pv) = node.get_property(prop) {
                                Ok(GraphValue::from_value(pv))
                            } else {
                                Ok(GraphValue::Null)
                            }
                        }
                        GraphValue::Edge(edge) => {
                            if let Some(pv) = edge.get_property(prop) {
                                Ok(GraphValue::from_value(pv))
                            } else {
                                Ok(GraphValue::Null)
                            }
                        }
                        GraphValue::Map(m) => {
                            Ok(m.get(prop).cloned().unwrap_or(GraphValue::Null))
                        }
                        _ => Ok(GraphValue::Null),
                    }
                } else {
                    Ok(GraphValue::Null)
                }
            }
            Expr::BinaryOp { left, op, right } => {
                let l = self.eval_expr(left, row, params)?;
                let r = self.eval_expr(right, row, params)?;
                self.eval_binary_op(&l, *op, &r)
            }
            Expr::UnaryOp { op, expr } => {
                let v = self.eval_expr(expr, row, params)?;
                match op {
                    UnaryOperator::Not => Ok(GraphValue::Boolean(!v.as_bool().unwrap_or(false))),
                    UnaryOperator::Neg => match v {
                        GraphValue::Integer(i) => Ok(GraphValue::Integer(-i)),
                        GraphValue::Float(f) => Ok(GraphValue::Float(-f)),
                        _ => Ok(GraphValue::Null),
                    },
                }
            }
            Expr::FunctionCall { name, args } => self.eval_function(name, args, row, params),
            Expr::List(exprs) => {
                let mut list = Vec::with_capacity(exprs.len());
                for e in exprs {
                    list.push(self.eval_expr(e, row, params)?);
                }
                Ok(GraphValue::List(list))
            }
        }
    }

    fn eval_binary_op(
        &self,
        left: &GraphValue,
        op: BinaryOperator,
        right: &GraphValue,
    ) -> H2Result<GraphValue> {
        match op {
            BinaryOperator::Eq => Ok(GraphValue::Boolean(left == right)),
            BinaryOperator::Neq => Ok(GraphValue::Boolean(left != right)),
            BinaryOperator::Lt => Ok(GraphValue::Boolean(
                self.compare_graph_values(left, right) == std::cmp::Ordering::Less,
            )),
            BinaryOperator::Lte => Ok(GraphValue::Boolean(matches!(
                self.compare_graph_values(left, right),
                std::cmp::Ordering::Less | std::cmp::Ordering::Equal
            ))),
            BinaryOperator::Gt => Ok(GraphValue::Boolean(
                self.compare_graph_values(left, right) == std::cmp::Ordering::Greater,
            )),
            BinaryOperator::Gte => Ok(GraphValue::Boolean(matches!(
                self.compare_graph_values(left, right),
                std::cmp::Ordering::Greater | std::cmp::Ordering::Equal
            ))),
            BinaryOperator::And => Ok(GraphValue::Boolean(
                left.as_bool().unwrap_or(false) && right.as_bool().unwrap_or(false),
            )),
            BinaryOperator::Or => Ok(GraphValue::Boolean(
                left.as_bool().unwrap_or(false) || right.as_bool().unwrap_or(false),
            )),
            BinaryOperator::Add => match (left, right) {
                (GraphValue::Integer(a), GraphValue::Integer(b)) => Ok(GraphValue::Integer(a + b)),
                (GraphValue::Float(a), GraphValue::Float(b)) => Ok(GraphValue::Float(a + b)),
                (GraphValue::Integer(a), GraphValue::Float(b)) => {
                    Ok(GraphValue::Float(*a as f64 + b))
                }
                (GraphValue::Float(a), GraphValue::Integer(b)) => {
                    Ok(GraphValue::Float(a + *b as f64))
                }
                (GraphValue::String(a), GraphValue::String(b)) => {
                    Ok(GraphValue::String(format!("{a}{b}")))
                }
                _ => Ok(GraphValue::Null),
            },
            BinaryOperator::Sub => match (left, right) {
                (GraphValue::Integer(a), GraphValue::Integer(b)) => Ok(GraphValue::Integer(a - b)),
                (GraphValue::Float(a), GraphValue::Float(b)) => Ok(GraphValue::Float(a - b)),
                (GraphValue::Integer(a), GraphValue::Float(b)) => {
                    Ok(GraphValue::Float(*a as f64 - b))
                }
                (GraphValue::Float(a), GraphValue::Integer(b)) => {
                    Ok(GraphValue::Float(a - *b as f64))
                }
                _ => Ok(GraphValue::Null),
            },
            BinaryOperator::Mul => match (left, right) {
                (GraphValue::Integer(a), GraphValue::Integer(b)) => Ok(GraphValue::Integer(a * b)),
                (GraphValue::Float(a), GraphValue::Float(b)) => Ok(GraphValue::Float(a * b)),
                (GraphValue::Integer(a), GraphValue::Float(b)) => {
                    Ok(GraphValue::Float(*a as f64 * b))
                }
                (GraphValue::Float(a), GraphValue::Integer(b)) => {
                    Ok(GraphValue::Float(a * *b as f64))
                }
                _ => Ok(GraphValue::Null),
            },
            BinaryOperator::Div => match (left, right) {
                (GraphValue::Integer(a), GraphValue::Integer(b)) if *b != 0 => {
                    Ok(GraphValue::Integer(a / b))
                }
                (GraphValue::Float(a), GraphValue::Float(b)) if *b != 0.0 => {
                    Ok(GraphValue::Float(a / b))
                }
                (GraphValue::Integer(a), GraphValue::Float(b)) if *b != 0.0 => {
                    Ok(GraphValue::Float(*a as f64 / b))
                }
                (GraphValue::Float(a), GraphValue::Integer(b)) if *b != 0 => {
                    Ok(GraphValue::Float(a / *b as f64))
                }
                _ => Ok(GraphValue::Null),
            },
        }
    }

    fn eval_function(
        &self,
        name: &str,
        args: &[Expr],
        row: &HashMap<String, GraphValue>,
        params: &HashMap<String, GraphValue>,
    ) -> H2Result<GraphValue> {
        match name.to_lowercase().as_str() {
            "id" => {
                let first = args.first().ok_or_else(|| {
                    H2Error::Execution("id() requires 1 argument".to_string())
                })?;
                let val = self.eval_expr(first, row, params)?;
                match val {
                    GraphValue::Node(n) => Ok(GraphValue::Integer(n.id as i64)),
                    GraphValue::Edge(e) => Ok(GraphValue::Integer(e.id as i64)),
                    _ => Ok(GraphValue::Null),
                }
            }
            "labels" => {
                let first = args.first().ok_or_else(|| {
                    H2Error::Execution("labels() requires 1 argument".to_string())
                })?;
                let val = self.eval_expr(first, row, params)?;
                match val {
                    GraphValue::Node(n) => {
                        let list = n
                            .labels
                            .into_iter()
                            .map(GraphValue::String)
                            .collect();
                        Ok(GraphValue::List(list))
                    }
                    _ => Ok(GraphValue::Null),
                }
            }
            "type" => {
                let first = args.first().ok_or_else(|| {
                    H2Error::Execution("type() requires 1 argument".to_string())
                })?;
                let val = self.eval_expr(first, row, params)?;
                match val {
                    GraphValue::Edge(e) => Ok(GraphValue::String(e.edge_type)),
                    _ => Ok(GraphValue::Null),
                }
            }
            "length" => {
                let first = args.first().ok_or_else(|| {
                    H2Error::Execution("length() requires 1 argument".to_string())
                })?;
                let val = self.eval_expr(first, row, params)?;
                match val {
                    GraphValue::Path(p) => Ok(GraphValue::Integer(p.len() as i64)),
                    GraphValue::List(l) => Ok(GraphValue::Integer(l.len() as i64)),
                    GraphValue::String(s) => Ok(GraphValue::Integer(s.len() as i64)),
                    _ => Ok(GraphValue::Null),
                }
            }
            "nodes" => {
                let first = args.first().ok_or_else(|| {
                    H2Error::Execution("nodes() requires 1 argument".to_string())
                })?;
                let val = self.eval_expr(first, row, params)?;
                match val {
                    GraphValue::Path(p) => {
                        let list = p.nodes.into_iter().map(GraphValue::Node).collect();
                        Ok(GraphValue::List(list))
                    }
                    _ => Ok(GraphValue::Null),
                }
            }
            "relationships" => {
                let first = args.first().ok_or_else(|| {
                    H2Error::Execution("relationships() requires 1 argument".to_string())
                })?;
                let val = self.eval_expr(first, row, params)?;
                match val {
                    GraphValue::Path(p) => {
                        let list = p.edges.into_iter().map(GraphValue::Edge).collect();
                        Ok(GraphValue::List(list))
                    }
                    _ => Ok(GraphValue::Null),
                }
            }
            "timestamp" => {
                let ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                Ok(GraphValue::Integer(ms))
            }
            "coalesce" => {
                for arg in args {
                    let v = self.eval_expr(arg, row, params)?;
                    if !v.is_null() {
                        return Ok(v);
                    }
                }
                Ok(GraphValue::Null)
            }
            "tointeger" => {
                let first = args.first().ok_or_else(|| {
                    H2Error::Execution("toInteger() requires 1 argument".to_string())
                })?;
                let val = self.eval_expr(first, row, params)?;
                match val {
                    GraphValue::Integer(i) => Ok(GraphValue::Integer(i)),
                    GraphValue::Float(f) => Ok(GraphValue::Integer(f as i64)),
                    GraphValue::String(s) => s
                        .parse::<i64>()
                        .map(GraphValue::Integer)
                        .map_err(|_| H2Error::Execution(format!("Cannot parse '{s}' as integer"))),
                    _ => Ok(GraphValue::Null),
                }
            }
            "tofloat" => {
                let first = args.first().ok_or_else(|| {
                    H2Error::Execution("toFloat() requires 1 argument".to_string())
                })?;
                let val = self.eval_expr(first, row, params)?;
                match val {
                    GraphValue::Float(f) => Ok(GraphValue::Float(f)),
                    GraphValue::Integer(i) => Ok(GraphValue::Float(i as f64)),
                    GraphValue::String(s) => s
                        .parse::<f64>()
                        .map(GraphValue::Float)
                        .map_err(|_| H2Error::Execution(format!("Cannot parse '{s}' as float"))),
                    _ => Ok(GraphValue::Null),
                }
            }
            "tostring" => {
                let first = args.first().ok_or_else(|| {
                    H2Error::Execution("toString() requires 1 argument".to_string())
                })?;
                let val = self.eval_expr(first, row, params)?;
                match val {
                    GraphValue::String(s) => Ok(GraphValue::String(s)),
                    GraphValue::Integer(i) => Ok(GraphValue::String(i.to_string())),
                    GraphValue::Float(f) => Ok(GraphValue::String(f.to_string())),
                    GraphValue::Boolean(b) => Ok(GraphValue::String(b.to_string())),
                    other => Ok(GraphValue::String(format!("{other}"))),
                }
            }
            "toboolean" => {
                let first = args.first().ok_or_else(|| {
                    H2Error::Execution("toBoolean() requires 1 argument".to_string())
                })?;
                let val = self.eval_expr(first, row, params)?;
                match val {
                    GraphValue::Boolean(b) => Ok(GraphValue::Boolean(b)),
                    GraphValue::String(s) => match s.to_lowercase().as_str() {
                        "true" => Ok(GraphValue::Boolean(true)),
                        "false" => Ok(GraphValue::Boolean(false)),
                        _ => Ok(GraphValue::Null),
                    },
                    _ => Ok(GraphValue::Null),
                }
            }
            other => Err(H2Error::Execution(format!("Unknown function: {other}"))),
        }
    }

    fn compare_graph_values(&self, a: &GraphValue, b: &GraphValue) -> std::cmp::Ordering {
        match (a, b) {
            (GraphValue::Null, GraphValue::Null) => std::cmp::Ordering::Equal,
            (GraphValue::Null, _) => std::cmp::Ordering::Less,
            (_, GraphValue::Null) => std::cmp::Ordering::Greater,
            (GraphValue::Boolean(x), GraphValue::Boolean(y)) => x.cmp(y),
            (GraphValue::Integer(x), GraphValue::Integer(y)) => x.cmp(y),
            (GraphValue::Float(x), GraphValue::Float(y)) => {
                x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
            }
            (GraphValue::Integer(x), GraphValue::Float(y)) => {
                (*x as f64).partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
            }
            (GraphValue::Float(x), GraphValue::Integer(y)) => {
                x.partial_cmp(&(*y as f64)).unwrap_or(std::cmp::Ordering::Equal)
            }
            (GraphValue::String(x), GraphValue::String(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        }
    }

    fn expr_to_name(&self, expr: &Expr) -> String {
        match expr {
            Expr::Variable(v) => v.clone(),
            Expr::PropertyAccess(v, p) => format!("{v}.{p}"),
            Expr::Literal(l) => format!("{l}"),
            Expr::FunctionCall { name, args } => {
                let args_str: Vec<String> = args.iter().map(|a| self.expr_to_name(a)).collect();
                format!("{name}({})", args_str.join(", "))
            }
            _ => "expr".to_string(),
        }
    }
}
