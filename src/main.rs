#[macro_use]
extern crate rocket;

mod models;
use models::*;

use rocket::{
    tokio::sync::broadcast::{channel, Sender},
    State,
};
use rocket::futures::{SinkExt, StreamExt};
use rocket::response::stream::{Event, EventStream};
use rocket::serde::{Deserialize, Serialize};
use rocket::tokio::select;
use rocket::fs::{FileServer, relative};
use std::sync::atomic::{AtomicUsize, Ordering};
use sqlx::sqlite::SqlitePool;
use md5::{Md5, Digest};
use hex;
use serde_json;

struct ChatState {
    tx: Sender<ChatMessage>,
    user_count: AtomicUsize,
    db: SqlitePool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    username: String,
    content: String,
    timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SendMessage {
    username: String,
    content: String,
}

async fn init_db() -> SqlitePool {
    // 获取当前工作目录
    let current_dir = std::env::current_dir().unwrap();
    let db_path = current_dir.join("chat.db");
    
    // 创建数据库连接选项
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true);
    
    // 创建数据库连接
    let pool = SqlitePool::connect_with(options)
        .await
        .expect("Failed to connect to database");
    
    // 运行迁移
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    pool
}

#[rocket::main]
async fn main() {
    // 初始化日志
    env_logger::init();

    // 创建广播通道
    let (tx, _) = channel::<ChatMessage>(1024);
    let db = init_db().await;
    let state = ChatState {
        tx,
        user_count: AtomicUsize::new(0),
        db,
    };

    println!("🚀 Chat Server is starting...");
    println!("🌐 Server running at: http://localhost:8000");
    println!("📝 API Endpoints:");
    println!("   - Login:    POST http://localhost:8000/login");
    println!("   - Register: POST http://localhost:8000/register");
    println!("   - Chat:     GET  http://localhost:8000/events");
    println!("   - Send:     POST http://localhost:8000/send");
    println!("📱 Web Interface: http://localhost:8000");

    let _ = rocket::build()
        .manage(state)
        .mount("/", FileServer::from(relative!("static")))
        .mount("/", routes![events, send, login, register])
        .launch()
        .await;
}

#[post("/login", data = "<request>")]
async fn login(request: rocket::serde::json::Json<LoginRequest>, state: &State<ChatState>) -> rocket::serde::json::Json<AuthResponse> {
    let mut hasher = Md5::new();
    hasher.update(request.password.as_bytes());
    let password_hash = hex::encode(hasher.finalize());

    match sqlx::query_as::<_, User>(
        "SELECT * FROM users WHERE username = ? AND password = ?"
    )
    .bind(&request.username)
    .bind(&password_hash)
    .fetch_optional(&state.db)
    .await
    .unwrap()
    {
        Some(_) => rocket::serde::json::Json(AuthResponse {
            success: true,
            message: "Login successful".to_string(),
            token: Some(request.username.clone()),
        }),
        None => rocket::serde::json::Json(AuthResponse {
            success: false,
            message: "Invalid username or password".to_string(),
            token: None,
        }),
    }
}

#[post("/register", data = "<request>")]
async fn register(request: rocket::serde::json::Json<RegisterRequest>, state: &State<ChatState>) -> rocket::serde::json::Json<AuthResponse> {
    let mut hasher = Md5::new();
    hasher.update(request.password.as_bytes());
    let password_hash = hex::encode(hasher.finalize());

    match sqlx::query(
        "INSERT INTO users (username, password) VALUES (?, ?)"
    )
    .bind(&request.username)
    .bind(&password_hash)
    .execute(&state.db)
    .await
    {
        Ok(_) => rocket::serde::json::Json(AuthResponse {
            success: true,
            message: "Registration successful".to_string(),
            token: Some(request.username.clone()),
        }),
        Err(_) => rocket::serde::json::Json(AuthResponse {
            success: false,
            message: "Username already exists".to_string(),
            token: None,
        }),
    }
}

#[get("/events")]
async fn events(state: &State<ChatState>) -> EventStream![] {
    let mut rx = state.tx.subscribe();

    EventStream! {
        let mut interval = rocket::tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            select! {
                msg = rx.recv() => {
                    if let Ok(msg) = msg {
                        yield Event::data(serde_json::to_string(&msg).unwrap());
                    }
                }
                _ = interval.tick() => {
                    // 保持连接活跃
                }
            }
        }
    }
}

#[post("/send", data = "<message>")]
async fn send(message: rocket::serde::json::Json<SendMessage>, state: &State<ChatState>) {
    let msg = ChatMessage {
        username: message.username.clone(),
        content: message.content.clone(),
        timestamp: chrono::Utc::now().timestamp(),
    };
    let _ = state.tx.send(msg);
}
