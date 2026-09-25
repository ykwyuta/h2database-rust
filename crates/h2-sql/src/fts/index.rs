use std::collections::HashSet;
use h2_mvstore::Transaction;
use h2_types::H2Result;
use crate::fts::tokenizer::{get_tokenizer, TokenizerKind};

pub struct FtsIndex;

impl FtsIndex {
    fn map_name(table: &str, column: &str, kind: TokenizerKind) -> String {
        let kind_str = match kind {
            TokenizerKind::NGram => "ngram",
            TokenizerKind::Morph => "morph",
        };
        format!("_fts_{}_{}_{}", table.to_lowercase(), column.to_lowercase(), kind_str)
    }

    /// ドキュメント（テキスト）の各トークンを行IDへマッピング
    pub fn index_document(
        tx: &Transaction,
        table: &str,
        column: &str,
        row_id: u64,
        text: &str,
        kind: TokenizerKind,
    ) -> H2Result<()> {
        let tokenizer = get_tokenizer(kind);
        let tokens = tokenizer.tokenize(text);
        let map = Self::map_name(table, column, kind);

        let unique_tokens: HashSet<String> = tokens.into_iter().collect();

        for token in unique_tokens {
            let key = token.into_bytes();
            let mut row_ids: Vec<u64> = if let Some(bytes) = tx.get(&map, &key)? {
                bincode::deserialize(&bytes).unwrap_or_default()
            } else {
                Vec::new()
            };

            if !row_ids.contains(&row_id) {
                row_ids.push(row_id);
                let serialized = bincode::serialize(&row_ids)
                    .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;
                tx.put(&map, key, serialized)?;
            }
        }

        Ok(())
    }

    /// ドキュメント削除時にインデックスから row_id を除去
    pub fn remove_document(
        tx: &Transaction,
        table: &str,
        column: &str,
        row_id: u64,
        text: &str,
        kind: TokenizerKind,
    ) -> H2Result<()> {
        let tokenizer = get_tokenizer(kind);
        let tokens = tokenizer.tokenize(text);
        let map = Self::map_name(table, column, kind);

        let unique_tokens: HashSet<String> = tokens.into_iter().collect();

        for token in unique_tokens {
            let key = token.into_bytes();
            if let Some(bytes) = tx.get(&map, &key)? {
                let mut row_ids: Vec<u64> = bincode::deserialize(&bytes).unwrap_or_default();
                if let Some(pos) = row_ids.iter().position(|&id| id == row_id) {
                    row_ids.remove(pos);
                    if row_ids.is_empty() {
                        tx.remove(&map, &key)?;
                    } else {
                        let serialized = bincode::serialize(&row_ids)
                            .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;
                        tx.put(&map, key, serialized)?;
                    }
                }
            }
        }

        Ok(())
    }

    /// クエリ文字列をトークナイズし、全トークンに合致する row_id を検索（AND検索）
    pub fn search(
        tx: &Transaction,
        table: &str,
        column: &str,
        query: &str,
        kind: TokenizerKind,
    ) -> H2Result<Vec<u64>> {
        let tokenizer = get_tokenizer(kind);
        let tokens = tokenizer.tokenize(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let map = Self::map_name(table, column, kind);
        let mut result_set: Option<HashSet<u64>> = None;

        for token in tokens {
            let key = token.into_bytes();
            let row_ids: HashSet<u64> = if let Some(bytes) = tx.get(&map, &key)? {
                let ids: Vec<u64> = bincode::deserialize(&bytes).unwrap_or_default();
                ids.into_iter().collect()
            } else {
                HashSet::new()
            };

            match result_set {
                None => {
                    result_set = Some(row_ids);
                }
                Some(ref mut current) => {
                    *current = current.intersection(&row_ids).cloned().collect();
                }
            }
        }

        let mut final_ids: Vec<u64> = result_set.unwrap_or_default().into_iter().collect();
        final_ids.sort_unstable();
        Ok(final_ids)
    }
}
