# JSDelivr Proxy

A lightweight JSDelivr Proxy with cache.

轻量级 JSDelivr 反代：回源 jsDelivr（或可自定义的镜像），并将资源缓存到 Redis（TTL 2 小时）。
仅依赖 Redis，不需要 MySQL / RabbitMQ。

## Docker 部署

仓库自带 compose 编排（app + redis）：

```bash
docker compose up -d --build
```

服务默认监听 `8000` 端口。arm64 机器请把 `docker-compose.yml` 中的 dockerfile
改为 `manifest/docker/release/aarch64-linux-musl/Dockerfile`。

### 配置

支持两种方式，任选其一：

1. 环境变量（compose 默认方式），前缀为 `JSDRLIVR_PROXY_`，如
   `JSDRLIVR_PROXY_REDIS_HOST`、`JSDRLIVR_PROXY_JSDELIVR_MIRROR`；
2. 配置文件：参考 `config.example.toml`，在 `./data/config.toml` 放置配置并挂载
   `./data:/app/data`。

## 手动构建

需要 Rust nightly 工具链：

```bash
cargo +nightly build --release
```
