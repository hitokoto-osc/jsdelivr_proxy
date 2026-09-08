# JSDelivr Proxy

A lightweight JSDelivr Proxy with cache.

轻量级 JSDelivr 反代：回源 jsDelivr（或可自定义的镜像），并将资源缓存到 Redis（TTL 2 小时）。
仅依赖 Redis，不需要 MySQL / RabbitMQ。

HTTP 层基于 [axum](https://github.com/tokio-rs/axum) 0.8 + tower-http。

## Docker 部署

仓库自带 compose 编排（app + redis）：

```bash
docker compose up -d --build
```

compose 中通过 `JSDRLIVR_PROXY_SERVER_PORT=8000` 把容器端口固定为 `8000` 并映射到宿主机
`8000`。arm64 机器请把 `docker-compose.yml` 中的 dockerfile
改为 `manifest/docker/release/aarch64-linux-musl/Dockerfile`。

> 程序自身的默认监听地址是 `0.0.0.0:28319`（见 `src/conf/server.rs`），
> 直接运行二进制时用的是这个端口；compose 里的 `8000` 来自环境变量覆盖。

### 配置

支持两种方式，任选其一：

1. 环境变量（compose 默认方式），前缀为 `JSDRLIVR_PROXY_`，如
   `JSDRLIVR_PROXY_REDIS_HOST`、`JSDRLIVR_PROXY_JSDELIVR_MIRROR`；
2. 配置文件：参考 `config.example.toml`，在 `./data/config.toml` 放置配置并挂载
   `./data:/app/data`。

## 手动构建

迁移到 axum 后可在 stable 工具链上构建（已在 rustc 1.91 stable 验证）：

```bash
cargo build --release
```

> CI 与 `manifest/docker/**` 目前仍固定使用 nightly 工具链，本次未一并调整。

运行：

```bash
./target/release/jsdelivr_proxy          # 监听 0.0.0.0:28319
JSDRLIVR_PROXY_SERVER_PORT=8000 ./target/release/jsdelivr_proxy
```
