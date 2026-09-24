#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenizerKind {
    NGram,
    Morph,
}

pub trait Tokenizer: Send + Sync {
    fn tokenize(&self, text: &str) -> Vec<String>;
}

/// N-Gram トークナイザー（デフォルト: 2-gram / Bigram）
#[derive(Debug, Clone)]
pub struct NGramTokenizer {
    pub n: usize,
}

impl Default for NGramTokenizer {
    fn default() -> Self {
        Self { n: 2 }
    }
}

impl Tokenizer for NGramTokenizer {
    fn tokenize(&self, text: &str) -> Vec<String> {
        let cleaned: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
            .flat_map(|c| c.to_lowercase())
            .collect();

        let chars: Vec<char> = cleaned.chars().collect();
        if chars.is_empty() {
            return Vec::new();
        }

        if chars.len() <= self.n {
            return vec![chars.into_iter().collect()];
        }

        let mut tokens = Vec::new();
        for i in 0..=chars.len() - self.n {
            let token: String = chars[i..i + self.n].iter().collect();
            tokens.push(token);
        }

        tokens
    }
}

/// 文字種（漢字・ひらがな・カタカナ・アルファベット）境界に基づく形態素解析トークナイザー
#[derive(Debug, Clone, Default)]
pub struct MorphTokenizer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharType {
    Kanji,
    Hiragana,
    Katakana,
    Alphanumeric,
    Other,
}

impl MorphTokenizer {
    fn char_type(c: char) -> CharType {
        let u = c as u32;
        if (0x4E00..=0x9FFF).contains(&u) || (0x3400..=0x4DBF).contains(&u) {
            CharType::Kanji
        } else if (0x3040..=0x309F).contains(&u) {
            CharType::Hiragana
        } else if (0x30A0..=0x30FF).contains(&u) {
            CharType::Katakana
        } else if c.is_alphanumeric() {
            CharType::Alphanumeric
        } else {
            CharType::Other
        }
    }

    /// 1文字の助詞などノイズになりやすい短語の判定
    fn is_stop_word(token: &str) -> bool {
        matches!(token, "の" | "に" | "は" | "を" | "た" | "が" | "で" | "て" | "と" | "し" | "れ" | "さ" | "ある" | "いる" | "も")
    }
}

impl Tokenizer for MorphTokenizer {
    fn tokenize(&self, text: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current_token = String::new();
        let mut current_type = CharType::Other;

        for c in text.chars() {
            let ct = Self::char_type(c);
            if ct == CharType::Other {
                if !current_token.is_empty() {
                    let tok = current_token.to_lowercase();
                    if !Self::is_stop_word(&tok) {
                        tokens.push(tok);
                    }
                    current_token.clear();
                }
                current_type = CharType::Other;
                continue;
            }

            if ct != current_type && !current_token.is_empty() {
                let tok = current_token.to_lowercase();
                if !Self::is_stop_word(&tok) {
                    tokens.push(tok);
                }
                current_token.clear();
            }

            current_token.push(c);
            current_type = ct;
        }

        if !current_token.is_empty() {
            let tok = current_token.to_lowercase();
            if !Self::is_stop_word(&tok) {
                tokens.push(tok);
            }
        }

        tokens
    }
}

pub fn get_tokenizer(kind: TokenizerKind) -> Box<dyn Tokenizer> {
    match kind {
        TokenizerKind::NGram => Box::new(NGramTokenizer::default()),
        TokenizerKind::Morph => Box::new(MorphTokenizer),
    }
}
