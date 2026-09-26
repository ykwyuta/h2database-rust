use h2_types::{DataType, H2Error, H2Result, Value};
use crate::procedural::ast::*;

/// PL/pgSQL トークン定義
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Declare,
    Begin,
    End,
    If,
    Then,
    Elsif,
    Else,
    While,
    For,
    In,
    Reverse,
    By,
    Loop,
    Exit,
    Continue,
    When,
    Return,
    ReturnNext,
    ReturnQuery,
    Next,
    Query,
    Raise,
    Notice,
    Warning,
    Info,
    Exception,
    Perform,
    Select,
    Into,
    Strict,
    Execute,
    Using,
    Alias,
    Constant,
    NotNull,
    Default,
    Null,
    And,
    Or,
    Not,
    Like,
    Is,
    True,
    False,

    // Symbols
    Semicolon,
    Colon,
    Comma,
    Assign,        // :=
    Equal,         // =
    NotEqual,      // != or <>
    Lt,            // <
    LtEq,          // <=
    Gt,            // >
    GtEq,          // >=
    Plus,          // +
    Minus,         // -
    Star,          // *
    Slash,         // /
    Percent,       // %
    Concat,        // ||
    DotDot,        // ..
    LParen,        // (
    RParen,        // )

    // Values & Identifiers
    Ident(String),
    StringLit(String),
    IntLit(i64),
    FloatLit(f64),
    PositionalArg(usize), // $1, $2, ...
}

/// PL/pgSQL 字句解析器
pub struct Lexer<'a> {
    _input: &'a str,
    chars: Vec<char>,
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            _input: input,
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_next(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.chars.get(self.pos).copied();
        if ch.is_some() {
            self.pos += 1;
        }
        ch
    }

    fn skip_whitespace_and_comments(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.advance();
            } else if ch == '-' && self.peek_next() == Some('-') {
                // 単一行コメント --
                while let Some(c) = self.advance() {
                    if c == '\n' {
                        break;
                    }
                }
            } else if ch == '/' && self.peek_next() == Some('*') {
                // 複数行コメント /* ... */
                self.advance();
                self.advance();
                while self.pos + 1 < self.chars.len() {
                    if self.chars[self.pos] == '*' && self.chars[self.pos + 1] == '/' {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    pub fn tokenize(&mut self) -> H2Result<Vec<Token>> {
        let mut tokens = Vec::new();
        while {
            self.skip_whitespace_and_comments();
            self.pos < self.chars.len()
        } {
            let ch = self.peek().unwrap();
            match ch {
                ';' => {
                    self.advance();
                    tokens.push(Token::Semicolon);
                }
                ',' => {
                    self.advance();
                    tokens.push(Token::Comma);
                }
                '(' => {
                    self.advance();
                    tokens.push(Token::LParen);
                }
                ')' => {
                    self.advance();
                    tokens.push(Token::RParen);
                }
                '+' => {
                    self.advance();
                    tokens.push(Token::Plus);
                }
                '-' => {
                    self.advance();
                    tokens.push(Token::Minus);
                }
                '*' => {
                    self.advance();
                    tokens.push(Token::Star);
                }
                '/' => {
                    self.advance();
                    tokens.push(Token::Slash);
                }
                '%' => {
                    self.advance();
                    tokens.push(Token::Percent);
                }
                ':' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token::Assign);
                    } else {
                        tokens.push(Token::Colon);
                    }
                }
                '=' => {
                    self.advance();
                    tokens.push(Token::Equal);
                }
                '<' => {
                    self.advance();
                    if self.peek() == Some('>') {
                        self.advance();
                        tokens.push(Token::NotEqual);
                    } else if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token::LtEq);
                    } else {
                        tokens.push(Token::Lt);
                    }
                }
                '>' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token::GtEq);
                    } else {
                        tokens.push(Token::Gt);
                    }
                }
                '!' => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        tokens.push(Token::NotEqual);
                    } else {
                        return Err(H2Error::SqlParse(format!("Unexpected character '!' at pos {}", self.pos)));
                    }
                }
                '|' => {
                    self.advance();
                    if self.peek() == Some('|') {
                        self.advance();
                        tokens.push(Token::Concat);
                    } else {
                        return Err(H2Error::SqlParse(format!("Unexpected character '|' at pos {}", self.pos)));
                    }
                }
                '.' => {
                    self.advance();
                    if self.peek() == Some('.') {
                        self.advance();
                        tokens.push(Token::DotDot);
                    } else {
                        tokens.push(Token::Ident(".".to_string()));
                    }
                }
                '$' => {
                    self.advance();
                    let mut num_str = String::new();
                    while let Some(c) = self.peek() {
                        if c.is_ascii_digit() {
                            num_str.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    if !num_str.is_empty() {
                        let pos_idx: usize = num_str.parse().unwrap_or(1);
                        tokens.push(Token::PositionalArg(pos_idx));
                    } else {
                        // Dollar sign used for identifiers
                        tokens.push(Token::Ident("$".to_string()));
                    }
                }
                '\'' => {
                    // 文字列リテラル
                    self.advance();
                    let mut s = String::new();
                    while let Some(c) = self.advance() {
                        if c == '\'' {
                            if self.peek() == Some('\'') {
                                self.advance();
                                s.push('\'');
                            } else {
                                break;
                            }
                        } else {
                            s.push(c);
                        }
                    }
                    tokens.push(Token::StringLit(s));
                }
                _ if ch.is_ascii_digit() => {
                    let mut num_str = String::new();
                    let mut is_float = false;
                    while let Some(c) = self.peek() {
                        if c.is_ascii_digit() {
                            num_str.push(c);
                            self.advance();
                        } else if c == '.' && self.peek_next() != Some('.') && !is_float {
                            is_float = true;
                            num_str.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    if is_float {
                        let f: f64 = num_str.parse().unwrap_or(0.0);
                        tokens.push(Token::FloatLit(f));
                    } else {
                        let i: i64 = num_str.parse().unwrap_or(0);
                        tokens.push(Token::IntLit(i));
                    }
                }
                _ if ch.is_alphabetic() || ch == '_' => {
                    let mut id = String::new();
                    while let Some(c) = self.peek() {
                        if c.is_alphanumeric() || c == '_' {
                            id.push(c);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    let upper = id.to_uppercase();
                    let tok = match upper.as_str() {
                        "DECLARE" => Token::Declare,
                        "BEGIN" => Token::Begin,
                        "END" => Token::End,
                        "IF" => Token::If,
                        "THEN" => Token::Then,
                        "ELSIF" | "ELSEIF" => Token::Elsif,
                        "ELSE" => Token::Else,
                        "WHILE" => Token::While,
                        "FOR" => Token::For,
                        "IN" => Token::In,
                        "REVERSE" => Token::Reverse,
                        "BY" => Token::By,
                        "LOOP" => Token::Loop,
                        "EXIT" => Token::Exit,
                        "CONTINUE" => Token::Continue,
                        "WHEN" => Token::When,
                        "RETURN" => Token::Return,
                        "NEXT" => Token::Next,
                        "QUERY" => Token::Query,
                        "RAISE" => Token::Raise,
                        "NOTICE" => Token::Notice,
                        "WARNING" => Token::Warning,
                        "INFO" => Token::Info,
                        "EXCEPTION" => Token::Exception,
                        "PERFORM" => Token::Perform,
                        "SELECT" => Token::Select,
                        "INTO" => Token::Into,
                        "STRICT" => Token::Strict,
                        "EXECUTE" => Token::Execute,
                        "USING" => Token::Using,
                        "ALIAS" => Token::Alias,
                        "CONSTANT" => Token::Constant,
                        "DEFAULT" => Token::Default,
                        "NULL" => Token::Null,
                        "AND" => Token::And,
                        "OR" => Token::Or,
                        "NOT" => Token::Not,
                        "LIKE" => Token::Like,
                        "IS" => Token::Is,
                        "TRUE" => Token::True,
                        "FALSE" => Token::False,
                        _ => Token::Ident(id),
                    };
                    tokens.push(tok);
                }
                _ => {
                    return Err(H2Error::SqlParse(format!("Unexpected character '{}' at pos {}", ch, self.pos)));
                }
            }
        }
        Ok(tokens)
    }
}

/// PL/pgSQL 構文解析器
pub struct PlPgSqlParser {
    tokens: Vec<Token>,
    pos: usize,
}

impl PlPgSqlParser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn peek_offset(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.pos + offset)
    }

    fn advance(&mut self) -> Option<&Token> {
        if self.pos < self.tokens.len() {
            let t = &self.tokens[self.pos];
            self.pos += 1;
            Some(t)
        } else {
            None
        }
    }

    fn check(&self, token: &Token) -> bool {
        self.peek() == Some(token)
    }

    fn match_token(&mut self, token: &Token) -> bool {
        if self.check(token) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: &Token) -> H2Result<()> {
        if self.match_token(token) {
            Ok(())
        } else {
            Err(H2Error::SqlParse(format!(
                "Expected token {:?}, found {:?}",
                token,
                self.peek()
            )))
        }
    }

    /// 本文ブロックをパース
    pub fn parse_block(&mut self) -> H2Result<ProcBlock> {
        let mut declarations = Vec::new();

        // [DECLARE ...]
        if self.match_token(&Token::Declare) {
            while let Some(tok) = self.peek() {
                if tok == &Token::Begin {
                    break;
                }
                declarations.push(self.parse_var_decl()?);
            }
        }

        self.expect(&Token::Begin)?;

        let mut statements = Vec::new();
        while let Some(tok) = self.peek() {
            if tok == &Token::End || tok == &Token::Exception {
                break;
            }
            statements.push(self.parse_statement()?);
        }

        let mut exception_handlers = Vec::new();
        if self.match_token(&Token::Exception) {
            while let Some(tok) = self.peek() {
                if tok == &Token::End {
                    break;
                }
                self.expect(&Token::When)?;
                let cond_name = match self.advance() {
                    Some(Token::Ident(s)) => s.clone(),
                    Some(Token::Exception) => "EXCEPTION".to_string(),
                    other => {
                        return Err(H2Error::SqlParse(format!("Expected exception condition, found {:?}", other)))
                    }
                };
                self.expect(&Token::Then)?;
                let mut handler_stmts = Vec::new();
                while let Some(t) = self.peek() {
                    if t == &Token::When || t == &Token::End {
                        break;
                    }
                    handler_stmts.push(self.parse_statement()?);
                }
                exception_handlers.push(ExceptionHandler {
                    condition: cond_name,
                    statements: handler_stmts,
                });
            }
        }

        self.expect(&Token::End)?;
        // END 後にラベルやセミコロンがある場合を消費
        if let Some(Token::Ident(_)) = self.peek() {
            self.advance();
        }
        let _ = self.match_token(&Token::Semicolon);

        Ok(ProcBlock {
            label: None,
            declarations,
            statements,
            exception_handlers,
        })
    }

    /// 変数宣言のパース
    fn parse_var_decl(&mut self) -> H2Result<VarDecl> {
        let var_name = match self.advance() {
            Some(Token::Ident(s)) => s.clone(),
            other => return Err(H2Error::SqlParse(format!("Expected variable name, found {:?}", other))),
        };

        // ALIAS FOR $1 / ALIAS FOR param_name
        if self.match_token(&Token::Alias) {
            self.expect(&Token::For)?;
            match self.advance() {
                Some(Token::PositionalArg(idx)) => {
                    let idx_val = *idx;
                    self.expect(&Token::Semicolon)?;
                    return Ok(VarDecl {
                        name: var_name,
                        data_type: DataType::VarChar(None), // 動的または推論
                        default: None,
                        not_null: false,
                        alias_for_pos: Some(idx_val),
                    });
                }
                Some(Token::Ident(aliased_name)) => {
                    let _ = aliased_name;
                    self.expect(&Token::Semicolon)?;
                    return Ok(VarDecl {
                        name: var_name,
                        data_type: DataType::VarChar(None),
                        default: None,
                        not_null: false,
                        alias_for_pos: None,
                    });
                }
                other => return Err(H2Error::SqlParse(format!("Expected ALIAS target, found {:?}", other))),
            }
        }

        let _is_constant = self.match_token(&Token::Constant);

        let data_type = self.parse_data_type()?;

        let mut not_null = false;
        if self.match_token(&Token::Not) {
            self.expect(&Token::Null)?;
            not_null = true;
        }

        let mut default = None;
        if self.match_token(&Token::Assign)
            || self.match_token(&Token::Equal)
            || self.match_token(&Token::Default)
        {
            default = Some(self.parse_expr()?);
        }

        self.expect(&Token::Semicolon)?;

        Ok(VarDecl {
            name: var_name,
            data_type,
            default,
            not_null,
            alias_for_pos: None,
        })
    }

    /// データ型のパース
    fn parse_data_type(&mut self) -> H2Result<DataType> {
        let type_name = match self.advance() {
            Some(Token::Ident(s)) => s.to_uppercase(),
            other => return Err(H2Error::SqlParse(format!("Expected data type name, found {:?}", other))),
        };

        match type_name.as_str() {
            "INT" | "INTEGER" | "INT4" => Ok(DataType::Integer),
            "BIGINT" | "INT8" => Ok(DataType::BigInt),
            "SMALLINT" | "INT2" => Ok(DataType::SmallInt),
            "TINYINT" => Ok(DataType::TinyInt),
            "BOOLEAN" | "BOOL" => Ok(DataType::Boolean),
            "FLOAT" | "REAL" | "FLOAT4" => Ok(DataType::Float),
            "DOUBLE" | "FLOAT8" => Ok(DataType::Double),
            "NUMERIC" | "DECIMAL" => {
                if self.match_token(&Token::LParen) {
                    let p = match self.advance() {
                        Some(Token::IntLit(n)) => *n as u8,
                        _ => 10,
                    };
                    let s = if self.match_token(&Token::Comma) {
                        match self.advance() {
                            Some(Token::IntLit(n)) => *n as u8,
                            _ => 0,
                        }
                    } else {
                        0
                    };
                    self.expect(&Token::RParen)?;
                    Ok(DataType::Decimal(p, s))
                } else {
                    Ok(DataType::Decimal(10, 2))
                }
            }
            "VARCHAR" | "CHAR" | "CHARACTER" | "TEXT" => {
                let mut len = None;
                if self.match_token(&Token::LParen) {
                    if let Some(Token::IntLit(n)) = self.advance() {
                        len = Some(*n as usize);
                    }
                    self.expect(&Token::RParen)?;
                }
                if type_name == "CHAR" {
                    Ok(DataType::Char(len.unwrap_or(1)))
                } else {
                    Ok(DataType::VarChar(len))
                }
            }
            "TIMESTAMP" => Ok(DataType::Timestamp),
            "DATE" => Ok(DataType::Date),
            "TIME" => Ok(DataType::Time),
            "JSON" | "JSONB" => Ok(DataType::Json),
            _ => Ok(DataType::VarChar(None)),
        }
    }

    /// 各種文のパース
    pub fn parse_statement(&mut self) -> H2Result<ProcStmt> {
        let tok = self.peek().cloned().ok_or_else(|| H2Error::SqlParse("Unexpected EOF in statement".to_string()))?;

        match tok {
            Token::Null => {
                self.advance();
                self.expect(&Token::Semicolon)?;
                Ok(ProcStmt::Null)
            }
            Token::Return => {
                self.advance();
                if self.match_token(&Token::Next) {
                    let val = self.parse_expr()?;
                    self.expect(&Token::Semicolon)?;
                    Ok(ProcStmt::ReturnNext { value: val })
                } else if self.match_token(&Token::Query) {
                    let query_str = self.collect_until_semicolon()?;
                    Ok(ProcStmt::ReturnQuery { query: query_str })
                } else if self.match_token(&Token::Semicolon) {
                    Ok(ProcStmt::Return { value: None })
                } else {
                    let val = self.parse_expr()?;
                    self.expect(&Token::Semicolon)?;
                    Ok(ProcStmt::Return { value: Some(val) })
                }
            }
            Token::If => {
                self.advance();
                let mut branches = Vec::new();
                let cond = self.parse_expr()?;
                self.expect(&Token::Then)?;

                let mut stmts = Vec::new();
                while let Some(t) = self.peek() {
                    if t == &Token::Elsif || t == &Token::Else || t == &Token::End {
                        break;
                    }
                    stmts.push(self.parse_statement()?);
                }
                branches.push((cond, stmts));

                while self.match_token(&Token::Elsif) {
                    let elsif_cond = self.parse_expr()?;
                    self.expect(&Token::Then)?;
                    let mut elsif_stmts = Vec::new();
                    while let Some(t) = self.peek() {
                        if t == &Token::Elsif || t == &Token::Else || t == &Token::End {
                            break;
                        }
                        elsif_stmts.push(self.parse_statement()?);
                    }
                    branches.push((elsif_cond, elsif_stmts));
                }

                let mut else_branch = None;
                if self.match_token(&Token::Else) {
                    let mut else_stmts = Vec::new();
                    while let Some(t) = self.peek() {
                        if t == &Token::End {
                            break;
                        }
                        else_stmts.push(self.parse_statement()?);
                    }
                    else_branch = Some(else_stmts);
                }

                self.expect(&Token::End)?;
                self.expect(&Token::If)?;
                self.expect(&Token::Semicolon)?;

                Ok(ProcStmt::If {
                    branches,
                    else_branch,
                })
            }
            Token::While => {
                self.advance();
                let cond = self.parse_expr()?;
                self.expect(&Token::Loop)?;
                let mut body = Vec::new();
                while let Some(t) = self.peek() {
                    if t == &Token::End {
                        break;
                    }
                    body.push(self.parse_statement()?);
                }
                self.expect(&Token::End)?;
                self.expect(&Token::Loop)?;
                self.expect(&Token::Semicolon)?;
                Ok(ProcStmt::While {
                    condition: cond,
                    body,
                    label: None,
                })
            }
            Token::For => {
                self.advance();
                let var_name = match self.advance() {
                    Some(Token::Ident(s)) => s.clone(),
                    other => return Err(H2Error::SqlParse(format!("Expected FOR loop variable, found {:?}", other))),
                };
                self.expect(&Token::In)?;

                let reverse = self.match_token(&Token::Reverse);
                let start = self.parse_expr()?;
                self.expect(&Token::DotDot)?;
                let end = self.parse_expr()?;

                let mut step = None;
                if self.match_token(&Token::By) {
                    step = Some(self.parse_expr()?);
                }

                self.expect(&Token::Loop)?;
                let mut body = Vec::new();
                while let Some(t) = self.peek() {
                    if t == &Token::End {
                        break;
                    }
                    body.push(self.parse_statement()?);
                }
                self.expect(&Token::End)?;
                self.expect(&Token::Loop)?;
                self.expect(&Token::Semicolon)?;

                Ok(ProcStmt::ForRange {
                    var_name,
                    start,
                    end,
                    step,
                    reverse,
                    body,
                })
            }
            Token::Loop => {
                self.advance();
                let mut body = Vec::new();
                while let Some(t) = self.peek() {
                    if t == &Token::End {
                        break;
                    }
                    body.push(self.parse_statement()?);
                }
                self.expect(&Token::End)?;
                self.expect(&Token::Loop)?;
                self.expect(&Token::Semicolon)?;
                Ok(ProcStmt::Loop {
                    body,
                    label: None,
                })
            }
            Token::Exit => {
                self.advance();
                let mut label = None;
                if let Some(Token::Ident(s)) = self.peek() {
                    label = Some(s.clone());
                    self.advance();
                }
                let mut condition = None;
                if self.match_token(&Token::When) {
                    condition = Some(self.parse_expr()?);
                }
                self.expect(&Token::Semicolon)?;
                Ok(ProcStmt::Exit { condition, label })
            }
            Token::Continue => {
                self.advance();
                let mut label = None;
                if let Some(Token::Ident(s)) = self.peek() {
                    label = Some(s.clone());
                    self.advance();
                }
                let mut condition = None;
                if self.match_token(&Token::When) {
                    condition = Some(self.parse_expr()?);
                }
                self.expect(&Token::Semicolon)?;
                Ok(ProcStmt::Continue { condition, label })
            }
            Token::Raise => {
                self.advance();
                let level = match self.peek() {
                    Some(Token::Exception) => {
                        self.advance();
                        RaiseLevel::Exception
                    }
                    Some(Token::Notice) => {
                        self.advance();
                        RaiseLevel::Notice
                    }
                    Some(Token::Warning) => {
                        self.advance();
                        RaiseLevel::Warning
                    }
                    Some(Token::Info) => {
                        self.advance();
                        RaiseLevel::Info
                    }
                    _ => RaiseLevel::Notice,
                };
                let message = match self.advance() {
                    Some(Token::StringLit(s)) => s.clone(),
                    other => return Err(H2Error::SqlParse(format!("Expected RAISE message string, found {:?}", other))),
                };
                let mut params = Vec::new();
                while self.match_token(&Token::Comma) {
                    params.push(self.parse_expr()?);
                }
                self.expect(&Token::Semicolon)?;
                Ok(ProcStmt::Raise {
                    level,
                    message,
                    params,
                })
            }
            Token::Perform => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(&Token::Semicolon)?;
                Ok(ProcStmt::Perform { expr })
            }
            Token::Select => {
                // SELECT col1, col2 INTO [STRICT] var1, var2 FROM ...;
                // トークンを走査して INTO の位置を特定
                self.advance();
                let mut select_cols = Vec::new();
                let mut found_into = false;
                let mut strict = false;
                let mut into_targets = Vec::new();

                while let Some(t) = self.peek() {
                    if t == &Token::Into {
                        self.advance();
                        found_into = true;
                        if self.match_token(&Token::Strict) {
                            strict = true;
                        }
                        loop {
                            match self.advance() {
                                Some(Token::Ident(s)) => into_targets.push(s.clone()),
                                other => return Err(H2Error::SqlParse(format!("Expected INTO target variable, found {:?}", other))),
                            }
                            if !self.match_token(&Token::Comma) {
                                break;
                            }
                        }
                        break;
                    }
                    select_cols.push(self.advance().unwrap().clone());
                }

                if found_into {
                    let remaining_sql = self.collect_until_semicolon()?;
                    let col_sql = tokens_to_sql(&select_cols);
                    let full_query = format!("SELECT {} {}", col_sql, remaining_sql);
                    Ok(ProcStmt::SelectInto {
                        targets: into_targets,
                        query: full_query,
                        strict,
                    })
                } else {
                    let remaining = self.collect_until_semicolon()?;
                    let full_sql = format!("SELECT {} {}", tokens_to_sql(&select_cols), remaining);
                    Ok(ProcStmt::SqlStmt { sql: full_sql })
                }
            }
            Token::Execute => {
                self.advance();
                let query_expr = self.parse_expr()?;
                let mut into_targets = Vec::new();
                if self.match_token(&Token::Into) {
                    loop {
                        match self.advance() {
                            Some(Token::Ident(s)) => into_targets.push(s.clone()),
                            other => return Err(H2Error::SqlParse(format!("Expected EXECUTE INTO target, found {:?}", other))),
                        }
                        if !self.match_token(&Token::Comma) {
                            break;
                        }
                    }
                }
                let mut using_params = Vec::new();
                if self.match_token(&Token::Using) {
                    loop {
                        using_params.push(self.parse_expr()?);
                        if !self.match_token(&Token::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&Token::Semicolon)?;
                Ok(ProcStmt::ExecuteDynamic {
                    query_expr,
                    into_targets,
                    using_params,
                })
            }
            Token::Begin | Token::Declare => {
                let sub_block = self.parse_block()?;
                Ok(ProcStmt::Block(sub_block))
            }
            Token::Ident(ref name) => {
                let target_name = name.clone();
                // target := expr; または target = expr;
                if self.peek_offset(1) == Some(&Token::Assign)
                    || self.peek_offset(1) == Some(&Token::Equal)
                {
                    self.advance(); // consume ident
                    self.advance(); // consume := or =
                    let expr = self.parse_expr()?;
                    self.expect(&Token::Semicolon)?;
                    Ok(ProcStmt::Assign {
                        target: target_name,
                        expr,
                    })
                } else {
                    // DML / DDL SQL 文 (INSERT, UPDATE, DELETE など)
                    let raw_sql = self.collect_until_semicolon()?;
                    Ok(ProcStmt::SqlStmt { sql: raw_sql })
                }
            }
            _ => {
                // その他の任意の SQL 文
                let raw_sql = self.collect_until_semicolon()?;
                Ok(ProcStmt::SqlStmt { sql: raw_sql })
            }
        }
    }

    fn collect_until_semicolon(&mut self) -> H2Result<String> {
        let mut parts = Vec::new();
        while let Some(tok) = self.advance() {
            if tok == &Token::Semicolon {
                break;
            }
            parts.push(tok.clone());
        }
        Ok(tokens_to_sql(&parts))
    }

    /// 式のパース (優先順位降順)
    pub fn parse_expr(&mut self) -> H2Result<ProcExpr> {
        self.parse_or_expr()
    }

    fn parse_or_expr(&mut self) -> H2Result<ProcExpr> {
        let mut left = self.parse_and_expr()?;
        while self.match_token(&Token::Or) {
            let right = self.parse_and_expr()?;
            left = ProcExpr::Binary {
                left: Box::new(left),
                op: ProcBinaryOp::Or,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and_expr(&mut self) -> H2Result<ProcExpr> {
        let mut left = self.parse_comparison_expr()?;
        while self.match_token(&Token::And) {
            let right = self.parse_comparison_expr()?;
            left = ProcExpr::Binary {
                left: Box::new(left),
                op: ProcBinaryOp::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_comparison_expr(&mut self) -> H2Result<ProcExpr> {
        let mut left = self.parse_concat_expr()?;

        if self.match_token(&Token::Is) {
            if self.match_token(&Token::Not) {
                self.expect(&Token::Null)?;
                return Ok(ProcExpr::IsNotNull(Box::new(left)));
            } else {
                self.expect(&Token::Null)?;
                return Ok(ProcExpr::IsNull(Box::new(left)));
            }
        }

        let op = if self.match_token(&Token::Equal) {
            Some(ProcBinaryOp::Eq)
        } else if self.match_token(&Token::NotEqual) {
            Some(ProcBinaryOp::NotEq)
        } else if self.match_token(&Token::LtEq) {
            Some(ProcBinaryOp::LtEq)
        } else if self.match_token(&Token::GtEq) {
            Some(ProcBinaryOp::GtEq)
        } else if self.match_token(&Token::Lt) {
            Some(ProcBinaryOp::Lt)
        } else if self.match_token(&Token::Gt) {
            Some(ProcBinaryOp::Gt)
        } else if self.match_token(&Token::Like) {
            Some(ProcBinaryOp::Like)
        } else {
            None
        };

        if let Some(bin_op) = op {
            let right = self.parse_concat_expr()?;
            left = ProcExpr::Binary {
                left: Box::new(left),
                op: bin_op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    fn parse_concat_expr(&mut self) -> H2Result<ProcExpr> {
        let mut left = self.parse_add_sub_expr()?;
        while self.match_token(&Token::Concat) {
            let right = self.parse_add_sub_expr()?;
            left = ProcExpr::Binary {
                left: Box::new(left),
                op: ProcBinaryOp::Concat,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_add_sub_expr(&mut self) -> H2Result<ProcExpr> {
        let mut left = self.parse_mul_div_expr()?;
        while let Some(tok) = self.peek() {
            let op = match tok {
                Token::Plus => ProcBinaryOp::Add,
                Token::Minus => ProcBinaryOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_mul_div_expr()?;
            left = ProcExpr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_mul_div_expr(&mut self) -> H2Result<ProcExpr> {
        let mut left = self.parse_unary_expr()?;
        while let Some(tok) = self.peek() {
            let op = match tok {
                Token::Star => ProcBinaryOp::Mul,
                Token::Slash => ProcBinaryOp::Div,
                Token::Percent => ProcBinaryOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary_expr()?;
            left = ProcExpr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary_expr(&mut self) -> H2Result<ProcExpr> {
        if self.match_token(&Token::Not) {
            let expr = self.parse_unary_expr()?;
            return Ok(ProcExpr::Unary {
                op: ProcUnaryOp::Not,
                expr: Box::new(expr),
            });
        }
        if self.match_token(&Token::Minus) {
            let expr = self.parse_unary_expr()?;
            return Ok(ProcExpr::Unary {
                op: ProcUnaryOp::Neg,
                expr: Box::new(expr),
            });
        }
        self.parse_primary_expr()
    }

    fn parse_primary_expr(&mut self) -> H2Result<ProcExpr> {
        let tok = self.advance().ok_or_else(|| H2Error::SqlParse("Unexpected EOF in expression".to_string()))?;
        match tok {
            Token::IntLit(n) => Ok(ProcExpr::Literal(Value::BigInt(*n))),
            Token::FloatLit(f) => Ok(ProcExpr::Literal(Value::Double(*f))),
            Token::StringLit(s) => Ok(ProcExpr::Literal(Value::String(s.clone()))),
            Token::True => Ok(ProcExpr::Literal(Value::Boolean(true))),
            Token::False => Ok(ProcExpr::Literal(Value::Boolean(false))),
            Token::Null => Ok(ProcExpr::Literal(Value::Null)),
            Token::PositionalArg(idx) => Ok(ProcExpr::PositionalArg(*idx)),
            Token::LParen => {
                let inner = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(inner)
            }
            Token::Ident(name) => {
                let id_name = name.clone();
                if self.match_token(&Token::LParen) {
                    // 関数呼出し: name(arg1, arg2, ...)
                    let mut args = Vec::new();
                    if !self.check(&Token::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if !self.match_token(&Token::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(&Token::RParen)?;
                    Ok(ProcExpr::FunctionCall {
                        name: id_name,
                        args,
                    })
                } else {
                    Ok(ProcExpr::Variable(id_name))
                }
            }
            other => Err(H2Error::SqlParse(format!("Unexpected token in expression: {:?}", other))),
        }
    }
}

fn tokens_to_sql(tokens: &[Token]) -> String {
    let mut out = String::new();
    for (i, t) in tokens.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match t {
            Token::Ident(s) => out.push_str(s),
            Token::StringLit(s) => {
                out.push('\'');
                out.push_str(&s.replace('\'', "''"));
                out.push('\'');
            }
            Token::IntLit(n) => out.push_str(&n.to_string()),
            Token::FloatLit(f) => out.push_str(&f.to_string()),
            Token::PositionalArg(idx) => {
                out.push('$');
                out.push_str(&idx.to_string());
            }
            Token::Semicolon => out.push(';'),
            Token::Colon => out.push(':'),
            Token::Comma => out.push(','),
            Token::Assign => out.push_str(":="),
            Token::Equal => out.push('='),
            Token::NotEqual => out.push_str("!="),
            Token::Lt => out.push('<'),
            Token::LtEq => out.push_str("<="),
            Token::Gt => out.push('>'),
            Token::GtEq => out.push_str(">="),
            Token::Plus => out.push('+'),
            Token::Minus => out.push('-'),
            Token::Star => out.push('*'),
            Token::Slash => out.push('/'),
            Token::Percent => out.push('%'),
            Token::Concat => out.push_str("||"),
            Token::DotDot => out.push_str(".."),
            Token::LParen => out.push('('),
            Token::RParen => out.push(')'),
            Token::Null => out.push_str("NULL"),
            Token::True => out.push_str("TRUE"),
            Token::False => out.push_str("FALSE"),
            _ => out.push_str(&format!("{:?}", t).to_uppercase()),
        }
    }
    out
}

/// DDL 文 (CREATE [OR REPLACE] FUNCTION / PROCEDURE ...) をパース
pub fn parse_create_routine(ddl: &str) -> H2Result<RoutineDef> {
    let trimmed = ddl.trim();
    let upper = trimmed.to_uppercase();

    let is_function = upper.contains("FUNCTION");
    let is_proc = upper.contains("PROCEDURE");
    if !is_function && !is_proc {
        return Err(H2Error::SqlParse("DDL must contain FUNCTION or PROCEDURE".to_string()));
    }
    let kind = if is_function { RoutineKind::Function } else { RoutineKind::Procedure };

    // 1. ルーチン名の抽出
    let header_kw = if is_function { "FUNCTION" } else { "PROCEDURE" };
    let after_kw = match upper.find(header_kw) {
        Some(idx) => &trimmed[idx + header_kw.len()..].trim(),
        None => return Err(H2Error::SqlParse("Could not find FUNCTION or PROCEDURE keyword".to_string())),
    };

    let paren_pos = after_kw.find('(').ok_or_else(|| {
        H2Error::SqlParse("Missing parameter list '(' in routine definition".to_string())
    })?;
    let raw_name = after_kw[..paren_pos].trim();
    let name = raw_name.trim_matches('"').trim_matches('\'').to_string();

    // 2. 引数リストの抽出
    let mut depth = 1;
    let mut close_paren = None;
    for (idx, ch) in after_kw[paren_pos + 1..].char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                close_paren = Some(paren_pos + 1 + idx);
                break;
            }
        }
    }
    let close_paren = close_paren.ok_or_else(|| {
        H2Error::SqlParse("Missing closing ')' in parameter list".to_string())
    })?;
    let params_str = &after_kw[paren_pos + 1..close_paren].trim();
    let parameters = parse_param_defs(params_str)?;

    // 3. 戻り値型の抽出 (RETURNS ...)
    let after_params = &after_kw[close_paren + 1..].trim();
    let upper_after_params = after_params.to_uppercase();
    let mut return_type = None;
    if let Some(returns_idx) = upper_after_params.find("RETURNS") {
        let after_returns = after_params[returns_idx + "RETURNS".len()..].trim();
        // AS または LANGUAGE または 属性キーワードまでの単語を型名として取得
        let ret_type_str = after_returns
            .split_whitespace()
            .next()
            .unwrap_or("void")
            .to_uppercase();
        if ret_type_str != "VOID" {
            return_type = match ret_type_str.as_str() {
                "INT" | "INTEGER" | "INT4" => Some(DataType::Integer),
                "BIGINT" | "INT8" => Some(DataType::BigInt),
                "SMALLINT" | "INT2" => Some(DataType::SmallInt),
                "BOOLEAN" | "BOOL" => Some(DataType::Boolean),
                "FLOAT" | "REAL" => Some(DataType::Float),
                "DOUBLE" => Some(DataType::Double),
                "NUMERIC" | "DECIMAL" => Some(DataType::Decimal(10, 2)),
                "VARCHAR" | "TEXT" => Some(DataType::VarChar(None)),
                "DATE" => Some(DataType::Date),
                "TIMESTAMP" => Some(DataType::Timestamp),
                _ => Some(DataType::VarChar(None)),
            };
        }
    }

    // 4. STRICT / SECURITY DEFINER 属性
    let is_strict = upper.contains("STRICT") || upper.contains("RETURNS NULL ON NULL INPUT");
    let security_definer = upper.contains("SECURITY DEFINER");

    // 5. 本文の抽出 (AS $$ ... $$ または AS '...')
    let body_str = extract_body_source(trimmed)?;

    // 6. 本文のパース
    let mut lexer = Lexer::new(&body_str);
    let tokens = lexer.tokenize()?;
    let mut parser = PlPgSqlParser::new(tokens);
    let body = parser.parse_block()?;

    Ok(RoutineDef {
        name,
        schema: Some("public".to_string()),
        kind,
        language: RoutineLanguage::PlPgSql,
        parameters,
        return_type,
        is_strict,
        security_definer,
        body,
        source_sql: ddl.to_string(),
    })
}

pub fn extract_body_source(ddl: &str) -> H2Result<String> {
    // 1. $$ ... $$
    if let Some(start_dollar) = ddl.find("$$") {
        if let Some(end_dollar) = ddl[start_dollar + 2..].find("$$") {
            return Ok(ddl[start_dollar + 2..start_dollar + 2 + end_dollar].trim().to_string());
        }
    }

    // 2. $tag$ ... $tag$
    let re_dollar = regex::Regex::new(r"(\$[A-Za-z0-9_]*\$)([\s\S]*?)(\1)").unwrap();
    if let Some(caps) = re_dollar.captures(ddl) {
        if let Some(m) = caps.get(2) {
            return Ok(m.as_str().trim().to_string());
        }
    }

    // 3. AS '...'
    let upper = ddl.to_uppercase();
    if let Some(as_idx) = upper.find("AS") {
        let after_as = ddl[as_idx + 2..].trim();
        if after_as.starts_with('\'') {
            if let Some(end_quote) = after_as[1..].rfind('\'') {
                return Ok(after_as[1..1 + end_quote].trim().to_string());
            }
        }
    }

    Err(H2Error::SqlParse("Could not extract routine body from DDL".to_string()))
}

fn parse_param_defs(params_str: &str) -> H2Result<Vec<ParamDef>> {
    if params_str.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut defs = Vec::new();
    let parts = split_param_parts(params_str);

    for (pos, part) in parts.iter().enumerate() {
        let tokens: Vec<&str> = part.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }

        let mut mode = ParamMode::In;
        let mut name = String::new();
        let mut type_str = String::new();

        let is_mode = |w: &str| -> Option<ParamMode> {
            match w.to_uppercase().as_str() {
                "IN" => Some(ParamMode::In),
                "OUT" => Some(ParamMode::Out),
                "INOUT" => Some(ParamMode::InOut),
                "VARIADIC" => Some(ParamMode::Variadic),
                _ => None,
            }
        };

        if let Some(m) = is_mode(tokens[0]) {
            // Style A: [mode] [name] [type] or [mode] [type]
            mode = m;
            if tokens.len() >= 3 {
                name = tokens[1].to_string();
                type_str = tokens[2..].join(" ");
            } else if tokens.len() == 2 {
                name = format!("${}", pos + 1);
                type_str = tokens[1].to_string();
            }
        } else if tokens.len() >= 2 && is_mode(tokens[1]).is_some() {
            // Style B: [name] [mode] [type]
            name = tokens[0].to_string();
            mode = is_mode(tokens[1]).unwrap();
            type_str = tokens[2..].join(" ");
        } else if tokens.len() >= 2 {
            // Style C: [name] [type]
            name = tokens[0].to_string();
            type_str = tokens[1..].join(" ");
        } else {
            // Style D: [type]
            name = format!("${}", pos + 1);
            type_str = tokens[0].to_string();
        }

        let clean_name = name.trim_matches('"').trim_matches('\'').to_string();
        let data_type = parse_simple_datatype(&type_str);

        defs.push(ParamDef {
            name: clean_name,
            data_type,
            mode,
            default_val: None,
        });
    }

    Ok(defs)
}

fn split_param_parts(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0;

    for ch in s.chars() {
        if ch == '(' {
            depth += 1;
            current.push(ch);
        } else if ch == ')' {
            depth -= 1;
            current.push(ch);
        } else if ch == ',' && depth == 0 {
            parts.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(ch);
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

fn parse_simple_datatype(s: &str) -> DataType {
    let upper = s.to_uppercase();
    if upper.starts_with("INT") || upper.starts_with("INTEGER") {
        DataType::Integer
    } else if upper.starts_with("BIGINT") {
        DataType::BigInt
    } else if upper.starts_with("SMALLINT") {
        DataType::SmallInt
    } else if upper.starts_with("BOOL") {
        DataType::Boolean
    } else if upper.starts_with("FLOAT") || upper.starts_with("REAL") {
        DataType::Float
    } else if upper.starts_with("DOUBLE") {
        DataType::Double
    } else if upper.starts_with("NUMERIC") || upper.starts_with("DECIMAL") {
        DataType::Decimal(10, 2)
    } else if upper.starts_with("VARCHAR") || upper.starts_with("TEXT") || upper.starts_with("CHAR") {
        DataType::VarChar(None)
    } else if upper.starts_with("TIMESTAMP") {
        DataType::Timestamp
    } else if upper.starts_with("DATE") {
        DataType::Date
    } else {
        DataType::VarChar(None)
    }
}
