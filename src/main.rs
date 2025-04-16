#[macro_use]
extern crate rocket;

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
use serde_json;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    username: String,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SendMessage {
    content: String,
}

struct ChatState {
    tx: Sender<ChatMessage>,
    user_count: AtomicUsize,
}

#[rocket::main]
async fn main() {
    // 初始化日志
    env_logger::init();

    // 创建广播通道
    let (tx, _) = channel::<ChatMessage>(1024);
    let state = ChatState {
        tx,
        user_count: AtomicUsize::new(0),
    };

    let _ = rocket::build()
        .manage(state)
        .mount("/", FileServer::from(relative!("static")))
        .mount("/", routes![events, send])
        .launch()
        .await;
}

#[get("/events")]
async fn events(state: &State<ChatState>) -> EventStream![] {
    let user_id = state.user_count.fetch_add(1, Ordering::SeqCst);
    let username = format!("User{}", user_id + 1);
    let mut rx = state.tx.subscribe();

    // 发送欢迎消息
    let welcome_msg = ChatMessage {
        username: "Server".to_string(),
        content: format!("{} joined the chat!", username),
    };
    let _ = state.tx.send(welcome_msg);

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
        username: "User".to_string(),
        content: message.content.clone(),
    };
    let _ = state.tx.send(msg);
}
