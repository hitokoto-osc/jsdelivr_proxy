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
* **条目压缩**：缓存里的响应体默认用 **zstd** 压缩（`compression`，可选
  `none` / `zstd` / `brotli`），上面两项预算都按**压缩后**的体积核算。
* **并发合并回源**：同一路径上的并发未命中只会触发一次上游请求。
* 淘汰策略是 moka 的 **W-TinyLFU**（带准入过滤的近似 LRU），而不是严格的 LRU。

### 压缩

`.js` / `.css` / `.json` 这类文本资源 zstd 通常能压到 1/3 ~ 1/4，
等于同样的 `max_capacity_mb` 能装下三四倍的热点文件；
代价是**每次命中都要解压一次**（不压缩时命中只是一次引用计数加一）。

| `compression` | 说明 |
| --- | --- |
| `zstd`（默认） | 压缩率与速度均衡，解压吞吐在 GB/s 量级 |
| `brotli` | 文本压缩率略高于 zstd，压缩慢得多；固定 quality 5 |
| `none` | 不压缩，命中路径是零拷贝，内存里存的就是原始字节 |

已经压过的资源（`woff2`、`png`、`wasm` 等）压不动时会**原样存放**，
因此开启压缩不会让缓存占用反而上升。压缩等级与字典目前不可配置。

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

## 资源预载（Preload）

冷启动之后第一个请求每个资源的人，都要替所有人吃一次回源延迟（实测 gcore 回源
0.7 ~ 2.3 秒）。预载把指定仓库 / 包的前端资源在**启动时**就灌进缓存，
让第一个真实请求也是命中（实测 4ms）。

**默认不预载**：不写 `[preload]`（或 `targets` 为空）时行为与之前完全一致。

```toml
[[preload.targets]]
provider = "gh"
name = "hitokoto-osc/sentences-bundle"
```

只写这两行就够了：版本默认取**仓库的默认分支**，内容默认取**全部前端资源文件**
并跳过 `.` 开头的隐藏目录。

### 版本怎么定

| provider | `version` 省略时 | 说明 |
| --- | --- | --- |
| `gh` | `HEAD` | jsDelivr 侧解析为仓库的默认分支（实测 `@HEAD` 与 `@<默认分支>` 返回同一份清单） |
| `npm` | `latest` | dist-tag |

`version` 也可以写成分支名、tag、commit 或 semver 范围。清单接口只认具体版本
（`npm/vue@latest` 会被它拒掉），因此 dist-tag 与 semver 范围会先经
`/v1/packages/.../resolved?specifier=` 换算一次；反过来 gh 的分支名能被清单接口
直接接受，却不能被 `resolved` 解析。两条路都试过，写哪种形态都能用。

> gh 的 `latest` 在 jsDelivr 的原意是「最新的 **tag**」，和「默认分支」不是一回事。
> 本项目按「默认 = 默认分支」的语义把 gh 的 `latest` 归一成了 `HEAD`；
> 要最新 tag 请直接写 tag，或者写 semver 范围（如 `"1.0"`）让 `resolved` 去挑。

**必须知道的一点**：预热的是**精确的缓存键**，而缓存键就是客户端请求的路径
（`gh/<owner>/<repo>@<version>/<file>`）。`version` 会原样进键，所以它要和客户端
实际请求的版本串一致 —— 预热 `@HEAD` 命中不了请求 `@master` 的客户端。
要覆盖多个版本串，就写多个 `[[preload.targets]]`。启动日志会打出每个目标
预热出来的键前缀，可以直接核对。

### 内容怎么选

默认放行的扩展名（不区分大小写）：

| 类别 | 扩展名 |
| --- | --- |
| 脚本 | `js` `mjs` `cjs` |
| 样式 | `css` |
| 标记 / 结构化数据 | `json` `html` `htm` `wasm` |
| 图片 | `apng` `avif` `bmp` `gif` `ico` `jpeg` `jpg` `png` `svg` `webp` |
| 字体 | `eot` `otf` `ttf` `woff` `woff2` |

刻意**不**收录 `map`（source map，只对调试有用且动辄数 MB）、`d.ts`、`md` 等
浏览器不会加载的文件。

`.` 开头的**隐藏目录与隐藏文件**（`.github/`、`.vscode/`、`.gitignore`）默认全部跳过，
判定按路径片段进行，`vue.global.min.js` 这种带点的文件名不受影响。

四个可选的收窄 / 放宽开关：

| 字段 | 作用 |
| --- | --- |
| `extensions` | 覆盖默认集合；`["*"]` = 不按扩展名过滤 |
| `include` | 只要这些路径前缀下的文件；留空 = 整个仓库 |
| `exclude` | 排除这些前缀，优先级高于 `include` |
| `include_hidden` | 连隐藏目录一起预载，默认 `false` |

`include` / `exclude` 按**路径片段整体**比较，不是裸的 `starts_with`：
`/dist` 命中 `/dist/vue.js`，但不会命中 `/dist-old/x.js`。

### 刷新与开销

预载条目和普通条目共用同一份 TTL，`get_or_fetch` 命中时既不回源也不续期，
所以只载一次的话 TTL 一到就全没了。预载因此是个**周期性任务**，
默认周期是 `cache.ttl_secs` 的 3/4（默认 TTL 下即 5400 秒），保证条目在过期前被续上。

续期**不等于**重新下载：清单接口带有每个文件的 SHA-256，摘要没变且条目还在缓存里时，
只把旧值重新写回去重置 TTL，一个字节都不用重传。日志里的 `warmed` / `renewed`
就是这两条路径的计数。

几道防呆闸：

* 清单里体积超过 `cache.max_entry_size_mb` 的文件在**下载之前**就被跳过
  —— 反正下下来也进不了缓存；
* 单个目标最多预载 `max_files` 个文件（默认 512）；
* 计划下载量超过整个缓存预算时打 `warn!`（那种配置下条目只会互相淘汰）；
* 被资源白名单拒绝的目标直接跳过并告警：那些资源永远服务不出去，
  预热它们只是白占缓存预算。

预载失败（数据 API 不可达、目标配错等）只影响预热本身，HTTP 服务照常启动与服务。

### 完整配置项

| 配置文件（`[preload]`） | 环境变量 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `enabled` | `JSDRLIVR_PROXY_PRELOAD_ENABLED` | 有 `targets` 即开 | 总开关 |
| `data_api` | `JSDRLIVR_PROXY_PRELOAD__DATA_API` | `https://data.jsdelivr.com` | 列清单用的数据 API |
| `refresh_interval_secs` | `JSDRLIVR_PROXY_PRELOAD__REFRESH_INTERVAL_SECS` | `cache.ttl_secs` x 3/4 | 刷新周期，下限 60 秒 |
| `concurrency` | `JSDRLIVR_PROXY_PRELOAD_CONCURRENCY` | `4` | 单目标内的并发回源数 |
| `max_files` | `JSDRLIVR_PROXY_PRELOAD__MAX_FILES` | `512` | 单目标预载文件数上限 |

> `data_api` 与 `jsdelivr.mirror` 是**两个不同的服务**：gcore 等镜像只分发资源，
> 不提供 `/v1/packages` 清单接口，所以它们分开配。
>
> `[[preload.targets]]` 是数组表，`config` 的环境变量 source 表达不了，
> **只能写在配置文件里**；上面这些标量项才能用环境变量覆盖。

## 管理面板与 Webhook

**默认全部关闭**：不设置管理员 Key 时，`/admin` 与管理 API 一律返回 `503`，
不设置 Webhook secret 时 `/webhook/cache/purge` 同样返回 `503`。
老部署升级上来不会多出任何可访问的入口。

```bash
# 管理面板 / 管理 API（缺省即关闭）
JSDRLIVR_PROXY_ADMIN_KEY="用一个足够长的随机串"
# 缓存清除 Webhook（缺省即关闭），与上面是两把独立的钥匙
JSDRLIVR_PROXY_ADMIN__WEBHOOK_SECRET="另一个随机串"
```

### 面板

浏览器打开 `http://<host>:<port>/admin`，填入管理员 Key 即可看到：

* **进程**：CPU 占用、常驻内存 RSS、虚拟内存、运行时长（每 5 秒采样一次）；
* **缓存**：条目数、已占用 / 预算、原始体积与实际压缩比，以及当前 TTL 等配置；
* **缓存条目**：按占用从大到小列出，可按前缀筛选，可单条清除、按前缀清除或清空；
* **操作记录**：谁在什么时候从哪个地址清了什么。

页面本身不含任何数据，也不加载任何外部资源；Key 只存在浏览器 `localStorage` 里，
每次请求以 `Authorization: Bearer <key>` 发出。

### 管理 API

全部要求 `Authorization: Bearer <管理员 Key>`，响应沿用项目统一的
`{status, message, data, ts}` 结构。

| 方法与路径 | 说明 |
| --- | --- |
| `GET /admin/api/stats` | 进程与缓存汇总 |
| `GET /admin/api/cache?prefix=&limit=&offset=` | 缓存条目列表，按占用降序；`limit` 默认 100、上限 1000 |
| `POST /admin/api/cache/purge` | 清除缓存，请求体见下 |
| `GET /admin/api/audit?limit=&offset=` | 操作记录，最新在前 |

清除请求体**三选一**，同时给多个会被拒绝（避免「本想清一个前缀，结果清了全部」）：

```jsonc
{ "all": true }                                    // 全部
{ "prefix": "gh/hitokoto-osc/sentences-bundle@HEAD" }  // 按前缀
{ "keys": ["npm/vue@3.4.21/dist/vue.global.prod.js"] } // 精确若干条
```

缓存键就是客户端请求的路径，所以前缀 `gh/owner/repo@HEAD` 只清这一个版本，
`gh/owner/repo` 则连带该仓库的所有版本。返回里的 `missed` 是「点名了但本来就不在缓存里」的条数。

### Webhook

```bash
BODY='{"prefix":"gh/hitokoto-osc/sentences-bundle@HEAD"}'
SIG=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$WEBHOOK_SECRET" -r | cut -d' ' -f1)
curl -X POST http://<host>:<port>/webhook/cache/purge \
  -H "Content-Type: application/json" \
  -H "X-Hub-Signature-256: sha256=$SIG" \
  -d "$BODY"
```

请求体与管理 API 的清除接口完全一致，鉴权换成对**原始请求体**的 HMAC-SHA256 签名，
放在 `X-Hub-Signature-256` 头里（GitHub / Gitea 的 webhook 格式，`sha256=` 前缀可省略）。

之所以不复用管理员 Key：一是签名是 GitHub 这类服务唯一能配的凭证，
二是把 secret 交给 CI 或仓库以后，它泄露也只等于「能清缓存」，不等于拿到管理员权限。

> 签名校验失败**不会**写进操作记录。这个端点在校验通过前是无鉴权的，
> 逐次记录会让任何人都能把远端审计库灌满；失败仍然以 `warn` 记进程序日志。

### 操作记录存哪

默认存在**进程内的环形缓冲**里（最近 500 条），重启即丢失——这保住了「一个二进制、
零外部依赖」的默认部署形态。需要持久化时配上 Turso：

```bash
JSDRLIVR_PROXY_ADMIN_TURSO_URL="libsql://<db>-<org>.turso.io"
JSDRLIVR_PROXY_ADMIN_TURSO_TOKEN="<turso db tokens create 生成的 token>"
```

表（`admin_audit_log`）会在启动时自动建好。启动时连不上 Turso**不会**导致启动失败，
只会退回内存缓冲并打一条 `warn`——记不了审计日志不该让反代本身停摆。

实现上没有引入官方的 `libsql` SDK：它的远程路径建在比 `reqwest` 更旧的一代 HTTP
栈上，接进来会多出 37 个 crate，以及 rustls / tower / hyper-rustls / thiserror 各两份。
Turso 的 `/v2/pipeline` 是有正式文档的稳定 JSON 协议，直接用现成的 `reqwest` 说这个协议，
依赖树一个字节都没变。

### 完整配置项

| 配置文件（`[admin]`） | 环境变量 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `key` | `JSDRLIVR_PROXY_ADMIN_KEY` | 空 = 关闭 | 管理面板与管理 API 的 Key |
| `webhook_secret` | `JSDRLIVR_PROXY_ADMIN__WEBHOOK_SECRET` | 空 = 关闭 | Webhook 的 HMAC secret |
| `turso.url` | `JSDRLIVR_PROXY_ADMIN_TURSO_URL` | 空 = 用内存 | Turso 数据库地址，`libsql://` 会被改写成 `https://` |
| `turso.token` | `JSDRLIVR_PROXY_ADMIN_TURSO_TOKEN` | 空 = 用内存 | Turso 访问 token |

> `webhook_secret` 字段名自身带下划线，所以层级必须用**双下划线**分隔，
> 写成 `JSDRLIVR_PROXY_ADMIN_WEBHOOK_SECRET` 会被解析成 `admin.webhook.secret` 而静默失效。

## Docker 部署

### 预构建镜像

每次打 tag 发布时，同一份构建会同时推送到 Docker Hub 与 GitHub Packages，
两边内容完全一致（同一个 manifest list，含 `linux/amd64` 与 `linux/arm64`），
按网络情况任选其一即可：

```bash
docker pull hitokoto/jsdelivr-proxy:latest              # Docker Hub
docker pull ghcr.io/hitokoto-osc/jsdelivr_proxy:latest  # GitHub Packages
```

除 `latest` 外还提供 `vX.Y.Z` / `vX.Y` / `vX` 三级 tag。

### 从源码构建

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
| `compression` | `JSDRLIVR_PROXY_CACHE_COMPRESSION` | `"zstd"` | 条目压缩算法：`none` / `zstd` / `brotli` |

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
