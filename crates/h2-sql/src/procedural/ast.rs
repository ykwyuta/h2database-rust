use serde::{Deserialize, Serialize};
use h2_types::{DataType, Value};

/// ルーチンの種類（関数またはプロシージャ）
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RoutineKind {
    Function,
    Procedure,
}

impl RoutineKind {
    pub fn as_char(&self) -> char {
        match self {
            RoutineKind::Function => 'f',
            RoutineKind::Procedure => 'p',
        }
    }
}

/// 手続き言語の種類
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RoutineLanguage {
    PlPgSql,
    TSql,
    Sql,
}

/// 引数の方向モード
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ParamMode {
    In,
    Out,
    InOut,
    Variadic,
}

/// 引数定義
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParamDef {
    pub name: String,
    pub data_type: DataType,
    pub mode: ParamMode,
    pub default_val: Option<ProcExpr>,
}

/// ルーチン定義（関数／プロシージャ）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutineDef {
    pub name: String,
    pub schema: Option<String>,
    pub kind: RoutineKind,
    pub language: RoutineLanguage,
    pub parameters: Vec<ParamDef>,
    pub return_type: Option<DataType>,
    pub is_strict: bool,
    pub security_definer: bool,
    pub body: ProcBlock,
    pub source_sql: String,
}

/// PL/pgSQL / T-SQL 共通のブロック構造
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProcBlock {
    pub label: Option<String>,
    pub declarations: Vec<VarDecl>,
    pub statements: Vec<ProcStmt>,
    pub exception_handlers: Vec<ExceptionHandler>,
}

/// ローカル変数宣言
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VarDecl {
    pub name: String,
    pub data_type: DataType,
    pub default: Option<ProcExpr>,
    pub not_null: bool,
    /// $1, $2 等の引数エイリアス（1-indexed）
    pub alias_for_pos: Option<usize>,
}

/// RAISE / エラーレベル
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RaiseLevel {
    Notice,
    Warning,
    Info,
    Exception,
}

/// 共通手続き文（Canonical Procedural Statement）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProcStmt {
    /// 変数代入: target := expr; または target = expr; / SET @target = expr;
    Assign {
        target: String,
        expr: ProcExpr,
    },
    /// 条件分岐: IF cond THEN ... ELSIF cond THEN ... ELSE ... END IF;
    If {
        branches: Vec<(ProcExpr, Vec<ProcStmt>)>,
        else_branch: Option<Vec<ProcStmt>>,
    },
    /// WHILE ループ: WHILE cond LOOP ... END LOOP;
    While {
        condition: ProcExpr,
        body: Vec<ProcStmt>,
        label: Option<String>,
    },
    /// FOR 整数範囲ループ: FOR i IN [REVERSE] start..end [BY step] LOOP ... END LOOP;
    ForRange {
        var_name: String,
        start: ProcExpr,
        end: ProcExpr,
        step: Option<ProcExpr>,
        reverse: bool,
        body: Vec<ProcStmt>,
    },
    /// 無条件 LOOP ... END LOOP;
    Loop {
        body: Vec<ProcStmt>,
        label: Option<String>,
    },
    /// ループ脱出: EXIT [label] [WHEN cond]; / BREAK;
    Exit {
        condition: Option<ProcExpr>,
        label: Option<String>,
    },
    /// ループ継続: CONTINUE [label] [WHEN cond]; / CONTINUE;
    Continue {
        condition: Option<ProcExpr>,
        label: Option<String>,
    },
    /// 返却: RETURN [expr];
    Return {
        value: Option<ProcExpr>,
    },
    /// 集合返却: RETURN NEXT expr;
    ReturnNext {
        value: ProcExpr,
    },
    /// 問合せ結果返却: RETURN QUERY query;
    ReturnQuery {
        query: String,
    },
    /// メッセージ/例外送出: RAISE level 'format', ...; / THROW / PRINT
    Raise {
        level: RaiseLevel,
        message: String,
        params: Vec<ProcExpr>,
    },
    /// 式/クエリ実行（結果破棄）: PERFORM expr;
    Perform {
        expr: ProcExpr,
    },
    /// 問合せ代入: SELECT expr1, expr2 INTO [STRICT] var1, var2 FROM ...;
    SelectInto {
        targets: Vec<String>,
        query: String,
        strict: bool,
    },
    /// 動的 SQL 実行: EXECUTE sql_expr [INTO var1, ...] [USING p1, ...]
    ExecuteDynamic {
        query_expr: ProcExpr,
        into_targets: Vec<String>,
        using_params: Vec<ProcExpr>,
    },
    /// 生の SQL 実行 (DML/DDL): INSERT, UPDATE, DELETE など（変数は置換される）
    SqlStmt {
        sql: String,
    },
    /// ネストされたブロック: [DECLARE ...] BEGIN ... END;
    Block(ProcBlock),
    /// 空文: NULL;
    Null,
}

/// EXCEPTION ハンドラ
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExceptionHandler {
    pub condition: String, // "OTHERS", "NO_DATA_FOUND", etc.
    pub statements: Vec<ProcStmt>,
}

/// 二項演算子
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProcBinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Concat,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    Like,
}

/// 単項演算子
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProcUnaryOp {
    Not,
    Neg,
}

/// 手続き式（Canonical Procedural Expression）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProcExpr {
    Literal(Value),
    Variable(String),
    PositionalArg(usize), // $1, $2, ...
    Unary {
        op: ProcUnaryOp,
        expr: Box<ProcExpr>,
    },
    Binary {
        left: Box<ProcExpr>,
        op: ProcBinaryOp,
        right: Box<ProcExpr>,
    },
    FunctionCall {
        name: String,
        args: Vec<ProcExpr>,
    },
    IsNull(Box<ProcExpr>),
    IsNotNull(Box<ProcExpr>),
}
