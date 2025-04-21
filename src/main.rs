#[macro_use]
extern crate rocket;

mod models;
use models::*;

use futures::stream::StreamExt;
use md5::{Digest, Md5};
use rocket::fs::{relative, FileServer};
use rocket::serde::{Deserialize, Serialize};
use rocket::{
    tokio::select,
    tokio::sync::broadcast::{channel, Sender},
    State,
};
use rocket_ws::{Message, WebSocket};
use sqlx::sqlite::SqlitePool;
use std::sync::atomic::{AtomicUsize, Ordering};

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

#[get("/ws")]
fn ws_handler(ws: WebSocket, state: &State<ChatState>) -> rocket_ws::Stream!['_] {
    let tx = state.tx.clone();
    let mut rx = tx.subscribe();
    state.user_count.fetch_add(1, Ordering::Relaxed);

    rocket_ws::Stream! { ws =>
        let mut ws = ws;
        // 发送初始用户数量
        let count = state.user_count.load(Ordering::Relaxed);
        let count_msg = serde_json::json!({
            "type": "user_count",
            "count": count
        });
        if let Ok(response) = serde_json::to_string(&count_msg) {
            yield Message::Text(response);
        }

        loop {
            select! {
                message = ws.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            match serde_json::from_str::<SendMessage>(&text) {
                                Ok(send_msg) => {
                                    let chat_msg = ChatMessage {
                                        username: send_msg.username,
                                        content: send_msg.content,
                                        timestamp: chrono::Utc::now().timestamp(),
                                    };

                                    if let Err(e) = tx.send(chat_msg.clone()) {
                                        eprintln!("Failed to broadcast message: {}", e);
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Failed to parse message: {}", e);
                                    let error_msg = ChatMessage {
                                        username: "Server".to_string(),
                                        content: "Invalid message format".to_string(),
                                        timestamp: chrono::Utc::now().timestamp(),
                                    };
                                    if let Ok(response) = serde_json::to_string(&error_msg) {
                                        yield Message::Text(response);
                                    }
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) => {
                            state.user_count.fetch_sub(1, Ordering::Relaxed);
                            break;
                        }
                        Some(Err(e)) => {
                            eprintln!("WebSocket error: {}", e);
                            break;
                        }
                        None => break,
                        _ => {}
                    }
                }
                msg = rx.recv() => {
                    match msg {
                        Ok(chat_msg) => {
                            if let Ok(response) = serde_json::to_string(&chat_msg) {
                                yield Message::Text(response);
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to receive broadcast message: {}", e);
                        }
                    }
                }
            }
        }
    }
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
    println!("   - WebSocket: WS  http://localhost:8000/ws");
    println!("📱 Web Interface: http://localhost:8000");

    let _ = rocket::build()
        .manage(state)
        .mount("/", FileServer::from(relative!("static")))
        .mount("/", routes![ws_handler, login, register])
        .launch()
        .await;
}

#[post("/login", data = "<request>")]
async fn login(
    request: rocket::serde::json::Json<LoginRequest>,
    state: &State<ChatState>,
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
async fn register(
    request: rocket::serde::json::Json<RegisterRequest>,
    state: &State<ChatState>,
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
