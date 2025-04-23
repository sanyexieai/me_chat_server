# ME Chat Server

一个高性能的聊天服务器，使用 Rust 语言开发。

## 项目简介

ME Chat Server 是一个轻量级、高性能的聊天服务器，专为需要稳定、高效通信的应用场景设计。使用 Rust 语言开发，保证了系统的安全性和性能。

## 功能特性

- 🚀 高性能：基于 Rust 异步运行时，支持高并发
- 🔒 安全性：内存安全，无数据竞争
- 💡 简单易用：清晰的 API 接口，易于集成
- 📦 跨平台：支持 Windows、Linux、macOS
- 🔄 实时通信：支持 WebSocket 协议
- 📝 消息持久化：可选的消息存储功能

## 系统要求

- 操作系统：
  - Windows 10/11 64位
  - Linux (Ubuntu 20.04+, Debian 10+)
  - macOS 10.15+
- 内存：至少 512MB
- 磁盘空间：至少 100MB

## 快速开始

### 下载二进制文件

本项目为不同平台提供了预编译的二进制文件，您可以在 [Releases](https://github.com/sanyexieai/me_chat_server/releases) 页面下载最新版本。

#### 二进制文件命名规则

二进制文件的命名遵循以下规则：
- `me_chat_server-{platform}-{arch}`

其中：
- `platform`: 操作系统平台
  - `linux`: Linux 系统
  - `windows`: Windows 系统
  - `macos`: macOS 系统
- `arch`: 系统架构
  - `x86_64`: 64位系统

#### 可用的二进制文件

1. Linux 64位
   - 文件名: `me_chat_server-linux-x86_64`
   - 适用于: Ubuntu, Debian 等 Linux 发行版

2. Windows 64位
   - 文件名: `me_chat_server-windows-x86_64.exe`
   - 适用于: Windows 10/11 64位系统

3. macOS 64位
   - 文件名: `me_chat_server-macos-x86_64`
   - 适用于: macOS 10.15 及以上版本

### 安装步骤

1. 下载适合您系统的二进制文件
2. 对于 Linux/macOS 系统，添加执行权限：
   ```bash
   chmod +x me_chat_server-{platform}-x86_64
   ```
3. 运行程序：
   - Linux/macOS:
     ```bash
     ./me_chat_server-{platform}-x86_64
     ```
   - Windows:
     ```bash
     me_chat_server-windows-x86_64.exe
     ```

## 配置说明

服务器默认配置：
- 监听地址：`0.0.0.0:8080`
- WebSocket 路径：`/ws`
- 最大连接数：1000

您可以通过配置文件或环境变量修改这些设置。

## 开发指南

### 从源码构建

1. 安装 Rust 工具链：
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. 克隆项目：
   ```bash
   git clone https://github.com/sanyexieai/me_chat_server.git
   cd me_chat_server
   ```

3. 构建项目：
   ```bash
   cargo build --release
   ```

### 开发环境设置

1. 安装开发依赖：
   ```bash
   rustup component add rustfmt clippy
   ```

2. 运行测试：
   ```bash
   cargo test
   ```

3. 代码格式化：
   ```bash
   cargo fmt
   ```

## 贡献指南

我们欢迎任何形式的贡献，包括但不限于：
- 提交 Bug 报告
- 提出新功能建议
- 提交代码改进
- 完善文档

请查看 [CONTRIBUTING.md](CONTRIBUTING.md) 了解详细的贡献指南。

## 许可证

本项目采用 [MIT 许可证](LICENSE)。

## 联系方式

- 项目主页：https://github.com/sanyexieai/me_chat_server
- Issues：https://github.com/sanyexieai/me_chat_server/issues
- Discussions：https://github.com/sanyexieai/me_chat_server/discussions

## 致谢

感谢所有为项目做出贡献的开发者！ 