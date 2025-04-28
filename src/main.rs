mod models;
use models::*;

use futures::stream::StreamExt;
use include_dir::{include_dir, Dir};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use md5::{Digest, Md5};
use rocket::form::{Form, FromForm};
use rocket::fs::{FileServer, TempFile};
use rocket::http::Status;
use rocket::request::{FromRequest, Outcome};
use rocket::response::content;
use rocket::serde::{json::Json, Deserialize, Serialize};
use rocket::{
    post, routes,
    tokio::select,
    tokio::sync::broadcast::{channel, Sender},
    Request, State,
};
use rocket_ws::{Message as WsMessage, WebSocket};
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use tokio::sync::Mutex as TokioMutex;

// JWT 密钥
const JWT_SECRET: &[u8] = b"your-secret-key";
const JWT_EXPIRATION: i64 = 24 * 60 * 60; // 24 hours

// JWT Claims 结构体
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    username: String,
    exp: i64,
}

// 文件上传状态结构体
struct FileUploadState {
    uploads: TokioMutex<HashMap<String, Vec<Vec<u8>>>>,
    db: SqlitePool,
}

// 分片上传请求结构体
#[derive(FromForm)]
struct ChunkUpload<'r> {
    file: TempFile<'r>,
    file_name: String,
    md5: String,
    chunk_index: usize,
    total_chunks: usize,
}

// 包含静态文件目录
static STATIC_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/static");

// 聊天状态结构体
struct ChatState {
    tx: Sender<ChatMessage>,
    db: SqlitePool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    username: String,
    content: String,
    timestamp: i64,
    message_type: String,
    target_type: String,
    direction: String,
    sender_id: i64,
    receiver_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_size: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SendMessage {
    username: String,
    content: String,
    #[serde(default = "default_message_type")]
    message_type: String,
    target_type: String,
    direction: String,
    sender_id: i64,
    receiver_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_size: Option<i64>,
}

fn default_message_type() -> String {
    "text".to_string()
}

// 用户认证结构体
struct AuthenticatedUser {
    username: String,
    id: i64,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for AuthenticatedUser {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        // 从请求头获取 token
        let token = request
            .headers()
            .get_one("Authorization")
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .or_else(|| request.query_value::<&str>("token")?.ok());

        if let Some(token) = token {
            // 验证 JWT token
            match decode::<Claims>(
                token,
                &DecodingKey::from_secret(JWT_SECRET),
                &Validation::default(),
            ) {
                Ok(token_data) => {
                    let username = token_data.claims.username;
                    let db = request.rocket().state::<SqlitePool>().unwrap();
                    if let Ok(user) =
                        sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
                            .bind(&username)
                            .fetch_one(db)
                            .await
                    {
                        Outcome::Success(AuthenticatedUser {
                            username,
                            id: user.id,
                        })
                    } else {
                        Outcome::Forward(Status::Unauthorized)
                    }
                }
                Err(_) => Outcome::Forward(Status::Unauthorized),
            }
        } else {
            Outcome::Forward(Status::Unauthorized)
        }
    }
}

// 检查用户是否是群组成员
async fn is_group_member(group_id: i64, user_id: i64, db: &SqlitePool) -> bool {
    sqlx::query("SELECT 1 FROM group_members WHERE group_id = ? AND user_id = ?")
        .bind(group_id)
        .bind(user_id)
        .fetch_optional(db)
        .await
        .unwrap()
        .is_some()
}

// 初始化数据库
async fn init_db() -> SqlitePool {
    // 获取当前工作目录
    let current_dir = std::env::current_dir().unwrap();
    let db_path = current_dir.join("chat.db");

    // 创建数据库连接选项
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_secs(5)); // 忙时等待超时

    // 使用 SqlitePoolOptions 创建连接池
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(10)
        .idle_timeout(std::time::Duration::from_secs(30))
        .connect_with(options)
        .await
        .expect("数据库连接失败");

    // 运行迁移
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("数据库迁移失败");

    pool
}

#[rocket::get("/ws")]
fn ws_handler(
    ws: WebSocket,
    state: &State<ChatState>,
    user: AuthenticatedUser,
) -> rocket_ws::Stream!['_] {
    let tx = state.tx.clone();
    let mut rx = tx.subscribe();
    let current_user_id = user.id;
    let current_username = user.username.clone();
    let db = state.db.clone();

    println!(
        "WebSocket 连接已建立，用户: {} (ID: {})",
        current_username, current_user_id
    );

    rocket_ws::Stream! { ws =>
        let mut ws = ws;
        loop {
            select! {
                message = ws.next() => {
                    match message {
                        Some(Ok(WsMessage::Text(text))) => {
                            println!("收到来自 {} 的消息: {}", current_username, text);
                            match serde_json::from_str::<SendMessage>(&text) {
                                Ok(send_msg) => {
                                    // 验证目标用户或群组是否存在
                                    let target_exists = if send_msg.target_type == "person" {
                                        sqlx::query("SELECT 1 FROM users WHERE id = ?")
                                            .bind(send_msg.receiver_id)
                                            .fetch_optional(&db)
                                            .await
                                            .unwrap()
                                            .is_some()
                                    } else {
                                        sqlx::query("SELECT 1 FROM groups WHERE id = ?")
                                            .bind(send_msg.receiver_id)
                                            .fetch_optional(&db)
                                            .await
                                            .unwrap()
                                            .is_some()
                                    };

                                    if !target_exists {
                                        let error_msg = ChatMessage {
                                            username: "Server".to_string(),
                                            content: format!("目标 {} 不存在", send_msg.receiver_id),
                                            timestamp: chrono::Utc::now().timestamp(),
                                            message_type: "error".to_string(),
                                            target_type: "person".to_string(),
                                            direction: "receive".to_string(),
                                            sender_id: current_user_id,
                                            receiver_id: current_user_id,
                                            file_path: None,
                                            file_name: None,
                                            file_size: None,
                                        };
                                        if let Ok(response) = serde_json::to_string(&error_msg) {
                                            yield WsMessage::Text(response);
                                        }
                                        continue;
                                    }

                                    // 如果是文件消息，获取文件信息
                                    let mut file_path = None;
                                    let mut file_name = None;
                                    let mut file_size = None;

                                    if send_msg.message_type == "file" {
                                        if let Ok(row) = sqlx::query(
                                            "SELECT file_path, file_name, file_size FROM files WHERE id = ?"
                                        )
                                        .bind(&send_msg.content)
                                        .fetch_one(&db)
                                        .await {
                                            file_path = row.get("file_path");
                                            file_name = row.get("file_name");
                                            file_size = row.get("file_size");
                                        }
                                    }

                                    let chat_msg = ChatMessage {
                                        username: current_username.clone(),
                                        content: send_msg.content,
                                        timestamp: chrono::Utc::now().timestamp(),
                                        message_type: send_msg.message_type,
                                        target_type: send_msg.target_type,
                                        direction: "send".to_string(),
                                        sender_id: current_user_id,
                                        receiver_id: send_msg.receiver_id,
                                        file_path,
                                        file_name,
                                        file_size,
                                    };

                                    // 保存消息到数据库
                                    let db = state.db.clone();
                                    let msg = chat_msg.clone();
                                    tokio::spawn(async move {
                                        match sqlx::query(
                                            "INSERT INTO messages (sender_id, receiver_id, group_id, content, message_type, created_at)
                                            VALUES (?, ?, ?, ?, ?, ?)"
                                        )
                                        .bind(current_user_id)
                                        .bind(if msg.target_type == "person" { Some(msg.receiver_id) } else { None })
                                        .bind(if msg.target_type == "group" { Some(msg.receiver_id) } else { None })
                                        .bind(&msg.content)
                                        .bind(&msg.message_type)
                                        .bind(chrono::Utc::now())
                                        .execute(&db)
                                        .await {
                                            Ok(_) => println!("消息已保存到数据库"),
                                            Err(e) => println!("保存消息失败: {}", e),
                                        }
                                    });

                                    // 广播消息
                                    if let Err(e) = tx.send(chat_msg) {
                                        println!("广播消息失败: {}", e);
                                    }
                                }
                                Err(e) => {
                                    println!("解析消息失败: {}", e);
                                    let error_msg = ChatMessage {
                                        username: "Server".to_string(),
                                        content: "消息格式错误".to_string(),
                                        timestamp: chrono::Utc::now().timestamp(),
                                        message_type: "error".to_string(),
                                        target_type: "person".to_string(),
                                        direction: "receive".to_string(),
                                        sender_id: current_user_id,
                                        receiver_id: current_user_id,
                                        file_path: None,
                                        file_name: None,
                                        file_size: None,
                                    };
                                    if let Ok(response) = serde_json::to_string(&error_msg) {
                                        yield WsMessage::Text(response);
                                    }
                                }
                            }
                        }
                        Some(Ok(WsMessage::Binary(_))) => {
                            // 忽略二进制消息
                            continue;
                        }
                        Some(Ok(WsMessage::Ping(_))) => {
                            // 忽略 ping 消息
                            continue;
                        }
                        Some(Ok(WsMessage::Pong(_))) => {
                            // 忽略 pong 消息
                            continue;
                        }
                        Some(Ok(WsMessage::Frame(_))) => {
                            // 忽略 frame 消息
                            continue;
                        }
                        Some(Ok(WsMessage::Close(_))) => {
                            println!("WebSocket 连接已关闭，用户: {}", current_username);
                            break;
                        }
                        Some(Err(e)) => {
                            println!("WebSocket 错误，用户 {}: {}", current_username, e);
                            break;
                        }
                        None => {
                            println!("WebSocket 连接已结束，用户: {}", current_username);
                            break;
                        }
                    }
                }
                msg = rx.recv() => {
                    match msg {
                        Ok(chat_msg) => {
                            // 只发送给目标用户或群组成员
                            if (chat_msg.target_type == "person" && chat_msg.receiver_id == current_user_id) ||
                               (chat_msg.target_type == "group" && is_group_member(chat_msg.receiver_id, current_user_id, &db).await) {
                                // 设置接收方向
                                let mut received_msg = chat_msg.clone();
                                received_msg.direction = "receive".to_string();
                                if let Ok(response) = serde_json::to_string(&received_msg) {
                                    yield WsMessage::Text(response);
                                }
                            }
                        }
                        Err(e) => {
                            println!("接收广播消息失败: {}", e);
                        }
                    }
                }
            }
        }
    }
}

#[rocket::get("/")]
fn index() -> content::RawHtml<&'static str> {
    // 返回首页 HTML
    content::RawHtml(include_str!("../static/index.html"))
}

#[rocket::get("/login")]
fn login_page() -> content::RawHtml<&'static str> {
    // 返回登录页面 HTML
    content::RawHtml(include_str!("../static/login.html"))
}

#[rocket::get("/register")]
fn register_page() -> content::RawHtml<&'static str> {
    // 返回注册页面 HTML
    content::RawHtml(include_str!("../static/register.html"))
}

#[rocket::get("/static/<file..>")]
fn static_files(file: std::path::PathBuf) -> Option<content::RawHtml<&'static str>> {
    // 返回静态文件内容
    if let Some(file) = STATIC_DIR.get_file(file) {
        if let Some(content) = file.contents_utf8() {
            return Some(content::RawHtml(content));
        }
    }
    None
}

#[rocket::post("/login", data = "<request>")]
async fn login(
    request: rocket::serde::json::Json<LoginRequest>,
    state: &State<ChatState>,
) -> rocket::serde::json::Json<AuthResponse> {
    // 计算密码哈希
    let mut hasher = Md5::new();
    hasher.update(request.password.as_bytes());
    let password_hash = hex::encode(hasher.finalize());

    // 验证用户登录
    match sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ? AND password = ?")
        .bind(&request.username)
        .bind(&password_hash)
        .fetch_optional(&state.db)
        .await
        .unwrap()
    {
        Some(user   ) => {
            // 生成 JWT token
            let expiration = chrono::Utc::now()
                .checked_add_signed(chrono::Duration::seconds(JWT_EXPIRATION))
                .expect("valid timestamp")
                .timestamp();

            let claims = Claims {
                username: request.username.clone(),
                exp: expiration,
            };

            let token = encode(
                &Header::default(),
                &claims,
                &EncodingKey::from_secret(JWT_SECRET),
            )
            .unwrap();

            rocket::serde::json::Json(AuthResponse {
                success: true,
                message: "登录成功".to_string(),
                token: Some(token),
                user_id: Some(user.id),
            })
        }
        None => rocket::serde::json::Json(AuthResponse {
            success: false,
            message: "用户名或密码错误".to_string(),
            token: None,
            user_id: None,
        }),
    }
}

#[rocket::post("/register", data = "<request>")]
async fn register(
    request: rocket::serde::json::Json<RegisterRequest>,
    state: &State<ChatState>,
    cookies: &rocket::http::CookieJar<'_>,
) -> rocket::serde::json::Json<AuthResponse> {
    // 计算密码哈希
    let mut hasher = Md5::new();
    hasher.update(request.password.as_bytes());
    let password_hash = hex::encode(hasher.finalize());

    // 注册新用户
    match sqlx::query("INSERT INTO users (username, password) VALUES (?, ?)")
        .bind(&request.username)
        .bind(&password_hash)
        .execute(&state.db)
        .await
    {
        Ok(_) => {
            // 注册成功，设置 cookie
            cookies.add_private(rocket::http::Cookie::new(
                "username",
                request.username.clone(),
            ));
            rocket::serde::json::Json(AuthResponse {
                success: true,
                message: "注册成功".to_string(),
                token: Some(request.username.clone()),
                user_id: None,
            })
        }
        Err(_) => rocket::serde::json::Json(AuthResponse {
            success: false,
            message: "用户名已存在".to_string(),
            token: None,
            user_id: None,
        }),
    }
}

#[rocket::post("/add_friend", data = "<request>")]
async fn add_friend(
    request: rocket::serde::json::Json<AddFriendRequest>,
    state: &State<ChatState>,
    user: AuthenticatedUser,
) -> rocket::serde::json::Json<AuthResponse> {
    // 获取当前用户信息
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();

    // 获取好友用户信息
    let friend = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&request.friend_username)
        .fetch_optional(&state.db)
        .await
        .unwrap();

    match friend {
        Some(friend) => {
            // 检查是否已经是好友
            let existing = sqlx::query_as::<_, Friendship>(
                "SELECT * FROM friendships WHERE (user_id = ? AND friend_id = ?) OR (user_id = ? AND friend_id = ?)",
            )
            .bind(user.id)
            .bind(friend.id)
            .bind(friend.id)
            .bind(user.id)
            .fetch_optional(&state.db)
            .await
            .unwrap();

            if existing.is_some() {
                return rocket::serde::json::Json(AuthResponse {
                    success: false,
                    message: "已经是好友".to_string(),
                    token: None,
                    user_id: Some(user.id),
                });
            }

            // 添加好友关系
            sqlx::query("INSERT INTO friendships (user_id, friend_id) VALUES (?, ?)")
                .bind(user.id)
                .bind(friend.id)
                .execute(&state.db)
                .await
                .unwrap();

            rocket::serde::json::Json(AuthResponse {
                success: true,
                message: "添加好友成功".to_string(),
                token: None,
                user_id: Some(user.id),
            })
        }
        None => rocket::serde::json::Json(AuthResponse {
            success: false,
            message: "用户不存在".to_string(),
            token: None,
            user_id: None,
        }),
    }
}

#[rocket::post("/create_group", data = "<request>")]
async fn create_group(
    request: rocket::serde::json::Json<CreateGroupRequest>,
    state: &State<ChatState>,
    user: AuthenticatedUser,
) -> rocket::serde::json::Json<AuthResponse> {
    // 获取当前用户信息
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();

    // 创建群组
    let group_id = sqlx::query("INSERT INTO groups (name, created_by) VALUES (?, ?)")
        .bind(&request.name)
        .bind(user.id)
        .execute(&state.db)
        .await
        .unwrap()
        .last_insert_rowid();

    // 添加群组成员
    for member_username in &request.members {
        if let Ok(member) = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
            .bind(member_username)
            .fetch_one(&state.db)
            .await
        {
            sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES (?, ?)")
                .bind(group_id)
                .bind(member.id)
                .execute(&state.db)
                .await
                .unwrap();
        }
    }

    // 添加创建者到群组
    sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES (?, ?)")
        .bind(group_id)
        .bind(user.id)
        .execute(&state.db)
        .await
        .unwrap();

    rocket::serde::json::Json(AuthResponse {
        success: true,
        message: format!("群组创建成功，ID: {}", group_id),
        token: None,
        user_id: Some(user.id),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
struct FriendInfo {
    id: i64,
    username: String,
    created_at: chrono::NaiveDateTime,
}

#[rocket::get("/friends")]
async fn get_friends(
    state: &State<ChatState>,
    user: AuthenticatedUser,
) -> rocket::serde::json::Json<Vec<FriendInfo>> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();

    let friends = sqlx::query_as::<_, FriendInfo>(
        "SELECT u.id, u.username, u.created_at FROM users u 
        JOIN friendships f ON (f.friend_id = u.id AND f.user_id = ?) OR (f.user_id = u.id AND f.friend_id = ?)",
    )
    .bind(user.id)
    .bind(user.id)
    .fetch_all(&state.db)
    .await
    .unwrap();

    rocket::serde::json::Json(friends)
}

#[rocket::get("/groups")]
async fn get_groups(
    state: &State<ChatState>,
    user: AuthenticatedUser,
) -> rocket::serde::json::Json<Vec<Group>> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();

    let groups = sqlx::query_as::<_, Group>(
        "SELECT g.* FROM groups g 
        JOIN group_members gm ON g.id = gm.group_id 
        WHERE gm.user_id = ?",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await
    .unwrap();

    rocket::serde::json::Json(groups)
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
struct Friendship {
    id: i64,
    user_id: i64,
    friend_id: i64,
    created_at: chrono::NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
struct Group {
    id: i64,
    name: String,
    created_by: i64,
    created_at: chrono::NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MessageResponse {
    id: i64,
    sender_id: i64,
    receiver_id: Option<i64>,
    group_id: Option<i64>,
    content: String,
    timestamp: i64,
    direction: String,
    username: String,
    file_path: Option<String>,
    file_name: Option<String>,
    file_size: Option<i64>,
    message_type: Option<String>,
}

#[rocket::get("/messages/<target_id>")]
async fn get_messages(
    state: &State<ChatState>,
    user: AuthenticatedUser,
    target_id: i64,
) -> rocket::serde::json::Json<Vec<MessageResponse>> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();

    let messages = sqlx::query(
        "SELECT m.id, m.sender_id, m.receiver_id, m.group_id, m.content, m.created_at,f.file_name, f.file_path,f.file_size,m.message_type,
        'history' as direction,u.username 
        FROM messages m
        JOIN users u ON m.sender_id = u.id
        LEFT JOIN files f ON m.content = f.id
        WHERE (m.sender_id = ? AND m.receiver_id = ?) 
        OR (m.sender_id = ? AND m.receiver_id = ?)
        ORDER BY m.created_at ASC",
    )
    .bind(user.id)
    .bind(target_id)
    .bind(target_id)
    .bind(user.id)
    .fetch_all(&state.db)
    .await
    .unwrap()
    .iter()
    .map(|row| MessageResponse {
        id: row.get("id"),
        sender_id: row.get("sender_id"),
        receiver_id: row.get("receiver_id"),
        group_id: row.get("group_id"),
        content: row.get("content"),
        timestamp: row
            .get::<chrono::DateTime<chrono::Utc>, _>("created_at")
            .timestamp(),
        direction: row.get("direction"),
        username: row.get("username"),
        file_name: row.get("file_name"),
        file_path: row.get("file_path"),
        file_size: row.get("file_size"),
        message_type: row.get("message_type"),
    })
    .collect();

    rocket::serde::json::Json(messages)
}

#[rocket::get("/group_messages/<group_id>")]
async fn get_group_messages(
    state: &State<ChatState>,
    user: AuthenticatedUser,
    group_id: i64,
) -> rocket::serde::json::Json<Vec<MessageResponse>> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();

    // 验证用户是否在群组中
    if !is_group_member(group_id, user.id, &state.db).await {
        return rocket::serde::json::Json(Vec::new());
    }

    let messages = sqlx::query(
        "SELECT m.id, m.sender_id, m.receiver_id, m.group_id, m.content, m.created_at,f.file_name, f.file_path,f.file_size,m.message_type,
        'history' as direction, u.username 
        FROM messages m
        JOIN users u ON m.sender_id = u.id
        LEFT JOIN files f ON m.content = f.id
        WHERE m.group_id = ?
        ORDER BY m.created_at ASC",
    )
    .bind(group_id)
    .fetch_all(&state.db)
    .await
    .unwrap()
    .iter()
    .map(|row| MessageResponse {
        id: row.get("id"),
        sender_id: row.get("sender_id"),
        receiver_id: row.get("receiver_id"),
        group_id: row.get("group_id"),
        content: row.get("content"),
        timestamp: row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").timestamp(),
        direction: row.get("direction"),
        username: row.get("username"),
        file_name: row.get("file_name"),
        file_path: row.get("file_path"),
        file_size: row.get("file_size"),
        message_type: row.get("message_type"),
    })
    .collect();

    rocket::serde::json::Json(messages)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserInfo {
    id: i64,
    username: String,
    created_at: chrono::NaiveDateTime,
}

#[rocket::get("/current_user")]
async fn get_current_user(
    state: &State<ChatState>,
    user: AuthenticatedUser,
) -> rocket::serde::json::Json<UserInfo> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();

    rocket::serde::json::Json(UserInfo {
        id: user.id,
        username: user.username,
        created_at: user.created_at,
    })
}

#[post("/api/upload_file_chunk", data = "<form>")]
async fn upload_file_chunk(
    mut form: Form<ChunkUpload<'_>>,
    state: &State<FileUploadState>,
    user: AuthenticatedUser,
) -> Result<Json<Value>, Status> {
    println!("收到文件分片上传请求");
    println!("文件名: {:?}", form.file_name);
    println!("文件 MD5: {:?}", form.md5);
    println!("分片索引: {}/{}", form.chunk_index, form.total_chunks);

    // 如果是第一片，只检查文件是否存在
    if form.chunk_index == 0 {
        // 创建上传目录
        let upload_dir = Path::new("uploads");
        if !upload_dir.exists() {
            if let Err(e) = fs::create_dir_all(upload_dir) {
                eprintln!("创建上传目录失败: {}", e);
                return Err(Status::InternalServerError);
            }
        }

        // 生成最终文件名
        let extension = Path::new(&form.file_name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");
        let final_filename = if !extension.is_empty() {
            format!("{}.{}", form.md5, extension)
        } else {
            form.md5.clone()
        };

        let file_path = upload_dir.join(final_filename.clone());

        // 检查本地文件是否已存在
        if file_path.exists() {
            // 文件已存在，直接保存到数据库
            let file_size = file_path
                .metadata()
                .map_err(|e| {
                    eprintln!("获取文件元数据失败: {}", e);
                    Status::InternalServerError
                })?
                .len() as i64;

            let mime_type = mime_guess::from_path(&form.file_name).first_or_octet_stream();

            let db = state.db.clone();
            let file_id = sqlx::query(
                "INSERT INTO files (md5, file_name, file_size, file_path, mime_type, created_by, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(&form.md5)
            .bind(&form.file_name)
            .bind(file_size)
            .bind(&final_filename)
            .bind(mime_type.to_string())
            .bind(user.id)
            .bind(chrono::Utc::now())
            .execute(&db)
            .await
            .map_err(|e| {
                eprintln!("保存文件信息到数据库失败: {}", e);
                Status::InternalServerError
            })?
            .last_insert_rowid();

            println!("文件已存在，已保存到数据库: {}", file_path.display());

            return Ok(Json(json!({
                "success": true,
                "file_url": final_filename,
                "file_id": file_id,
                "skip_upload": true
            })));
        }

        // 文件不存在，返回继续上传
        return Ok(Json(json!({
            "success": true,
            "skip_upload": false
        })));
    }

    // 如果不是第一片，按正常流程处理
    // 创建上传目录
    let upload_dir = Path::new("uploads");
    if !upload_dir.exists() {
        if let Err(e) = fs::create_dir_all(upload_dir) {
            eprintln!("创建上传目录失败: {}", e);
            return Err(Status::InternalServerError);
        }
    }

    // 读取分片数据
    let temp_path = upload_dir.join(format!("temp_{}", form.md5));
    if let Err(e) = form.file.copy_to(&temp_path).await {
        eprintln!("读取分片数据失败: {}", e);
        return Err(Status::InternalServerError);
    }
    let chunk_data = fs::read(&temp_path).map_err(|e| {
        eprintln!("读取临时文件失败: {}", e);
        Status::InternalServerError
    })?;
    fs::remove_file(&temp_path).ok(); // 清理临时文件

    // 将分片数据存储到状态中
    let mut uploads = state.uploads.lock().await;
    let chunks = uploads.entry(form.md5.clone()).or_insert_with(Vec::new);

    // 确保分片索引正确（从1开始）
    while chunks.len() < form.chunk_index - 1 {
        chunks.push(Vec::new());
    }
    chunks.push(chunk_data);

    // 生成最终文件名
    let extension = Path::new(&form.file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");
    let final_filename = if !extension.is_empty() {
        format!("{}.{}", form.md5, extension)
    } else {
        form.md5.clone()
    };

    // 检查是否所有分片都已上传（不包括第一片）
    if chunks.len() == form.total_chunks - 1 {
        let file_path = upload_dir.join(final_filename.clone());

        // 创建临时文件
        let mut file = File::create(&file_path).map_err(|e| {
            eprintln!("创建文件失败: {}", e);
            Status::InternalServerError
        })?;

        // 合并所有分片
        let mut hasher = Md5::new();
        for chunk in chunks.iter() {
            if let Err(e) = file.write_all(chunk) {
                eprintln!("写入分片失败: {}", e);
                return Err(Status::InternalServerError);
            }
            hasher.update(chunk);
        }
        let file_md5 = hex::encode(hasher.finalize());

        // 验证文件 MD5
        if file_md5 != form.md5 {
            eprintln!("文件 MD5 不匹配: 期望 {}, 实际 {}", form.md5, file_md5);
            return Err(Status::BadRequest);
        }

        // 保存文件信息到数据库
        let mut file_size: i64 = 0;
        for chunk in chunks.iter() {
            file_size += chunk.len() as i64;
        }
        let mime_type = mime_guess::from_path(&form.file_name).first_or_octet_stream();

        let db = state.db.clone();
        let file_id = sqlx::query(
            "INSERT INTO files (md5, file_name, file_size, file_path, mime_type, created_by, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&form.md5)
        .bind(&form.file_name)
        .bind(file_size)
        .bind(&final_filename)
        .bind(mime_type.to_string())
        .bind(user.id)
        .bind(chrono::Utc::now())
        .execute(&db)
        .await
        .map_err(|e| {
            eprintln!("保存文件信息到数据库失败: {}", e);
            Status::InternalServerError
        })?
        .last_insert_rowid();

        // 清理上传状态
        uploads.remove(&form.md5);

        println!("文件保存成功: {}", file_path.display());

        // 返回文件URL
        Ok(Json(json!({
            "success": true,
            "file_url": final_filename,
            "file_id": file_id
        })))
    } else {
        // 返回上传进度（不包括第一片）
        Ok(Json(json!({
            "success": true,
            "progress": ((chunks.len() as f64 / (form.total_chunks - 1) as f64) * 100.0) as i32,
            "file_url": final_filename,
            "file_id": 0
        })))
    }
}

#[rocket::get("/files/<file_id>")]
async fn get_file_info(state: &State<ChatState>, file_id: i64) -> Result<Json<Value>, Status> {
    match sqlx::query("SELECT file_path, file_name, file_size FROM files WHERE id = ?")
        .bind(file_id)
        .fetch_one(&state.db)
        .await
    {
        Ok(row) => {
            let file_info = json!({
                "file_path": row.get::<String, _>("file_path"),
                "file_name": row.get::<String, _>("file_name"),
                "file_size": row.get::<i64, _>("file_size")
            });
            Ok(Json(file_info))
        }
        Err(_) => Err(Status::NotFound),
    }
}

#[rocket::main]
async fn main() {
    // 初始化日志
    env_logger::init();

    // 创建上传目录
    let upload_dir = Path::new("uploads");
    if !upload_dir.exists() {
        fs::create_dir_all(upload_dir).expect("Failed to create uploads directory");
    }

    // 从环境变量获取端口，默认为8080
    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);

    // 创建广播通道
    let (tx, _) = channel::<ChatMessage>(1024);
    let db = init_db().await;
    let state = ChatState {
        tx,
        db: db.clone(), // 克隆数据库连接池
    };

    // 创建文件上传状态
    let file_upload_state = FileUploadState {
        uploads: TokioMutex::new(HashMap::new()),
        db: db.clone(),
    };

    println!("🚀 聊天服务器正在启动...");
    println!("🌐 服务器运行在: http://localhost:{}", port);
    println!("📝 API 端点:");
    println!("   - 登录:    POST http://localhost:{}/api/login", port);
    println!("   - 注册: POST http://localhost:{}/api/register", port);
    println!("📱 网页界面: http://localhost:{}", port);

    let config = rocket::Config::figment()
        .merge(("port", port))
        .merge(("address", "0.0.0.0"))
        .merge(("secret_key", "8Xui8SN4mI+7egV/9dlfYYLGQJeEx4+DwmSQLwDVXJg="));

    let _ = rocket::build()
        .manage(state)
        .manage(db)
        .manage(file_upload_state)
        .mount(
            "/",
            routes![
                index,
                login_page,
                register_page,
                static_files,
                ws_handler,
                upload_file_chunk,
            ],
        )
        .mount(
            "/api",
            routes![
                login,
                register,
                add_friend,
                create_group,
                get_friends,
                get_groups,
                get_messages,
                get_group_messages,
                get_current_user,
                get_file_info,
            ],
        )
        .mount("/", FileServer::from("uploads"))
        .configure(config)
        .launch()
        .await;
}
