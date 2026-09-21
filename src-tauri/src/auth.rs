use chrono::{DateTime, Utc};
use domain::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
use zeroize::Zeroizing;

const PRODUCT_PATH: &str = "/auth/products/data-desensitization/sessions";
const LEGACY_CREDENTIAL_SERVICE: &str = "LocalDesensitization";
const LEGACY_SESSION_CREDENTIAL: &str = "auth-session-v1";
const LEGACY_DEVICE_CREDENTIAL: &str = "device-id-v1";
const CLOCK_ROLLBACK_TOLERANCE_SECONDS: i64 = 300;
const AUTH_STATE_SCHEMA: u32 = 1;
const AUTH_STATE_AAD: &[u8] = b"sixa-auth-state-v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthSubject {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub tenant_id: String,
    pub tenant_code: String,
    pub tenant_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProductPolicy {
    pub product_code: String,
    pub module_enabled: bool,
    pub concurrent_device_limit: u32,
    pub active_session_count: u32,
    pub heartbeat_interval_seconds: u64,
    pub offline_grace_seconds: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthStatus {
    pub authenticated: bool,
    pub offline: bool,
    pub offline_until: Option<i64>,
    pub subject: Option<AuthSubject>,
    pub policy: Option<ProductPolicy>,
    pub reason: Option<String>,
}

impl AuthStatus {
    fn signed_out(reason: Option<String>) -> Self {
        Self {
            authenticated: false,
            offline: false,
            offline_until: None,
            subject: None,
            policy: None,
            reason,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredSession {
    token: String,
    subject: AuthSubject,
    policy: ProductPolicy,
    offline_until: i64,
    last_observed_at: i64,
}

#[derive(Serialize, Deserialize)]
struct StoredAuthState {
    schema: u32,
    device_id: String,
    session: Option<StoredSession>,
}

struct AuthPersistence {
    path: PathBuf,
    key: Zeroizing<[u8; 32]>,
    device_id: String,
}

impl AuthPersistence {
    fn open(
        root: &Path,
        key: Zeroizing<[u8; 32]>,
        migrate_legacy: bool,
    ) -> Result<(Self, Option<StoredSession>)> {
        std::fs::create_dir_all(root.join("db"))?;
        let path = root.join("db/auth-state.enc");
        let state = if path.is_file() {
            let encrypted = std::fs::read(&path)?;
            let plain = Zeroizing::new(storage::vault::open(&key, &encrypted, AUTH_STATE_AAD)?);
            let state: StoredAuthState =
                serde_json::from_slice(&plain).map_err(|_| Error::Authentication)?;
            validate_auth_state(&state)?;
            state
        } else if migrate_legacy {
            read_legacy_auth_state().unwrap_or_else(new_auth_state)
        } else {
            new_auth_state()
        };
        let persistence = Self {
            path,
            key,
            device_id: state.device_id,
        };
        if !persistence.path.is_file() {
            persistence.write_session(state.session.as_ref())?;
        }
        Ok((persistence, state.session))
    }

    fn write_session(&self, session: Option<&StoredSession>) -> Result<()> {
        let state = StoredAuthState {
            schema: AUTH_STATE_SCHEMA,
            device_id: self.device_id.clone(),
            session: session.cloned(),
        };
        let plain = Zeroizing::new(
            serde_json::to_vec(&state).map_err(|error| Error::Io(error.to_string()))?,
        );
        let encrypted = storage::vault::seal(&self.key, &plain, AUTH_STATE_AAD)?;
        storage::atomic_write(&self.path, &encrypted)
    }
}

fn new_auth_state() -> StoredAuthState {
    StoredAuthState {
        schema: AUTH_STATE_SCHEMA,
        device_id: uuid::Uuid::new_v4().to_string(),
        session: None,
    }
}

fn read_legacy_auth_state() -> Option<StoredAuthState> {
    let session = match keyring::Entry::new(LEGACY_CREDENTIAL_SERVICE, LEGACY_SESSION_CREDENTIAL)
        .ok()?
        .get_secret()
    {
        Ok(secret) => serde_json::from_slice(&secret).ok(),
        Err(keyring::Error::NoEntry) => None,
        Err(_) => return None,
    };
    let device_id = match keyring::Entry::new(LEGACY_CREDENTIAL_SERVICE, LEGACY_DEVICE_CREDENTIAL)
        .ok()?
        .get_secret()
    {
        Ok(secret) => String::from_utf8(secret)
            .ok()
            .filter(|value| uuid::Uuid::parse_str(value).is_ok()),
        Err(keyring::Error::NoEntry) => None,
        Err(_) => return None,
    };
    if session.is_none() && device_id.is_none() {
        return None;
    }
    Some(StoredAuthState {
        schema: AUTH_STATE_SCHEMA,
        device_id: device_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        session,
    })
}

fn validate_auth_state(state: &StoredAuthState) -> Result<()> {
    if state.schema != AUTH_STATE_SCHEMA || uuid::Uuid::parse_str(&state.device_id).is_err() {
        return Err(Error::Authentication);
    }
    Ok(())
}

#[derive(Serialize)]
struct LoginRequest<'a> {
    tenant_code: &'a str,
    username: &'a str,
    password: &'a str,
}

#[derive(Deserialize)]
struct LoginResponse {
    access_token: String,
}

#[derive(Serialize)]
struct CreateSessionRequest<'a> {
    device_id: &'a str,
    device_name: &'a str,
    app_version: &'a str,
}

#[derive(Deserialize)]
struct CreateSessionResponse {
    session_token: String,
    subject: AuthSubject,
    policy: ProductPolicy,
    offline_until: DateTime<Utc>,
}

#[derive(Deserialize)]
struct HeartbeatResponse {
    subject: AuthSubject,
    policy: ProductPolicy,
    offline_until: DateTime<Utc>,
}

#[derive(Debug)]
enum RequestFailure {
    Rejected(String),
    Unavailable(String),
    Network(String),
}

pub struct AuthManager {
    client: reqwest::Client,
    api_base: String,
    session: Mutex<Option<StoredSession>>,
    operation: tokio::sync::Mutex<()>,
    persistence: AuthPersistence,
}

impl AuthManager {
    pub fn new(root: &Path, key: Zeroizing<[u8; 32]>) -> Result<Self> {
        let api_base = option_env!("LOCAL_DESENSITIZATION_AUTH_API_BASE")
            .unwrap_or("https://dongdongkc.shierkeji.com:6201/ua2/api/v1")
            .trim_end_matches('/')
            .to_owned();
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
            .user_agent(concat!("Sixa/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| Error::Io(error.to_string()))?;
        let (persistence, session) = AuthPersistence::open(root, key, true)?;
        Ok(Self {
            client,
            api_base,
            session: Mutex::new(session),
            operation: tokio::sync::Mutex::new(()),
            persistence,
        })
    }

    pub fn require_authenticated(&self) -> Result<()> {
        let now = unix_time();
        let session = self.session.lock().map_err(poisoned)?;
        let session = session
            .as_ref()
            .ok_or_else(|| Error::State("请先登录后再使用私匣".into()))?;
        match offline_lease(now, session.last_observed_at, session.offline_until) {
            LeaseState::Valid => Ok(()),
            LeaseState::ClockRollback => Err(Error::State(
                "检测到系统时间异常，请联网校验登录状态".into(),
            )),
            LeaseState::Expired => Err(Error::State(
                "离线使用期限已结束，请联网校验登录状态".into(),
            )),
        }
    }

    pub async fn status(&self) -> Result<AuthStatus> {
        let _guard = self.operation.lock().await;
        let Some(session) = self.session.lock().map_err(poisoned)?.clone() else {
            return Ok(AuthStatus::signed_out(None));
        };
        self.refresh_or_use_offline(session).await
    }

    pub async fn login(
        &self,
        tenant_code: String,
        username: String,
        password: String,
    ) -> Result<AuthStatus> {
        let _guard = self.operation.lock().await;
        let tenant_code = tenant_code.trim();
        let username = username.trim();
        if tenant_code.is_empty() || username.is_empty() || password.is_empty() {
            return Err(Error::Invalid("请输入租户编码、用户名和密码".into()));
        }
        let password = Zeroizing::new(password);
        let login = self
            .client
            .post(format!("{}/auth/tenant-user/login", self.api_base))
            .json(&LoginRequest {
                tenant_code,
                username,
                password: password.as_str(),
            })
            .send()
            .await
            .map_err(network_error)?;
        let login: LoginResponse = decode_response(login).await.map_err(request_error)?;
        let access_token = Zeroizing::new(login.access_token);
        let device_id = &self.persistence.device_id;
        let device_name = device_name();
        let created = self
            .client
            .post(format!("{}{}", self.api_base, PRODUCT_PATH))
            .bearer_auth(access_token.as_str())
            .json(&CreateSessionRequest {
                device_id,
                device_name: &device_name,
                app_version: env!("CARGO_PKG_VERSION"),
            })
            .send()
            .await
            .map_err(network_error)?;
        let created: CreateSessionResponse =
            decode_response(created).await.map_err(request_error)?;
        let now = unix_time();
        let stored = StoredSession {
            token: created.session_token,
            subject: created.subject,
            policy: created.policy,
            offline_until: created.offline_until.timestamp(),
            last_observed_at: now,
        };
        self.persistence.write_session(Some(&stored))?;
        *self.session.lock().map_err(poisoned)? = Some(stored.clone());
        Ok(status_from_session(&stored, false))
    }

    pub async fn heartbeat(&self) -> Result<AuthStatus> {
        let _guard = self.operation.lock().await;
        let Some(session) = self.session.lock().map_err(poisoned)?.clone() else {
            return Ok(AuthStatus::signed_out(None));
        };
        self.refresh_or_use_offline(session).await
    }

    pub async fn logout(&self) -> Result<()> {
        let _guard = self.operation.lock().await;
        let session = self.session.lock().map_err(poisoned)?.clone();
        if let Some(session) = session {
            let _ = self
                .client
                .delete(format!("{}{}/current", self.api_base, PRODUCT_PATH))
                .bearer_auth(&session.token)
                .send()
                .await;
        }
        self.persistence.write_session(None)?;
        *self.session.lock().map_err(poisoned)? = None;
        Ok(())
    }

    async fn refresh_or_use_offline(&self, session: StoredSession) -> Result<AuthStatus> {
        let now = unix_time();
        let lease = offline_lease(now, session.last_observed_at, session.offline_until);
        let response = self
            .client
            .post(format!(
                "{}{}/current/heartbeat",
                self.api_base, PRODUCT_PATH
            ))
            .bearer_auth(&session.token)
            .send()
            .await;
        match response {
            Ok(response) => match decode_response::<HeartbeatResponse>(response).await {
                Ok(heartbeat) => {
                    let refreshed = StoredSession {
                        token: session.token,
                        subject: heartbeat.subject,
                        policy: heartbeat.policy,
                        offline_until: heartbeat.offline_until.timestamp(),
                        last_observed_at: now,
                    };
                    self.persistence.write_session(Some(&refreshed))?;
                    *self.session.lock().map_err(poisoned)? = Some(refreshed.clone());
                    Ok(status_from_session(&refreshed, false))
                }
                Err(RequestFailure::Rejected(reason)) => {
                    self.persistence.write_session(None)?;
                    *self.session.lock().map_err(poisoned)? = None;
                    Ok(AuthStatus::signed_out(Some(reason)))
                }
                Err(RequestFailure::Unavailable(reason)) => {
                    offline_status(&self.persistence, session, lease, reason)
                }
                Err(RequestFailure::Network(reason)) => {
                    offline_status(&self.persistence, session, lease, reason)
                }
            },
            Err(error) => {
                offline_status(&self.persistence, session, lease, network_message(&error))
            }
        }
    }
}

fn offline_status(
    persistence: &AuthPersistence,
    mut session: StoredSession,
    lease: LeaseState,
    network_reason: String,
) -> Result<AuthStatus> {
    match lease {
        LeaseState::Valid => {
            session.last_observed_at = session.last_observed_at.max(unix_time());
            persistence.write_session(Some(&session))?;
            Ok(status_from_session(&session, true))
        }
        LeaseState::ClockRollback => Ok(AuthStatus::signed_out(Some(
            "系统时间异常，必须联网校验登录状态".into(),
        ))),
        LeaseState::Expired => Ok(AuthStatus::signed_out(Some(format!(
            "离线使用期限已结束；{network_reason}"
        )))),
    }
}

fn status_from_session(session: &StoredSession, offline: bool) -> AuthStatus {
    AuthStatus {
        authenticated: true,
        offline,
        offline_until: Some(session.offline_until),
        subject: Some(session.subject.clone()),
        policy: Some(session.policy.clone()),
        reason: None,
    }
}

async fn decode_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> std::result::Result<T, RequestFailure> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| RequestFailure::Network(network_message(&error)))?;
    if status.is_success() {
        return serde_json::from_slice(&bytes)
            .map_err(|_| RequestFailure::Unavailable("服务器返回了无法识别的数据".into()));
    }
    let detail = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|payload| payload.get("detail").cloned());
    let message = localized_error(status.as_u16(), detail);
    if matches!(status.as_u16(), 401 | 403 | 404) {
        Err(RequestFailure::Rejected(message))
    } else {
        Err(RequestFailure::Unavailable(message))
    }
}

fn localized_error(status: u16, detail: Option<serde_json::Value>) -> String {
    let raw = detail
        .as_ref()
        .and_then(|value| match value {
            serde_json::Value::String(value) => Some(value.as_str()),
            serde_json::Value::Object(value) => value.get("message").and_then(|v| v.as_str()),
            _ => None,
        })
        .unwrap_or_default();
    match (status, raw) {
        (401, "Tenant is unavailable") => "租户不可用或服务期限已结束".into(),
        (401, "Invalid username or password") => "用户名或密码错误".into(),
        (401, _) => "登录状态已失效，请重新登录".into(),
        (403, _) => "当前租户尚未开通私匣（本机数据脱敏）".into(),
        (429, _) => "登录尝试过于频繁，请稍后再试".into(),
        (_, "") => format!("认证服务请求失败（HTTP {status}）"),
        (_, value) => value.to_owned(),
    }
}

fn request_error(error: RequestFailure) -> Error {
    match error {
        RequestFailure::Rejected(message) => Error::State(message),
        RequestFailure::Unavailable(message) | RequestFailure::Network(message) => {
            Error::Io(message)
        }
    }
}

fn network_error(error: reqwest::Error) -> Error {
    Error::Io(network_message(&error))
}

fn network_message(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "连接认证服务超时，请检查网络后重试".into()
    } else {
        "无法连接认证服务，请检查网络后重试".into()
    }
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "本机设备".into())
}

fn unix_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> Error {
    Error::State("登录状态异常，请重启应用".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeaseState {
    Valid,
    Expired,
    ClockRollback,
}

fn offline_lease(now: i64, last_observed_at: i64, offline_until: i64) -> LeaseState {
    if now + CLOCK_ROLLBACK_TOLERANCE_SECONDS < last_observed_at {
        LeaseState::ClockRollback
    } else if now <= offline_until {
        LeaseState::Valid
    } else {
        LeaseState::Expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_session() -> StoredSession {
        StoredSession {
            token: "secret-session-token".into(),
            subject: AuthSubject {
                id: "user-1".into(),
                username: "admin".into(),
                display_name: Some("管理员".into()),
                tenant_id: "tenant-1".into(),
                tenant_code: "demo".into(),
                tenant_name: "演示租户".into(),
            },
            policy: ProductPolicy {
                product_code: "data-desensitization".into(),
                module_enabled: true,
                concurrent_device_limit: 3,
                active_session_count: 1,
                heartbeat_interval_seconds: 600,
                offline_grace_seconds: 86_400,
            },
            offline_until: 2_000,
            last_observed_at: 1_000,
        }
    }

    #[test]
    fn offline_lease_rejects_expiry_and_clock_rollback() {
        assert_eq!(offline_lease(1_000, 900, 2_000), LeaseState::Valid);
        assert_eq!(offline_lease(2_001, 900, 2_000), LeaseState::Expired);
        assert_eq!(offline_lease(300, 1_000, 2_000), LeaseState::ClockRollback);
    }

    #[test]
    fn server_auth_errors_are_localized() {
        assert_eq!(
            localized_error(401, Some(serde_json::json!("Invalid username or password"))),
            "用户名或密码错误"
        );
        assert_eq!(
            localized_error(403, None),
            "当前租户尚未开通私匣（本机数据脱敏）"
        );
    }

    #[test]
    fn auth_state_is_encrypted_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let key = Zeroizing::new([41; 32]);
        let (persistence, session) =
            AuthPersistence::open(root.path(), key.clone(), false).unwrap();
        assert!(session.is_none());
        let device_id = persistence.device_id.clone();
        let session = stored_session();
        persistence.write_session(Some(&session)).unwrap();

        let bytes = std::fs::read(root.path().join("db/auth-state.enc")).unwrap();
        assert!(
            !bytes
                .windows(session.token.len())
                .any(|window| window == session.token.as_bytes())
        );

        let (reopened, loaded) = AuthPersistence::open(root.path(), key, false).unwrap();
        let loaded = loaded.unwrap();
        assert_eq!(reopened.device_id, device_id);
        assert_eq!(loaded.token, session.token);
        assert_eq!(loaded.subject, session.subject);
    }

    #[test]
    fn auth_state_rejects_the_wrong_key() {
        let root = tempfile::tempdir().unwrap();
        AuthPersistence::open(root.path(), Zeroizing::new([7; 32]), false).unwrap();
        assert!(AuthPersistence::open(root.path(), Zeroizing::new([8; 32]), false).is_err());
    }
}
