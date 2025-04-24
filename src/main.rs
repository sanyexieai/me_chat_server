mod models;
use models::*;

use futures::stream::StreamExt;
use include_dir::{include_dir, Dir};
use md5::{Digest, Md5};
use rocket::serde::{Deserialize, Serialize};
use rocket::{
    tokio::select,
    tokio::sync::broadcast::{channel, Sender},
    State, Request, routes,
};
use rocket_ws::{Message as WsMessage, WebSocket};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use rocket::http::Status;
use rocket::request::{FromRequest, Outcome};
use rocket::response::content;

// 包含静态文件目录
static STATIC_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/static");

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
        let cookies = request.cookies();
        if let Some(cookie) = cookies.get_private("username") {
            let username = cookie.value().to_string();
            let db = request.rocket().state::<SqlitePool>().unwrap();
            if let Ok(user) = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
                .bind(&username)
                .fetch_one(db)
                .await {
                Outcome::Success(AuthenticatedUser {
                    username,
                    id: user.id,
                })
            } else {
                Outcome::Forward(Status::Unauthorized)
            }
        } else {
            Outcome::Forward(Status::Unauthorized)
        }
    }
}

async fn init_db() -> SqlitePool {
    // 获取当前工作目录
    let current_dir = std::env::current_dir().unwrap();
    let db_path = current_dir.join("chat.db");

    // 创建数据库连接选项
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_secs(5));  // 忙时等待超时

    // 使用 SqlitePoolOptions 创建连接池
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(10)
        .idle_timeout(std::time::Duration::from_secs(30))
        .connect_with(options)
        .await
        .expect("Failed to connect to database");

    // 运行迁移
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    pool
}

#[rocket::get("/ws")]
fn ws_handler(ws: WebSocket, state: &State<ChatState>, user: AuthenticatedUser) -> rocket_ws::Stream!['_] {
    let tx = state.tx.clone();
    let mut rx = tx.subscribe();
    let current_user_id = user.id;
    let current_username = user.username.clone();
    let db = state.db.clone();

    println!("WebSocket connection established for user: {} (ID: {})", current_username, current_user_id);

    rocket_ws::Stream! { ws =>
        let mut ws = ws;
        loop {
            select! {
                message = ws.next() => {
                    match message {
                        Some(Ok(WsMessage::Text(text))) => {
                            println!("Received message from {}: {}", current_username, text);
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
                                        };
                                        if let Ok(response) = serde_json::to_string(&error_msg) {
                                            yield WsMessage::Text(response);
                                        }
                                        continue;
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
                            println!("WebSocket connection closed for user: {}", current_username);
                            break;
                        }
                        Some(Err(e)) => {
                            println!("WebSocket error for user {}: {}", current_username, e);
                            break;
                        }
                        None => {
                            println!("WebSocket connection ended for user: {}", current_username);
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

#[rocket::get("/")]
fn index() -> content::RawHtml<&'static str> {
    content::RawHtml(include_str!("../static/index.html"))
}

#[rocket::get("/login")]
fn login_page() -> content::RawHtml<&'static str> {
    content::RawHtml(include_str!("../static/login.html"))
}

#[rocket::get("/register")]
fn register_page() -> content::RawHtml<&'static str> {
    content::RawHtml(include_str!("../static/register.html"))
}

#[rocket::get("/static/<file..>")]
fn static_files(file: std::path::PathBuf) -> Option<content::RawHtml<&'static str>> {
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
    cookies: &rocket::http::CookieJar<'_>,
) -> rocket::serde::json::Json<AuthResponse> {
    let mut hasher = Md5::new();
    hasher.update(request.password.as_bytes());
    let password_hash = hex::encode(hasher.finalize());

    match sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ? AND password = ?")
        .bind(&request.username)
        .bind(&password_hash)
        .fetch_optional(&state.db)
        .await
        .unwrap()
    {
        Some(_) => {
            cookies.add_private(rocket::http::Cookie::new("username", request.username.clone()));
            rocket::serde::json::Json(AuthResponse {
                success: true,
                message: "Login successful".to_string(),
                token: Some(request.username.clone()),
            })
        }
        None => rocket::serde::json::Json(AuthResponse {
            success: false,
            message: "Invalid username or password".to_string(),
            token: None,
        }),
    }
}

#[rocket::post("/register", data = "<request>")]
async fn register(
    request: rocket::serde::json::Json<RegisterRequest>,
    state: &State<ChatState>,
    cookies: &rocket::http::CookieJar<'_>,
) -> rocket::serde::json::Json<AuthResponse> {
    let mut hasher = Md5::new();
    hasher.update(request.password.as_bytes());
    let password_hash = hex::encode(hasher.finalize());

    match sqlx::query("INSERT INTO users (username, password) VALUES (?, ?)")
        .bind(&request.username)
        .bind(&password_hash)
        .execute(&state.db)
        .await
    {
        Ok(_) => {
            cookies.add_private(rocket::http::Cookie::new("username", request.username.clone()));
            rocket::serde::json::Json(AuthResponse {
                success: true,
                message: "Registration successful".to_string(),
                token: Some(request.username.clone()),
            })
        }
        Err(_) => rocket::serde::json::Json(AuthResponse {
            success: false,
            message: "Username already exists".to_string(),
            token: None,
        }),
    }
}

#[rocket::post("/add_friend", data = "<request>")]
async fn add_friend(
    request: rocket::serde::json::Json<AddFriendRequest>,
    state: &State<ChatState>,
    user: AuthenticatedUser,
) -> rocket::serde::json::Json<AuthResponse> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();

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
                    message: "Already friends".to_string(),
                    token: None,
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
                message: "Friend added successfully".to_string(),
                token: None,
            })
        }
        None => rocket::serde::json::Json(AuthResponse {
            success: false,
            message: "User not found".to_string(),
            token: None,
        }),
    }
}

#[rocket::post("/create_group", data = "<request>")]
async fn create_group(
    request: rocket::serde::json::Json<CreateGroupRequest>,
    state: &State<ChatState>,
    user: AuthenticatedUser,
) -> rocket::serde::json::Json<AuthResponse> {
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
        message: format!("Group created successfully with ID: {}", group_id),
        token: None,
    })
}

#[rocket::get("/friends")]
async fn get_friends(
    state: &State<ChatState>,
    user: AuthenticatedUser,
) -> rocket::serde::json::Json<Vec<User>> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();

    let friends = sqlx::query_as::<_, User>(
        "SELECT u.* FROM users u 
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
struct DbMessage {
    id: i64,
    sender_id: i64,
    receiver_id: Option<i64>,
    group_id: Option<i64>,
    content: String,
    created_at: chrono::DateTime<chrono::Utc>,
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
        "SELECT m.id, m.sender_id, m.receiver_id, m.group_id, m.content, m.created_at, 
        'history' as direction,
        u.username 
        FROM messages m
        JOIN users u ON m.sender_id = u.id
        WHERE (m.sender_id = ? AND m.receiver_id = ?) 
        OR (m.sender_id = ? AND m.receiver_id = ?)
        ORDER BY m.created_at ASC"
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
        timestamp: row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").timestamp(),
        direction: row.get("direction"),
        username: row.get("username"),
    })
    .collect();

    rocket::serde::json::Json(messages)
}

#[rocket::get("/group_messages/<group_id>")]
async fn get_group_messages(
    state: &State<ChatState>,
    user: AuthenticatedUser,
    group_id: i64,
) -> rocket::serde::json::Json<Vec<DbMessage>> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();

    // 验证用户是否在群组中
    if !is_group_member(group_id, user.id, &state.db).await {
        return rocket::serde::json::Json(Vec::new());
    }

    let messages = sqlx::query_as::<_, DbMessage>(
        "SELECT * FROM messages 
        WHERE group_id = ?
        ORDER BY created_at ASC"
    )
    .bind(group_id)
    .fetch_all(&state.db)
    .await
    .unwrap();

    rocket::serde::json::Json(messages)
}

#[rocket::get("/current_user")]
async fn get_current_user(
    state: &State<ChatState>,
    user: AuthenticatedUser,
) -> rocket::serde::json::Json<User> {
    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(&user.username)
        .fetch_one(&state.db)
        .await
        .unwrap();
    rocket::serde::json::Json(user)
}

#[rocket::main]
async fn main() {
    // 初始化日志
    env_logger::init();

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
        db: db.clone(),  // 克隆数据库连接池
    };

    println!("🚀 Chat Server is starting...");
    println!("🌐 Server running at: http://localhost:{}", port);
    println!("📝 API Endpoints:");
    println!("   - Login:    POST http://localhost:{}/api/login", port);
    println!("   - Register: POST http://localhost:{}/api/register", port);
    println!("📱 Web Interface: http://localhost:{}", port);

    let config = rocket::Config::figment()
        .merge(("port", port))
        .merge(("address", "0.0.0.0"))
        .merge(("secret_key", "8Xui8SN4mI+7egV/9dlfYYLGQJeEx4+DwmSQLwDVXJg="));

    let _ = rocket::build()
        .manage(state)
        .manage(db)  // 添加数据库连接池作为独立状态
        .mount("/", routes![index, login_page, register_page, static_files, ws_handler])
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
            ],
        )
        .configure(config)
        .launch()
        .await;
}
