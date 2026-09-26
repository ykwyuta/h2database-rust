use h2_types::{H2Error, H2Result};
use crate::model::GraphValue;

#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub matches: Vec<MatchClause>,
    pub creates: Vec<CreateClause>,
    pub merges: Vec<MergeClause>,
    pub sets: Vec<SetClause>,
    pub deletes: Vec<DeleteClause>,
    pub return_clause: Option<ReturnClause>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchClause {
    pub optional: bool,
    pub path_var: Option<String>,
    pub pattern: Pattern,
    pub where_clause: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateClause {
    pub pattern: Pattern,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeClause {
    pub pattern: Pattern,
    pub on_create_sets: Vec<SetItem>,
    pub on_match_sets: Vec<SetItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetClause {
    pub items: Vec<SetItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetItem {
    Property {
        variable: String,
        property: String,
        value: Expr,
    },
    Label {
        variable: String,
        label: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeleteClause {
    pub detach: bool,
    pub variables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub start: NodePattern,
    pub chain: Vec<(EdgePattern, NodePattern)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodePattern {
    pub variable: Option<String>,
    pub labels: Vec<String>,
    pub properties: Vec<(String, Expr)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgePattern {
    pub variable: Option<String>,
    pub edge_types: Vec<String>,
    pub properties: Vec<(String, Expr)>,
    pub direction: Direction,
    pub var_length: Option<(Option<usize>, Option<usize>)>,
    pub shortest_path: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Outgoing, // -[]->
    Incoming, // <-[]-
    Both,     // -[]-
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnClause {
    pub distinct: bool,
    pub items: Vec<ReturnItem>,
    pub order_by: Vec<(Expr, bool)>,
    pub skip: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    pub expr: Expr,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(GraphValue),
    Parameter(String),
    Variable(String),
    PropertyAccess(String, String),
    BinaryOp {
        left: Box<Expr>,
        op: BinaryOperator,
        right: Box<Expr>,
    },
    UnaryOp {
        op: UnaryOperator,
        expr: Box<Expr>,
    },
    FunctionCall {
        name: String,
        args: Vec<Expr>,
    },
    List(Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    And,
    Or,
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    Not,
    Neg,
}

// ================= Lexer =================

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Match,
    Optional,
    Where,
    Return,
    Distinct,
    Create,
    Merge,
    Set,
    Delete,
    Detach,
    On,
    OrderBy,
    Asc,
    Desc,
    Skip,
    Limit,
    And,
    Or,
    Not,
    Ident(String),
    Param(String),
    StringLit(String),
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    NullLit,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Dot,
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    Plus,
    Minus,
    Star,
    Slash,
    Dash,
    ArrowRight, // ->
    ArrowLeft,  // <-
    DotDot,     // ..
}

struct Lexer<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.input[self.pos..].chars().next()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn starts_with(&self, s: &str) -> bool {
        self.input[self.pos..].starts_with(s)
    }

    fn tokenize(&mut self) -> H2Result<Vec<Token>> {
        let mut tokens = Vec::new();
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.next();
                continue;
            }
            if ch == '/' {
                self.next();
                if self.peek() == Some('/') {
                    // Line comment
                    self.next();
                    while let Some(c) = self.peek() {
                        self.next();
                        if c == '\n' { break; }
                    }
                    continue;
                } else {
                    tokens.push(Token::Slash);
                    continue;
                }
            }

            match ch {
                '(' => { self.next(); tokens.push(Token::LParen); }
                ')' => { self.next(); tokens.push(Token::RParen); }
                '[' => { self.next(); tokens.push(Token::LBracket); }
                ']' => { self.next(); tokens.push(Token::RBracket); }
                '{' => { self.next(); tokens.push(Token::LBrace); }
                '}' => { self.next(); tokens.push(Token::RBrace); }
                ':' => { self.next(); tokens.push(Token::Colon); }
                ',' => { self.next(); tokens.push(Token::Comma); }
                '+' => { self.next(); tokens.push(Token::Plus); }
                '*' => { self.next(); tokens.push(Token::Star); }
                '$' => {
                    self.next();
                    let name = self.read_ident()?;
                    tokens.push(Token::Param(name));
                }
                '.' => {
                    self.next();
                    if self.peek() == Some('.') {
                        self.next();
                        tokens.push(Token::DotDot);
                    } else {
                        tokens.push(Token::Dot);
                    }
                }
                '=' => { self.next(); tokens.push(Token::Eq); }
                '<' => {
                    self.next();
                    if self.peek() == Some('=') {
                        self.next();
                        tokens.push(Token::Lte);
                    } else if self.peek() == Some('>') {
                        self.next();
                        tokens.push(Token::Neq);
                    } else if self.peek() == Some('-') {
                        self.next();
                        tokens.push(Token::ArrowLeft);
                    } else {
                        tokens.push(Token::Lt);
                    }
                }
                '>' => {
                    self.next();
                    if self.peek() == Some('=') {
                        self.next();
                        tokens.push(Token::Gte);
                    } else {
                        tokens.push(Token::Gt);
                    }
                }
                '!' => {
                    self.next();
                    if self.peek() == Some('=') {
                        self.next();
                        tokens.push(Token::Neq);
                    } else {
                        return Err(H2Error::Execution("Unexpected '!'".to_string()));
                    }
                }
                '-' => {
                    self.next();
                    if self.peek() == Some('>') {
                        self.next();
                        tokens.push(Token::ArrowRight);
                    } else {
                        tokens.push(Token::Dash);
                    }
                }
                '\'' | '"' => {
                    tokens.push(Token::StringLit(self.read_string(ch)?));
                }
                '0'..='9' => {
                    tokens.push(self.read_number()?);
                }
                _ if ch.is_alphabetic() || ch == '_' => {
                    let ident = self.read_ident()?;
                    let upper = ident.to_uppercase();
                    let tok = match upper.as_str() {
                        "MATCH" => Token::Match,
                        "OPTIONAL" => Token::Optional,
                        "WHERE" => Token::Where,
                        "RETURN" => Token::Return,
                        "DISTINCT" => Token::Distinct,
                        "CREATE" => Token::Create,
                        "MERGE" => Token::Merge,
                        "SET" => Token::Set,
                        "DELETE" => Token::Delete,
                        "DETACH" => Token::Detach,
                        "ON" => Token::On,
                        "ORDER" => {
                            // Check for BY
                            self.skip_whitespace();
                            if let Some('B') | Some('b') = self.peek() {
                                let next_id = self.read_ident()?;
                                if next_id.eq_ignore_ascii_case("BY") {
                                    Token::OrderBy
                                } else {
                                    Token::Ident(ident)
                                }
                            } else {
                                Token::Ident(ident)
                            }
                        }
                        "ASC" => Token::Asc,
                        "DESC" => Token::Desc,
                        "SKIP" => Token::Skip,
                        "LIMIT" => Token::Limit,
                        "AND" => Token::And,
                        "OR" => Token::Or,
                        "NOT" => Token::Not,
                        "TRUE" => Token::BoolLit(true),
                        "FALSE" => Token::BoolLit(false),
                        "NULL" => Token::NullLit,
                        _ => Token::Ident(ident),
                    };
                    tokens.push(tok);
                }
                _ => {
                    return Err(H2Error::Execution(format!("Unexpected character: '{ch}'")));
                }
            }
        }
        Ok(tokens)
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.next();
            } else {
                break;
            }
        }
    }

    fn read_string(&mut self, quote: char) -> H2Result<String> {
        self.next(); // skip quote
        let mut s = String::new();
        while let Some(ch) = self.next() {
            if ch == quote {
                return Ok(s);
            }
            if ch == '\\' {
                if let Some(esc) = self.next() {
                    match esc {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        '\\' => s.push('\\'),
                        _ if esc == quote => s.push(quote),
                        _ => { s.push('\\'); s.push(esc); }
                    }
                    continue;
                }
            }
            s.push(ch);
        }
        Err(H2Error::Execution("Unterminated string literal".to_string()))
    }

    fn read_ident(&mut self) -> H2Result<String> {
        let mut s = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                s.push(ch);
                self.next();
            } else {
                break;
            }
        }
        Ok(s)
    }

    fn read_number(&mut self) -> H2Result<Token> {
        let mut s = String::new();
        let mut is_float = false;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                s.push(ch);
                self.next();
            } else if ch == '.' && !is_float {
                if self.starts_with("..") {
                    break;
                }
                is_float = true;
                s.push('.');
                self.next();
            } else {
                break;
            }
        }
        if is_float {
            s.parse::<f64>()
                .map(Token::FloatLit)
                .map_err(|e| H2Error::Execution(format!("Invalid float: {e}")))
        } else {
            s.parse::<i64>()
                .map(Token::IntLit)
                .map_err(|e| H2Error::Execution(format!("Invalid integer: {e}")))
        }
    }
}

// ================= Parser =================

pub type QueryParser = Parser;

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn parse(input: &str) -> H2Result<Query> {
        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize()?;
        let mut parser = Parser::new(tokens);
        parser.parse_query()
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, expected: Token) -> H2Result<()> {
        match self.next() {
            Some(tok) if tok == expected => Ok(()),
            Some(tok) => Err(H2Error::Execution(format!(
                "Expected {:?}, found {:?}",
                expected, tok
            ))),
            None => Err(H2Error::Execution(format!(
                "Expected {:?}, found EOF",
                expected
            ))),
        }
    }

    fn parse_query(&mut self) -> H2Result<Query> {
        let mut matches = Vec::new();
        let mut creates = Vec::new();
        let mut merges = Vec::new();
        let mut sets = Vec::new();
        let mut deletes = Vec::new();
        let mut return_clause = None;

        while let Some(tok) = self.peek() {
            match tok {
                Token::Match | Token::Optional => {
                    matches.extend(self.parse_matches()?);
                }
                Token::Create => {
                    creates.extend(self.parse_create()?);
                }
                Token::Merge => {
                    merges.push(self.parse_merge()?);
                }
                Token::Set => {
                    sets.push(self.parse_set()?);
                }
                Token::Delete | Token::Detach => {
                    deletes.push(self.parse_delete()?);
                }
                Token::Return => {
                    return_clause = Some(self.parse_return()?);
                    break;
                }
                _ => {
                    return Err(H2Error::Execution(format!("Unexpected token at top level: {:?}", tok)));
                }
            }
        }

        Ok(Query {
            matches,
            creates,
            merges,
            sets,
            deletes,
            return_clause,
        })
    }

    fn parse_matches(&mut self) -> H2Result<Vec<MatchClause>> {
        let optional = if self.peek() == Some(&Token::Optional) {
            self.next();
            true
        } else {
            false
        };
        self.expect(Token::Match)?;

        let mut path_var = None;
        // Check for `p = shortestPath(...)` or `p = (...)`
        if let Some(Token::Ident(name)) = self.peek().cloned() {
            if self.tokens.get(self.pos + 1) == Some(&Token::Eq) {
                path_var = Some(name);
                self.pos += 2; // skip name and '='
            }
        }

        let mut patterns = Vec::new();
        loop {
            patterns.push(self.parse_pattern()?);
            if self.peek() == Some(&Token::Comma) {
                self.next();
            } else {
                break;
            }
        }

        let where_clause = if self.peek() == Some(&Token::Where) {
            self.next();
            Some(self.parse_expr()?)
        } else {
            None
        };

        let num_patterns = patterns.len();
        let mut clauses = Vec::new();
        for (i, pattern) in patterns.into_iter().enumerate() {
            clauses.push(MatchClause {
                optional,
                path_var: if i == 0 { path_var.clone() } else { None },
                pattern,
                where_clause: if i == num_patterns - 1 {
                    where_clause.clone()
                } else {
                    None
                },
            });
        }
        Ok(clauses)
    }

    fn parse_create(&mut self) -> H2Result<Vec<CreateClause>> {
        self.expect(Token::Create)?;
        let mut list = Vec::new();
        loop {
            let pattern = self.parse_pattern()?;
            list.push(CreateClause { pattern });
            if self.peek() == Some(&Token::Comma) {
                self.next();
            } else {
                break;
            }
        }
        Ok(list)
    }

    fn parse_merge(&mut self) -> H2Result<MergeClause> {
        self.expect(Token::Merge)?;
        let pattern = self.parse_pattern()?;
        let mut on_create_sets = Vec::new();
        let mut on_match_sets = Vec::new();

        while self.peek() == Some(&Token::On) {
            self.next(); // ON
            match self.next() {
                Some(Token::Create) => {
                    self.expect(Token::Set)?;
                    on_create_sets.extend(self.parse_set_items()?);
                }
                Some(Token::Match) => {
                    self.expect(Token::Set)?;
                    on_match_sets.extend(self.parse_set_items()?);
                }
                _ => return Err(H2Error::Execution("Expected CREATE or MATCH after ON".to_string())),
            }
        }

        Ok(MergeClause {
            pattern,
            on_create_sets,
            on_match_sets,
        })
    }

    fn parse_set(&mut self) -> H2Result<SetClause> {
        self.expect(Token::Set)?;
        let items = self.parse_set_items()?;
        Ok(SetClause { items })
    }

    fn parse_set_items(&mut self) -> H2Result<Vec<SetItem>> {
        let mut items = Vec::new();
        loop {
            let var = match self.next() {
                Some(Token::Ident(name)) => name,
                other => return Err(H2Error::Execution(format!("Expected variable name in SET, found {:?}", other))),
            };

            if self.peek() == Some(&Token::Dot) {
                self.next(); // .
                let prop = match self.next() {
                    Some(Token::Ident(p)) => p,
                    other => return Err(H2Error::Execution(format!("Expected property name in SET, found {:?}", other))),
                };
                self.expect(Token::Eq)?;
                let value = self.parse_expr()?;
                items.push(SetItem::Property {
                    variable: var,
                    property: prop,
                    value,
                });
            } else if self.peek() == Some(&Token::Colon) {
                self.next(); // :
                let label = match self.next() {
                    Some(Token::Ident(l)) => l,
                    other => return Err(H2Error::Execution(format!("Expected label name in SET, found {:?}", other))),
                };
                items.push(SetItem::Label {
                    variable: var,
                    label,
                });
            } else {
                return Err(H2Error::Execution("Expected '.' or ':' after variable in SET".to_string()));
            }

            if self.peek() == Some(&Token::Comma) {
                self.next();
            } else {
                break;
            }
        }
        Ok(items)
    }

    fn parse_delete(&mut self) -> H2Result<DeleteClause> {
        let detach = if self.peek() == Some(&Token::Detach) {
            self.next();
            true
        } else {
            false
        };
        self.expect(Token::Delete)?;

        let mut variables = Vec::new();
        loop {
            match self.next() {
                Some(Token::Ident(v)) => variables.push(v),
                other => return Err(H2Error::Execution(format!("Expected variable in DELETE, found {:?}", other))),
            }
            if self.peek() == Some(&Token::Comma) {
                self.next();
            } else {
                break;
            }
        }
        Ok(DeleteClause { detach, variables })
    }

    fn parse_return(&mut self) -> H2Result<ReturnClause> {
        self.expect(Token::Return)?;
        let distinct = if self.peek() == Some(&Token::Distinct) {
            self.next();
            true
        } else {
            false
        };

        let mut items = Vec::new();
        loop {
            let expr = self.parse_expr()?;
            let alias = if let Some(Token::Ident(name)) = self.peek().cloned() {
                if name.eq_ignore_ascii_case("AS") {
                    self.next();
                    match self.next() {
                        Some(Token::Ident(a)) => Some(a),
                        _ => return Err(H2Error::Execution("Expected identifier after AS".to_string())),
                    }
                } else if !matches!(self.peek(), Some(Token::Comma) | Some(Token::OrderBy) | Some(Token::Skip) | Some(Token::Limit) | None) {
                    self.next();
                    Some(name)
                } else {
                    None
                }
            } else {
                None
            };
            items.push(ReturnItem { expr, alias });

            if self.peek() == Some(&Token::Comma) {
                self.next();
            } else {
                break;
            }
        }

        let mut order_by = Vec::new();
        if self.peek() == Some(&Token::OrderBy) {
            self.next();
            loop {
                let expr = self.parse_expr()?;
                let is_asc = if self.peek() == Some(&Token::Desc) {
                    self.next();
                    false
                } else {
                    if self.peek() == Some(&Token::Asc) {
                        self.next();
                    }
                    true
                };
                order_by.push((expr, is_asc));
                if self.peek() == Some(&Token::Comma) {
                    self.next();
                } else {
                    break;
                }
            }
        }

        let skip = if self.peek() == Some(&Token::Skip) {
            self.next();
            match self.next() {
                Some(Token::IntLit(n)) if n >= 0 => Some(n as usize),
                _ => return Err(H2Error::Execution("Expected non-negative integer after SKIP".to_string())),
            }
        } else {
            None
        };

        let limit = if self.peek() == Some(&Token::Limit) {
            self.next();
            match self.next() {
                Some(Token::IntLit(n)) if n >= 0 => Some(n as usize),
                _ => return Err(H2Error::Execution("Expected non-negative integer after LIMIT".to_string())),
            }
        } else {
            None
        };

        Ok(ReturnClause {
            distinct,
            items,
            order_by,
            skip,
            limit,
        })
    }

    // ================= Pattern Parser =================

    fn parse_pattern(&mut self) -> H2Result<Pattern> {
        // Check for shortestPath((...))
        if let Some(Token::Ident(name)) = self.peek().cloned() {
            if name.eq_ignore_ascii_case("shortestPath") {
                self.next();
                self.expect(Token::LParen)?;
                let mut pat = self.parse_pattern_chain()?;
                self.expect(Token::RParen)?;
                for (edge, _) in &mut pat.chain {
                    edge.shortest_path = true;
                }
                return Ok(pat);
            }
        }
        self.parse_pattern_chain()
    }

    fn parse_pattern_chain(&mut self) -> H2Result<Pattern> {
        let start = self.parse_node_pattern()?;
        let mut chain = Vec::new();

        while matches!(self.peek(), Some(Token::Dash) | Some(Token::ArrowLeft)) {
            let edge = self.parse_edge_pattern()?;
            let node = self.parse_node_pattern()?;
            chain.push((edge, node));
        }

        Ok(Pattern { start, chain })
    }

    fn parse_node_pattern(&mut self) -> H2Result<NodePattern> {
        self.expect(Token::LParen)?;
        let mut node = NodePattern::default();

        if let Some(Token::Ident(name)) = self.peek().cloned() {
            node.variable = Some(name);
            self.next();
        }

        while self.peek() == Some(&Token::Colon) {
            self.next();
            match self.next() {
                Some(Token::Ident(label)) => node.labels.push(label),
                other => return Err(H2Error::Execution(format!("Expected label after ':', found {:?}", other))),
            }
        }

        if self.peek() == Some(&Token::LBrace) {
            node.properties = self.parse_properties_map()?;
        }

        self.expect(Token::RParen)?;
        Ok(node)
    }

    fn parse_edge_pattern(&mut self) -> H2Result<EdgePattern> {
        let is_incoming = if self.peek() == Some(&Token::ArrowLeft) {
            self.next();
            true
        } else {
            self.expect(Token::Dash)?;
            false
        };

        let mut variable = None;
        let mut edge_types = Vec::new();
        let mut properties = Vec::new();
        let mut var_length = None;

        if self.peek() == Some(&Token::LBracket) {
            self.next();
            if let Some(Token::Ident(name)) = self.peek().cloned() {
                variable = Some(name);
                self.next();
            }

            while self.peek() == Some(&Token::Colon) {
                self.next();
                match self.next() {
                    Some(Token::Ident(t)) => edge_types.push(t),
                    other => return Err(H2Error::Execution(format!("Expected relationship type after ':', found {:?}", other))),
                }
            }

            // Variable length pattern: `*` or `*1..3`
            if self.peek() == Some(&Token::Star) {
                self.next();
                let min = if let Some(Token::IntLit(n)) = self.peek().cloned() {
                    self.next();
                    Some(n as usize)
                } else {
                    None
                };
                let max = if self.peek() == Some(&Token::DotDot) {
                    self.next();
                    if let Some(Token::IntLit(n)) = self.peek().cloned() {
                        self.next();
                        Some(n as usize)
                    } else {
                        None
                    }
                } else {
                    min
                };
                var_length = Some((min, max));
            }

            if self.peek() == Some(&Token::LBrace) {
                properties = self.parse_properties_map()?;
            }

            self.expect(Token::RBracket)?;
        }

        let direction = if self.peek() == Some(&Token::ArrowRight) {
            self.next();
            if is_incoming {
                return Err(H2Error::Execution("Relationship cannot be bidirectional '<-[]->'".to_string()));
            }
            Direction::Outgoing
        } else if self.peek() == Some(&Token::Dash) {
            self.next();
            if is_incoming {
                Direction::Incoming
            } else {
                Direction::Both
            }
        } else {
            return Err(H2Error::Execution("Expected '->' or '-' to close relationship pattern".to_string()));
        };

        Ok(EdgePattern {
            variable,
            edge_types,
            properties,
            direction,
            var_length,
            shortest_path: false,
        })
    }

    fn parse_properties_map(&mut self) -> H2Result<Vec<(String, Expr)>> {
        self.expect(Token::LBrace)?;
        let mut props = Vec::new();
        if self.peek() == Some(&Token::RBrace) {
            self.next();
            return Ok(props);
        }

        loop {
            let key = match self.next() {
                Some(Token::Ident(k)) => k,
                other => return Err(H2Error::Execution(format!("Expected property key in map, found {:?}", other))),
            };
            self.expect(Token::Colon)?;
            let val_expr = self.parse_expr()?;
            props.push((key, val_expr));

            if self.peek() == Some(&Token::Comma) {
                self.next();
            } else {
                break;
            }
        }
        self.expect(Token::RBrace)?;
        Ok(props)
    }

    // ================= Expression Parser =================

    pub fn parse_expr(&mut self) -> H2Result<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> H2Result<Expr> {
        let mut left = self.parse_and()?;
        while self.peek() == Some(&Token::Or) {
            self.next();
            let right = self.parse_and()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::Or,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> H2Result<Expr> {
        let mut left = self.parse_not()?;
        while self.peek() == Some(&Token::And) {
            self.next();
            let right = self.parse_not()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> H2Result<Expr> {
        if self.peek() == Some(&Token::Not) {
            self.next();
            let expr = self.parse_comparison()?;
            Ok(Expr::UnaryOp {
                op: UnaryOperator::Not,
                expr: Box::new(expr),
            })
        } else {
            self.parse_comparison()
        }
    }

    fn parse_comparison(&mut self) -> H2Result<Expr> {
        let left = self.parse_add_sub()?;
        let op = match self.peek() {
            Some(Token::Eq) => BinaryOperator::Eq,
            Some(Token::Neq) => BinaryOperator::Neq,
            Some(Token::Lt) => BinaryOperator::Lt,
            Some(Token::Lte) => BinaryOperator::Lte,
            Some(Token::Gt) => BinaryOperator::Gt,
            Some(Token::Gte) => BinaryOperator::Gte,
            _ => return Ok(left),
        };
        self.next();
        let right = self.parse_add_sub()?;
        Ok(Expr::BinaryOp {
            left: Box::new(left),
            op,
            right: Box::new(right),
        })
    }

    fn parse_add_sub(&mut self) -> H2Result<Expr> {
        let mut left = self.parse_mul_div()?;
        while let Some(tok) = self.peek() {
            let op = match tok {
                Token::Plus => BinaryOperator::Add,
                Token::Minus | Token::Dash => BinaryOperator::Sub,
                _ => break,
            };
            self.next();
            let right = self.parse_mul_div()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_mul_div(&mut self) -> H2Result<Expr> {
        let mut left = self.parse_unary()?;
        while let Some(tok) = self.peek() {
            let op = match tok {
                Token::Star => BinaryOperator::Mul,
                Token::Slash => BinaryOperator::Div,
                _ => break,
            };
            self.next();
            let right = self.parse_unary()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> H2Result<Expr> {
        if self.peek() == Some(&Token::Minus) || self.peek() == Some(&Token::Dash) {
            self.next();
            let expr = self.parse_primary()?;
            Ok(Expr::UnaryOp {
                op: UnaryOperator::Neg,
                expr: Box::new(expr),
            })
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> H2Result<Expr> {
        let tok = self.next().ok_or_else(|| H2Error::Execution("Unexpected end of expression".to_string()))?;
        match tok {
            Token::NullLit => Ok(Expr::Literal(GraphValue::Null)),
            Token::BoolLit(b) => Ok(Expr::Literal(GraphValue::Boolean(b))),
            Token::IntLit(i) => Ok(Expr::Literal(GraphValue::Integer(i))),
            Token::FloatLit(f) => Ok(Expr::Literal(GraphValue::Float(f))),
            Token::StringLit(s) => Ok(Expr::Literal(GraphValue::String(s))),
            Token::Param(p) => Ok(Expr::Parameter(p)),
            Token::LParen => {
                let expr = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            Token::LBracket => {
                let mut list = Vec::new();
                if self.peek() != Some(&Token::RBracket) {
                    loop {
                        list.push(self.parse_expr()?);
                        if self.peek() == Some(&Token::Comma) {
                            self.next();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(Token::RBracket)?;
                Ok(Expr::List(list))
            }
            Token::Ident(name) => {
                if self.peek() == Some(&Token::LParen) {
                    // Function call: count(*), length(p), id(n), labels(n), etc.
                    self.next();
                    let mut args = Vec::new();
                    if self.peek() == Some(&Token::Star) {
                        self.next();
                        args.push(Expr::Literal(GraphValue::String("*".to_string())));
                    } else if self.peek() != Some(&Token::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if self.peek() == Some(&Token::Comma) {
                                self.next();
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(Token::RParen)?;
                    Ok(Expr::FunctionCall { name, args })
                } else if self.peek() == Some(&Token::Dot) {
                    self.next();
                    match self.next() {
                        Some(Token::Ident(prop)) => Ok(Expr::PropertyAccess(name, prop)),
                        other => Err(H2Error::Execution(format!("Expected property name after '.', found {:?}", other))),
                    }
                } else {
                    Ok(Expr::Variable(name))
                }
            }
            other => Err(H2Error::Execution(format!("Unexpected token in expression: {:?}", other))),
        }
    }
}
