# JSDelivr Proxy

A lightweight JSDelivr Proxy with cache.

轻量级 JSDelivr 反代：回源 jsDelivr（或可自定义的镜像），并把资源缓存在**进程内**（TTL 2 小时）。
**不依赖任何外部服务**——没有 Redis，也不需要 MySQL / RabbitMQ，一个二进制即可运行。

HTTP 层基于 [axum](https://github.com/tokio-rs/axum) 0.8 + tower-http，
缓存基于 [moka](https://github.com/moka-rs/moka) 0.12（`future` 特性）。

## 缓存

* **TTL 2 小时**，与此前的 Redis 版本一致。
* **按字节计容**：`max_capacity_mb` 是整个缓存的字节预算（moka 的容量单位是
  weigher 权重，本项目的 weigher 返回条目的实际字节数），默认 256MB。
* **单条目上限**：超过 `max_entry_size_mb`（默认 16MB）的资源仍会正常返回给客户端，
  但不会进入缓存，避免单个超大文件挤占整个预算。
* **并发合并回源**：同一路径上的并发未命中只会触发一次上游请求。
* 淘汰策略是 moka 的 **W-TinyLFU**（带准入过滤的近似 LRU），而不是严格的 LRU。

**权衡（相对 Redis 版本的退步）**：缓存位于进程内，**重启即全部丢失**；
多副本部署时各副本各自持有一份缓存，**不再共享**，回源次数会随副本数上升。
由于回源是幂等的只读操作，这只影响缓存命中率，不影响正确性。
如果确实需要跨副本共享缓存，建议在本服务前面再放一层 CDN / 反代缓存，
而不是让服务本身重新背上一个有状态依赖。

## 资源白名单（反滥用）

公网部署时，一个不设限的 jsDelivr 反代等于一台**任何人都能白嫖的开放代理**：
别人只要把域名换成你的，就能用你的带宽和 IP 分发任意 jsDelivr 资源。
白名单让运维方把代理**钉死在自己的包 / 仓库上**。

**默认不限制**：不写 `[jsdelivr.allowlist]`（或三个列表全为空）时行为与之前完全一致，
老部署升级上来不受任何影响。只要任意一个列表非空，白名单即生效，
未命中的请求在**读缓存与回源之前**就被拒绝（返回 403，不产生上游流量，也不会污染缓存），
并以 `warn!` 记录被拒绝的路径。

```toml
[jsdelivr.allowlist]
# 允许的 provider；留空 = 不限制 provider
providers = ["npm", "gh"]
# 允许的 npm 包；留空 = 该 provider 下所有包都允许
npm = ["vue", "@hitokoto/core", "@hitokoto"]
# 允许的 GitHub 仓库；留空 = 该 provider 下所有仓库都允许
gh  = ["hitokoto-osc", "hitokoto-osc/sentences-bundle"]
```

匹配规则：

| 配置项 | 条目形式 | 含义 |
| --- | --- | --- |
| `providers` | `npm` | 放行该 provider |
| `npm` | `vue` | 精确匹配包名 |
| `npm` | `@hitokoto/core` | 精确匹配 scoped 包 |
| `npm` | `@hitokoto` | 匹配该 scope 下的所有包 |
| `gh` | `hitokoto-osc` | 匹配该 owner 的全部仓库 |
| `gh` | `hitokoto-osc/sentences-bundle` | 精确匹配单个仓库 |

几个必须知道的细节：

* **版本号先剥离再匹配**：`npm/vue@3.5.0/dist/vue.js` 按 `vue` 判定；
  scoped 包的 `@scope` 是独立片段，`@scope/pkg@1.2.3` 能被正确拆开。
* **`combine/` 会被拆开逐个校验**。`/combine/npm/a@1/x.js,npm/b@2/y.js`
  在一次请求里打包多个资源，只校验最外层路径等于给整个白名单开后门；
  本实现按 `,` 拆分后对**每一个分量**套用完整规则，任意一个不通过就整体 403。
* **匹配不区分大小写**（GitHub 的 owner/repo 大小写不敏感），
  且**严格按片段整体比较**：`hitokoto-osc` 不会匹配 `hitokoto-osc-evil`，
  `@hitokoto` 也不会匹配 `@hitokoto-evil`——前缀匹配是白名单最经典的绕过方式。
* **无法按资源粒度识别的 provider**（`wp`、`esm` 等）在白名单生效时，
  必须显式写进 `providers` 才会放行；无法解析的路径一律拒绝。

对应的环境变量（列表用**逗号**分隔）：

| 配置文件（`[jsdelivr.allowlist]`） | 环境变量 |
| --- | --- |
| `providers` | `JSDRLIVR_PROXY_JSDELIVR_ALLOWLIST_PROVIDERS` |
| `npm` | `JSDRLIVR_PROXY_JSDELIVR_ALLOWLIST_NPM` |
| `gh` | `JSDRLIVR_PROXY_JSDELIVR_ALLOWLIST_GH` |

```bash
JSDRLIVR_PROXY_JSDELIVR_ALLOWLIST_PROVIDERS="npm,gh" \
JSDRLIVR_PROXY_JSDELIVR_ALLOWLIST_NPM="vue,@hitokoto" ./jsdelivr_proxy
```

> 环境变量方式仅在**不使用 `-c <配置文件>`** 时生效（`-c` 会关掉环境变量 source，
> 这是既有行为）。列表键的逗号拆分在 `src/conf/mod.rs` 的 `LIST_VALUED_KEYS` 中登记。

## Docker 部署

仓库自带 compose 编排（仅 app 一个服务）：

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
   `JSDRLIVR_PROXY_SERVER_PORT`、`JSDRLIVR_PROXY_JSDELIVR_MIRROR`；
   配置项**字段名本身含下划线时，层级要用双下划线分隔**，否则会被当成多层嵌套而静默失效，
   例如 `JSDRLIVR_PROXY_CACHE__TTL_SECS`；
2. 配置文件：参考 `config.example.toml`，在 `./data/config.toml` 放置配置并挂载
   `./data:/app/data`。

可用的缓存配置项：

| 配置文件（`[cache]`） | 环境变量 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `ttl_secs` | `JSDRLIVR_PROXY_CACHE__TTL_SECS` | `7200` | 缓存存活时间（秒） |
| `max_capacity_mb` | `JSDRLIVR_PROXY_CACHE__MAX_CAPACITY_MB` | `256` | 缓存总字节预算（MB） |
| `max_entry_size_mb` | `JSDRLIVR_PROXY_CACHE__MAX_ENTRY_SIZE_MB` | `16` | 单条目上限（MB），超过则不缓存 |

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
