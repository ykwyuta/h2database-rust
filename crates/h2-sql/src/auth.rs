use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use h2_types::{H2Error, H2Result};

/// テーブル操作に対する権限種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Privilege {
    Select,
    Insert,
    Update,
    Delete,
    All,
}

impl std::fmt::Display for Privilege {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Privilege::Select => write!(f, "SELECT"),
            Privilege::Insert => write!(f, "INSERT"),
            Privilege::Update => write!(f, "UPDATE"),
            Privilege::Delete => write!(f, "DELETE"),
            Privilege::All => write!(f, "ALL"),
        }
    }
}

impl FromStr for Privilege {
    type Err = H2Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_uppercase().as_str() {
            "SELECT" => Ok(Privilege::Select),
            "INSERT" => Ok(Privilege::Insert),
            "UPDATE" => Ok(Privilege::Update),
            "DELETE" => Ok(Privilege::Delete),
            "ALL" | "ALL PRIVILEGES" => Ok(Privilege::All),
            other => Err(H2Error::SqlParse(format!("Unknown privilege type: {}", other))),
        }
    }
}

/// ユーザーアカウント情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub username: String,
    pub password: Option<String>,
    pub allowed_hosts: Vec<String>,
    pub is_superuser: bool,
    pub table_privileges: HashMap<String, HashSet<Privilege>>,
}

impl UserInfo {
    pub fn new(username: &str, password: Option<&str>, allowed_host: Option<&str>, is_superuser: bool) -> Self {
        let hosts = match allowed_host {
            Some(h) if !h.trim().is_empty() => vec![h.trim().to_string()],
            _ => vec!["%".to_string()],
        };

        Self {
            username: username.to_string(),
            password: password.map(|p| p.to_string()),
            allowed_hosts: hosts,
            is_superuser,
            table_privileges: HashMap::new(),
        }
    }

    /// クライアント IP アドレスまたはホスト名がアクセス許可リストに合致するか判定
    pub fn matches_host(&self, client_ip: &str) -> bool {
        let trimmed_ip = client_ip.trim();

        for host in &self.allowed_hosts {
            let h = host.trim();
            // ワイルドカードまたは 'any'
            if h == "%" || h == "*" || h.eq_ignore_ascii_case("any") {
                return true;
            }

            // localhost 判定
            if h.eq_ignore_ascii_case("localhost") {
                if trimmed_ip == "127.0.0.1" || trimmed_ip == "::1" || trimmed_ip.eq_ignore_ascii_case("localhost") {
                    return true;
                }
            }

            // IP アドレスの完全一致
            if h == trimmed_ip {
                return true;
            }

            // CIDR プレフィックス判定 (例: 192.168.1.0/24)
            if let Some((cidr_ip, prefix_len)) = parse_cidr(h) {
                if match_cidr(cidr_ip, prefix_len, trimmed_ip) {
                    return true;
                }
            }
        }

        false
    }

    /// 対象テーブルに対する指定権限を保持しているか確認
    pub fn has_table_privilege(&self, table: &str, required: Privilege) -> bool {
        if self.is_superuser {
            return true;
        }

        let table_key = table.to_lowercase();
        if let Some(privs) = self.table_privileges.get(&table_key) {
            if privs.contains(&Privilege::All) || privs.contains(&required) {
                return true;
            }
        }

        // "*" (すべてのテーブルに対する権限) のチェック
        if let Some(privs) = self.table_privileges.get("*") {
            if privs.contains(&Privilege::All) || privs.contains(&required) {
                return true;
            }
        }

        false
    }
}

/// 認証および権限マネージャー
#[derive(Debug, Clone)]
pub struct AuthManager {
    users: Arc<RwLock<HashMap<String, UserInfo>>>,
    auth_enabled: Arc<AtomicBool>,
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthManager {
    pub fn new() -> Self {
        let mut users = HashMap::new();

        // デフォルトのスーパーユーザー (admin, postgres, sa, root, app) を初期登録（後方互換性のため）
        for name in &["admin", "postgres", "sa", "root", "app"] {
            let u = UserInfo::new(name, None, Some("%"), true);
            users.insert(name.to_string(), u);
        }

        Self {
            users: Arc::new(RwLock::new(users)),
            auth_enabled: Arc::new(AtomicBool::new(true)),
        }
    }

    /// 認証の有効/無効を切り替え
    pub fn set_auth_enabled(&self, enabled: bool) {
        self.auth_enabled.store(enabled, Ordering::SeqCst);
    }

    pub fn is_auth_enabled(&self) -> bool {
        self.auth_enabled.load(Ordering::SeqCst)
    }

    /// ユーザーの新規作成
    pub fn create_user(&self, username: &str, password: Option<&str>, allowed_host: Option<&str>, is_superuser: bool) -> H2Result<()> {
        let username_lower = username.to_lowercase();
        let mut users = self.users.write();

        if users.contains_key(&username_lower) {
            return Err(H2Error::Execution(format!("User '{}' already exists", username)));
        }

        let user = UserInfo::new(&username_lower, password, allowed_host, is_superuser);
        users.insert(username_lower, user);
        Ok(())
    }

    /// ユーザー情報の変更 (パスワードや許可ホスト)
    pub fn alter_user(&self, username: &str, password: Option<&str>, allowed_host: Option<&str>) -> H2Result<()> {
        let username_lower = username.to_lowercase();
        let mut users = self.users.write();

        let user = users.get_mut(&username_lower).ok_or_else(|| {
            H2Error::Execution(format!("User '{}' does not exist", username))
        })?;

        if let Some(p) = password {
            user.password = Some(p.to_string());
        }
        if let Some(h) = allowed_host {
            user.allowed_hosts = vec![h.to_string()];
        }

        Ok(())
    }

    /// ユーザーの削除
    pub fn drop_user(&self, username: &str, if_exists: bool) -> H2Result<()> {
        let username_lower = username.to_lowercase();
        let mut users = self.users.write();

        if username_lower == "admin" || username_lower == "postgres" {
            return Err(H2Error::Execution(format!("Cannot drop system default superuser '{}'", username)));
        }

        if users.remove(&username_lower).is_none() && !if_exists {
            return Err(H2Error::Execution(format!("User '{}' does not exist", username)));
        }

        Ok(())
    }

    /// ユーザー認証 & アクセス元ホストの検証
    pub fn authenticate(&self, username: &str, password: Option<&str>, client_ip: &str) -> H2Result<UserInfo> {
        let username_lower = username.to_lowercase();
        let users = self.users.read();

        let user = users.get(&username_lower).ok_or_else(|| {
            H2Error::Authentication(format!("User '{}' does not exist", username))
        })?;

        // 1. アクセス元ホストの検証
        if !user.matches_host(client_ip) {
            return Err(H2Error::Authentication(format!(
                "Access denied for user '{}' from host '{}' (host restriction)",
                username, client_ip
            )));
        }

        // 2. パスワードの検証 (パスワードが設定されている場合)
        if let Some(ref expected_pw) = user.password {
            match password {
                Some(provided_pw) if provided_pw == expected_pw => {}
                _ => return Err(H2Error::Authentication(format!("Password authentication failed for user '{}'", username))),
            }
        }

        Ok(user.clone())
    }

    /// テーブル権限の付与 (GRANT)
    pub fn grant(&self, username: &str, table: &str, privileges: Vec<Privilege>) -> H2Result<()> {
        let username_lower = username.to_lowercase();
        let mut users = self.users.write();

        let user = users.get_mut(&username_lower).ok_or_else(|| {
            H2Error::Execution(format!("User '{}' does not exist", username))
        })?;

        let table_key = table.to_lowercase();
        let priv_set = user.table_privileges.entry(table_key).or_default();
        for p in privileges {
            priv_set.insert(p);
        }

        Ok(())
    }

    /// テーブル権限の剥奪 (REVOKE)
    pub fn revoke(&self, username: &str, table: &str, privileges: Vec<Privilege>) -> H2Result<()> {
        let username_lower = username.to_lowercase();
        let mut users = self.users.write();

        let user = users.get_mut(&username_lower).ok_or_else(|| {
            H2Error::Execution(format!("User '{}' does not exist", username))
        })?;

        let table_key = table.to_lowercase();
        if let Some(priv_set) = user.table_privileges.get_mut(&table_key) {
            let revoke_all = privileges.contains(&Privilege::All);
            if revoke_all {
                priv_set.clear();
            } else {
                for p in &privileges {
                    priv_set.remove(p);
                }
            }
        }

        Ok(())
    }

    /// 指定ユーザーのテーブル操作権限を検証
    pub fn check_privilege(&self, username: &str, table: &str, required: Privilege) -> H2Result<()> {
        if !self.is_auth_enabled() {
            return Ok(());
        }

        let username_lower = username.to_lowercase();
        let users = self.users.read();

        let user = users.get(&username_lower).ok_or_else(|| {
            H2Error::PermissionDenied(format!("User '{}' does not exist", username))
        })?;

        if !user.has_table_privilege(table, required) {
            return Err(H2Error::PermissionDenied(format!(
                "Permission denied: User '{}' does not have '{}' privilege on table '{}'",
                username, required, table
            )));
        }

        Ok(())
    }

    /// 登録全ユーザーの一覧を取得
    pub fn list_users(&self) -> Vec<UserInfo> {
        let users = self.users.read();
        users.values().cloned().collect()
    }

    /// 特定ユーザーの保持権限一覧を取得
    pub fn get_user_grants(&self, username: &str) -> H2Result<Vec<(String, Vec<Privilege>)>> {
        let username_lower = username.to_lowercase();
        let users = self.users.read();

        let user = users.get(&username_lower).ok_or_else(|| {
            H2Error::Execution(format!("User '{}' does not exist", username))
        })?;

        let mut res = Vec::new();
        for (table, privs) in &user.table_privileges {
            let mut list: Vec<Privilege> = privs.iter().copied().collect();
            list.sort_by_key(|p| p.to_string());
            res.push((table.clone(), list));
        }
        res.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(res)
    }
}

// ---------------------------------------------------------------------------
// CIDR 判定ヘルパー関数
// ---------------------------------------------------------------------------
fn parse_cidr(s: &str) -> Option<(Ipv4Addr, u8)> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        return None;
    }

    let ip: Ipv4Addr = parts[0].trim().parse().ok()?;
    let prefix_len: u8 = parts[1].trim().parse().ok()?;
    if prefix_len > 32 {
        return None;
    }

    Some((ip, prefix_len))
}

fn match_cidr(net_ip: Ipv4Addr, prefix_len: u8, client_ip_str: &str) -> bool {
    let client_ip: Ipv4Addr = match client_ip_str.parse() {
        Ok(IpAddr::V4(ipv4)) => ipv4,
        _ => return false,
    };

    if prefix_len == 0 {
        return true;
    }

    let mask = !((1u32 << (32 - prefix_len)) - 1);
    let net_u32 = u32::from(net_ip) & mask;
    let client_u32 = u32::from(client_ip) & mask;

    net_u32 == client_u32
}
