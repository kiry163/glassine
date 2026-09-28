# glassine 设计规范

- 日期：2026-09-28
- 状态：待评审
- 平台：Windows 优先（最低 Windows 10 1809），macOS 后续
- 依据：本规范中的所有平台行为均来自 `glassine-spike` 的实测（数据见第 17 节），非推断

---

## 1. 目的

glassine 是一个 Windows 桌面透明挂件。它**独立可用**——启动即在桌面顶层显示当前时间；同时提供一个 localhost HTTP 接口，使任何语言编写的本地进程都能把窗口内容替换为自定义文本。

它不是库、不是框架、不是可嵌入组件。它是一个单一可执行文件。

## 2. 术语

| 术语 | 含义 |
|---|---|
| 窗口 | 唯一的分层窗口，无边框、置顶、不接收鼠标事件 |
| 模式 | 窗口当前显示什么：`time`、`text`、`blank` 三者之一 |
| 表面 | 一块 top-down 32bpp DIB 的内存，内容为预乘 BGRA |
| 调用方 | 通过 HTTP 向 glassine 发送命令的本地进程 |

## 3. 产品行为（可观测契约）

1. 进程启动后，在配置指定的位置显示当前时间。
2. 收到 `POST /text` 时，窗口内容替换为请求体中的文本，模式变为 `text`。
3. 文本模式**持续有效**，直到显式收到 `POST /time` 或进程重启。不设自动回退。
4. 文本按窗口宽度自动换行；超出窗口高度的行被截断，截断状态可经 `GET /status` 查询。
5. 空请求体使窗口完全不显示任何内容（模式 `blank`，全透明）。
6. 窗口永不接收鼠标事件：窗口矩形内的任何点击都落到下层窗口。
7. 窗口永不出现在任务栏或 Alt-Tab 列表中，且从不抢焦点。
8. 窗口始终位于普通窗口之上。
9. 进程无托盘图标、无右键菜单、无任何用户界面入口。唯一退出方式是 `POST /quit` 或终止进程。

## 4. 非目标（明确不做）

- 滚动、淡入淡出、打字机或任何连续动画；窗口内不存在帧时钟
- 图像、动图、图标
- 矢量图形、进度条、卡片背景、圆角、阴影
- 多个窗口或多个实例
- 用户交互：拖拽、缩放、内嵌编辑、托盘、右键菜单、全局热键
- 歌词概念、倒计时、秒表、番茄钟
- 对外可嵌入的三方库形式
- 独立的命令行客户端（`curl` 即客户端）
- 配置热重载
- HTTP 鉴权

这些不是"以后再说"的占位项，而是当前范围的边界。任何一项要进入范围都需要单独的决策。

## 5. 系统结构

单个 cargo crate `glassine`，模块划分：

| 模块 | 职责 | 依赖窗口？ |
|---|---|---|
| `config` | 解析、校验 TOML 配置，解析锚点为像素矩形 | 否 |
| `content` | 内容状态机（`time` / `text` / `blank`）与时钟 | 否 |
| `layout` | 文本整形与排版（cosmic-text） | 否 |
| `render` | 覆盖率 → 预乘 → 合成的像素管线 | 否 |
| `platform::win` | 窗口、DIB、`UpdateLayeredWindow`、消息循环 | 是 |
| `http` | HTTP 服务、请求解析、命令投递 | 否 |
| `logging` | 文件日志 | 否 |

**约束**：除 `platform` 外所有模块不得引用 `windows` crate。这条约束使整条渲染管线可以在无窗口环境下被单元测试，也是 macOS 移植只需要重写 `platform` 的前提。

依赖：`cosmic-text`（整形 + 光栅）、`windows`、`toml` + `serde`、`time` 或 `chrono`（时钟格式化）。**不引入 `tiny-skia`**（理由见第 17.3 节）。

## 6. 窗口与呈现

### 6.1 窗口配方（实测确认）

```
样式：  WS_POPUP
扩展：  WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOPMOST
实测 exstyle = 0x080800A8，WS_EX_APPWINDOW 未置位
```

| 扩展样式 | 作用 |
|---|---|
| `WS_EX_LAYERED` | 逐像素 alpha 的前提 |
| `WS_EX_TRANSPARENT` | 整窗点击穿透（无需逐像素命中测试） |
| `WS_EX_TOOLWINDOW` | 不出现在任务栏与 Alt-Tab |
| `WS_EX_NOACTIVATE` | 永不抢焦点 |
| `WS_EX_TOPMOST` | 置顶 |

进程以 Windows GUI 子系统构建（`#![windows_subsystem = "windows"]`），避免控制台窗口与 conhost 子进程。

### 6.2 呈现路径

```
CreateDIBSection(biBitCount=32, biCompression=BI_RGB, biHeight=-h)   // top-down
  → 渲染为预乘 RGBA
  → 原地交换 R/B 得到预乘 BGRA
  → UpdateLayeredWindow(..., BLENDFUNCTION{AC_SRC_OVER,0,255,AC_SRC_ALPHA}, ULW_ALPHA)
```

DIB 的 stride 恒等于 `width * 4`（32bpp 行天然 4 字节对齐），因此无需处理行填充。

### 6.3 强制约束：禁止 `SetLayeredWindowAttributes`

对同一个分层窗口调用过一次 `SetLayeredWindowAttributes` 之后，`UpdateLayeredWindow` 会以 `ERROR_INVALID_PARAMETER (0x80070057)` 失败，**且此后永久失败**（实测：一次调用之后连续 43 次呈现全部失败，窗口内容冻结）。

因此：

- 代码中**不得出现** `SetLayeredWindowAttributes`。
- 配置里的整体不透明度**必须实现为像素缓冲中的 alpha 系数**（在合成时对每个像素的 alpha 与 RGB 统一缩放），而不是窗口属性。
- 若未来某个功能必须使用它，唯一出路是重置 `WS_EX_LAYERED` 位后重建窗口状态，且必须重新呈现整个表面。

### 6.4 `WM_PAINT` 必须验证更新区域

窗口通过 `UpdateLayeredWindow` 呈现，`WM_PAINT` 不携带任何内容，但**仍必须调用 `BeginPaint`/`EndPaint`**。省略它们会使更新区域保持脏状态，系统无限重投 `WM_PAINT`——实测烧掉 **80.9% 的单核**。这是"从不真正绘制"的窗口特有的陷阱。

`WM_PAINT` 处理：`BeginPaint` → `EndPaint` → 返回 0。`WM_ERASEBKGND` 返回 1。

### 6.5 置顶重断言

其他窗口或全屏应用可能夺走 topmost。配置项 `window.reassert_topmost_ms`（默认 5000，0 表示关闭）控制一个周期性重断言：若窗口已不再具有 `WS_EX_TOPMOST`，则以 `SetWindowPos(HWND_TOPMOST, SWP_NOMOVE|SWP_NOSIZE|SWP_NOACTIVATE)` 恢复。此做法借鉴 Catime 的生产经验（其 `window_topmost_retry.c`）。

该定时器**只读取扩展样式**，不接触表面、不触发重绘；样式已正确时是一次无副作用的读取。这是它能与第 14 节的空闲 CPU 预算共存的原因。

### 6.6 DPI 与多显示器

- 进程声明 Per-Monitor V2 DPI 感知（`SetProcessDpiAwarenessContext`）。
- 配置中的坐标与尺寸单位是**物理像素**。
- 锚点 + 偏移在**目标显示器的工作区**坐标系内解析为窗口矩形。
- 收到 `WM_DPICHANGED` 时，按新 DPI 重新解析锚点并重绘（文本尺寸按配置的物理像素值保持不变，不随 DPI 缩放）。
- 收到 `WM_DISPLAYCHANGE` 时，重新解析锚点。

## 7. 渲染管线

每次重绘的步骤（顺序固定）：

1. 用全透明（`[0, 0, 0, 0]`）填充整个表面。glassine 没有背景色概念——窗口除了文字之外处处全透明（含 `blank` 模式）。
2. `Buffer::set_size(width, height)`、`Buffer::set_text(text, attrs, Shaping::Advanced, None)`、`Buffer::shape_until_scroll`。
3. 遍历 `layout_runs()`，对每个 glyph 求 `physical((0.0, run.line_y), 1.0)`，得到 `(x, y, cache_key)`。
4. 对每个 cache_key 调 `SwashCache::get_image`；只处理 `Content::Mask`（轮廓字体）与 `Content::Color`（彩色字形，如 emoji）。
5. 对每个覆盖像素：
   - `alpha = coverage * color_alpha / 255`
   - `src = [R*alpha/255, G*alpha/255, B*alpha/255, alpha]`（预乘）
   - 以 source-over 合成到表面：`dst = src + dst * (255 - alpha) / 255`
6. 原地交换每个像素的 R 与 B（RGBA → BGRA）。
7. `UpdateLayeredWindow`。

**该管线的每一步都已被实测验证为数值正确**（第 17.2 节）。

重绘触发条件（全部）：

| 触发 | 频率 |
|---|---|
| `WM_APP_GLASSINE`（即 `WM_APP + 1`，HTTP 命令到达） | 按需 |
| `WM_TIMER` 时钟 tick | 见 7.1 |
| `WM_DPICHANGED` / `WM_DISPLAYCHANGE` | 系统事件 |

置顶重断言（见 6.5）**不属于**重绘触发：它只修正 z 序与扩展样式，分层表面的内容不变。

### 7.1 时钟 tick 间隔

- 若 `clock.format` 含 `%S` 或亚秒格式符，tick = 1000 ms。
- 否则 tick = 60000 ms。
- `clock.tick_ms` 可显式覆盖。
- `blank` 与 `text` 模式下 tick 仍然运行（切回 `time` 时无需等到下一个整秒）。

## 8. 内容模型与排版

### 8.1 状态机

```
        POST /text（非空）        POST /text（空体）
time ──────────────────────► text ──────────────────────► blank
  ▲                            │                            │
  └────────────────────────────┴────────────────────────────┘
                    POST /time
```

无自动回退、无超时。进程重启回到 `time`。

### 8.2 文本排版规则

- 换行：`\n` 强制换行；同时按窗口宽度自动换行（CJK 逐字断行，拉丁按词断行，由 cosmic-text 的换行算法处理）。
- 溢出：当排版后的行数超出窗口高度可容纳的行数时，**丢弃底部超出的行**，不缩小字号、不滚动。
- 截断状态记录在内容状态中，经 `GET /status` 的 `truncated` 字段暴露。
- 对齐：`text.align`（`left` / `center` / `right`），默认 `center`。
- 字形缓存由 `SwashCache` 持有，跨帧复用。

### 8.3 时钟排版

`clock.format` 为 strftime 风格模板，默认 `%H:%M:%S`，用配置的 `clock.utc_offset`（默认本机时区）求值。排版规则与 8.2 相同。

## 9. HTTP 接口

### 9.1 绑定

- 地址：`127.0.0.1`，**绝不绑定 `0.0.0.0`**。
- 端口：`server.port`，默认 `17321`。
- 端口被占用：启动失败，以非零退出码退出，日志与 stderr 说明原因。**不静默换端口。**
- 无鉴权。理由：能连接本机回环的进程本已可读配置、注入 DLL；token 不改变威胁模型，只增加调用方负担。

### 9.2 端点

四个端点，无路径前缀。

#### `POST /text`

请求体：

- `Content-Type: text/plain` → 正文即文本（UTF-8）
- `Content-Type: application/json` → `{"text": "..."}`
- 其他 `Content-Type`：若正文为合法 UTF-8，按纯文本处理；否则 400

行为：设置文本，模式变为 `text`（正文为空串时模式为 `blank`）。

响应：

```json
{"ok": true, "mode": "text"}
```

失败：

```json
{"ok": false, "error": {"code": "payload_too_large", "message": "body exceeds 65536 bytes"}}
```

#### `POST /time`

请求体被忽略。模式变为 `time`。

```json
{"ok": true, "mode": "time"}
```

#### `POST /quit`

返回响应后进程退出（退出码 0）。响应：

```json
{"ok": true}
```

顺序是强制的：响应体**写入并 flush 之后**，才把 `Command::Quit` 投递给窗口线程，窗口线程调用 `PostQuitMessage(0)`。若先退出进程，调用方会拿到连接重置而不是响应。

#### `GET /status`

```json
{
  "ok": true,
  "mode": "text",
  "window": {"x": 120, "y": 120, "width": 640, "height": 220},
  "monitor": {"name": "\\\\.\\DISPLAY1", "dpi": 96},
  "truncated": false,
  "text_bytes": 42
}
```

字段定义：

| 字段 | 含义 |
|---|---|
| `mode` | `"time"` \| `"text"` \| `"blank"` |
| `window` | 窗口当前的物理像素矩形（`GetWindowRect` 的结果，可能因 DPI/显示器变化与配置不同） |
| `monitor.name` | 窗口所在显示器的设备名 |
| `monitor.dpi` | 窗口当前的 DPI |
| `truncated` | 当前内容排版时是否发生过行截断；`time`/`blank` 模式下恒为 `false` |
| `text_bytes` | 当前文本内容的 UTF-8 字节数；`time`/`blank` 模式下为 `0` |

`/status` 读取的是窗口线程发布的快照（见第 12 节），**不阻塞窗口线程**，也不要求窗口线程参与响应。

### 9.3 限制与错误码

| 条件 | 状态码 | `error.code` |
|---|---|---|
| 请求体 > 64 KiB（另有 `Content-Length` 声明 > 64 KiB 时直接拒绝） | 413 | `payload_too_large` |
| JSON 解析失败 | 400 | `invalid_json` |
| 未知路径 | 404 | `unknown_endpoint` |
| 未知方法 | 405 | `method_not_allowed` |
| 正文非法 UTF-8 | 400 | `invalid_utf8` |
| 连接超时（2 s 读/写） | 408 | `timeout` |

所有响应 `Content-Type: application/json`。错误响应体格式与成功响应一致（`ok: false` + `error`）。

### 9.4 并发语义

调用方被假定为**串行调用**（一次一行、等待响应）。服务端据此简化：

- 一个 accept 线程，**一次处理一个连接**，读/写超时各 2 秒。
- 命令按到达顺序进入通道，窗口线程按顺序应用。因此并发调用下"后到覆盖先到"。
- 一个卡住的客户端最多阻塞其他调用方 2 秒，之后连接被关闭。

### 9.5 客户端断开

调用方断开连接不影响窗口：窗口保持当前内容，进程继续运行。这是"窗口内容属于 glassine，不属于任何调用方"的直接结果。

## 10. 配置

### 10.1 位置与覆盖

- 默认：`%APPDATA%\glassine\config.toml`
- 覆盖顺序（后者优先）：默认值 < 配置文件 < 环境变量 < 命令行参数
- 命令行：`--config <path>`、`--port <n>`、`--log-level <level>`、`--check-config`、`--install-autostart`、`--uninstall-autostart`
- 环境变量：`GLASSINE_CONFIG`、`GLASSINE_PORT`、`GLASSINE_LOG`

| 参数 | 行为 |
|---|---|
| `--check-config` | 解析并校验配置，打印将要创建的窗口矩形、目标显示器与其工作区、端口、字体解析结果，**然后退出（码 0）**，不创建窗口、不绑定端口。这是窗口跑到屏幕外时的唯一事前补救手段。 |
| `--install-autostart` | 在启动文件夹创建快捷方式并退出 |
| `--uninstall-autostart` | 删除该快捷方式并退出 |

### 10.2 内容

```toml
[window]
anchor = "top-center"        # top/middle/bottom × left/center/right
offset = [0, 64]             # 相对锚点的物理像素偏移
size = [640, 220]            # 物理像素
monitor = "primary"          # "primary" | 索引数字 | 设备名
opacity = 100                # 0-100，实现为像素 alpha 缩放
reassert_topmost_ms = 5000   # 0 关闭

[text]
family = "Microsoft YaHei"   # 找不到时回退到系统 sans-serif
size = 34.0                  # 物理像素
weight = 400
line_height = 44.0
color = "#FFFFFF"
alpha = 230                  # 0-255
align = "center"

[clock]
format = "%H:%M:%S"
utc_offset = "local"         # "local" 或 "+08:00" 形式
# tick_ms = 1000             # 不设时由 format 推导（见 7.1）

[server]
port = 17321

[log]
level = "info"
```

#### 值约定

| 键 | 形式 |
|---|---|
| `anchor` | `"<vertical>-<horizontal>"`，vertical ∈ {`top`, `middle`, `bottom`}，horizontal ∈ {`left`, `center`, `right`}，共 9 种 |
| `offset` | `[dx, dy]` 整数，**统一采用屏幕坐标系：+x 向右、+y 向下**，对九种锚点一律适用。例：`anchor = "bottom-right"` + `offset = [-20, -20]` 表示从右下角向内缩 20 px |
| `size` | `[w, h]` 整数，物理像素，两维均须 ≥ 1 |
| `monitor` | 字符串。`"primary"`，或十进制索引（`"1"` = 第二块显示器，按 `EnumDisplayMonitors` 顺序），或设备名（如 `"\\\\.\\DISPLAY2"`） |
| `color` | `"#RRGGBB"`。**不接受** `#RRGGBBAA`——透明度只由 `alpha` 表达 |
| `alpha` | 0–255，与 `color` 相乘得到文本最终的 alpha |
| `weight` | 数字 100–900（CSS 语义），用于选择字面；缺该字面时选最接近的 |
| `opacity` | 0–100。**不**改变 `text.alpha`，而是在合成完成后对整幅表面的 A、R、G、B 四通道统一缩放（预乘语义下这是正确做法） |
| `utc_offset` | `"local"` 或 `"+HH:MM"` / `"-HH:MM"` |

### 10.3 校验与失败行为

- 配置缺省字段使用默认值；**非法值一律启动失败**并打印具体字段与原因，退出码 2。
- 非法值包括：未知的 `anchor`/`align`/`monitor` 形式、`size` 任一维 ≤ 0、`opacity` 超出 0–100、`alpha` 超出 0–255、`color` 无法解析、`port` 为 0 或 > 65535、`format` 产生非 UTF-8 结果、字体族不存在（回退成功则仅告警）。
- **不存在静默回落默认值的路径**。理由：点穿窗口没有交互入口，配置错误若被静默吸收，表现为"窗口不见了"，是最难排查的故障。
- 配置合法但窗口落在屏幕外（例如刻意写下屏幕外的偏移）时进程**不会拒绝**——这是允许的，可用于贴边效果。补救手段是 `--check-config`（事前，见 10.1）与 `GET /status`（事后），两者都报告实际矩形。

## 11. 生命周期与运维

| 关注点 | 方案 |
|---|---|
| 单实例 | 命名互斥体 `Glassine.SingleInstance`；已存在则打印"已有实例在运行"、退出码 1 |
| 退出 | `POST /quit`；进程终止信号；无托盘、无右键入口 |
| 崩溃隔离 | glassine 崩溃不影响调用方；调用方断开不影响 glassine |
| panic 处理 | panic hook 写日志后 `abort` |
| 日志 | `%LOCALAPPDATA%\glassine\logs\glassine.log`，按大小轮转（单文件 1 MiB，保留 3 个） |
| 日志内容 | 启动配置摘要、绑定端口、每次命令的端点与结果码、窗口矩形、置顶重断言、错误 |
| 日志不含 | 完整文本内容（只有字节数），避免把调用方数据写进磁盘 |
| 开机自启 | `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\glassine.lnk`，默认**关闭**，由 `--install-autostart` 创建、`--uninstall-autostart` 删除（见 10.1）。glassine 不提供安装包，因此不由安装器写入 |

## 12. 线程模型与数据流

```
调用方 ──HTTP──► accept 线程 ──解析──► Command ──┐
                                                 │ mpsc::Sender
                                                 ▼
                                        Channel<Command>
                                                 ▲
                                                 │ drain
主线程（唯一窗口线程）：注册窗口 → 消息循环 ◄──────┘
   GetMessage 阻塞
   ├─ WM_APP_GLASSINE  → 排空通道 → 更新内容状态 → 重绘 → UpdateLayeredWindow
   ├─ WM_TIMER         → 更新时钟 → 重绘
   └─ WM_DPICHANGED    → 重新解析锚点 → 重绘
```

**关键约束**：窗口线程阻塞在 `GetMessage` 上，通道投递无法唤醒它。HTTP 线程在投递命令后**必须**调用 `PostMessageW(hwnd, WM_APP_GLASSINE, 0, 0)`。省略这一步的症状是"接口返回 200 但窗口不刷新"。

`HWND` 通过 `AtomicIsize` 在启动时发布给 HTTP 线程（窗口创建早于服务器绑定）。

**`/status` 的数据来源**：内容状态由窗口线程拥有，而 HTTP 线程需要读它。采用 `Arc<Mutex<StatusSnapshot>>` 发布：窗口线程在每次内容状态变化、DPI 变化或窗口矩形变化后，用最新的 `{mode, rect, monitor, dpi, truncated, text_bytes}` 覆盖快照；HTTP 线程加锁克隆。快照是只读纯数据，**不含 GDI 句柄、不含表面指针**，因此不存在跨线程访问窗口资源的风险。HTTP 线程从不向窗口线程索取数据——那会造成反向依赖与死锁风险。

**所有渲染都在主线程完成**。HTTP 线程不接触表面、不接触 GDI 对象。

## 13. 错误处理

| 场景 | 行为 |
|---|---|
| 配置非法 | 启动失败，退出码 2，打印字段与原因 |
| 端口被占用 | 启动失败，退出码 3 |
| 已有实例 | 退出码 1 |
| 呈现失败（`UpdateLayeredWindow` 返回错误） | 记录错误码；丢弃该帧；**不重试、不降级**；下次触发时重绘 |
| 字体族缺失 | 回退到系统 sans-serif 并告警，不失败 |
| 字形光栅失败 | 跳过该字形，记录次数 |
| 文本超过 64 KiB | 413，内容状态不变 |
| 内容状态不变的所有失败路径 | 窗口保持上一帧内容——绝不清空 |

## 14. 资源预算与验收

数值来自 spike 实测（单窗口 640×220、34px 中文两行、20 字形/帧、1Hz 重绘）：

| 指标 | 实测 | 验收上限 |
|---|---|---|
| Working Set | 9.2 – 9.8 MB | **≤ 15 MB** |
| Private Bytes | 1.9 – 2.0 MB | **≤ 5 MB** |
| 空闲 CPU（1Hz 重绘） | 0.312% 单核 | **≤ 0.5%** |
| 启动 → 首帧 | 16.5 – 19.1 ms | **≤ 50 ms** |
| 单帧成本 p95 | ≈ 170 µs | **≤ 1 ms** |
| 单 exe 体积 | 2.0 MB | **≤ 5 MB** |
| 稳态线程数 | 1（窗口线程）+ 1（HTTP accept） | **≤ 4**（含可能的日志线程） |

参考对照：Catime v1.6.2 在**同一台机器**实测 Working Set 19.4 MB、Private 4.9 MB、空闲 CPU 0.21%、句柄 325；一个平凡的 Rust 二进制实测 4.2 MB / 0.7 MB。即整条窗口 + 文本渲染栈约 5 MB。

## 15. 测试策略

### 15.1 单元测试（无窗口，因第 5 节的分层约束而可行）

| 目标 | 用例 |
|---|---|
| `render::blend_src_over` | 不透明源覆盖、透明源恒等、半透明源在预乘空间的期望值、透明底取源原值 |
| `render` 预乘不变量 | 合成后任意像素 R/G/B ≤ A；alpha=0 的像素 RGB 必须为 0 |
| `render` 颜色映射 | 给定颜色与覆盖率，输出的预乘值与理论值逐通道相等 |
| `config` 解析 | 缺省填充；每个非法字段各自导致失败（表驱动，逐字段断言错误信息包含字段名） |
| `config` 锚点解析 | 九种锚点 × 显示器工作区 → 期望矩形，含负偏移与多显示器偏移 |
| `layout` 截断 | 文本高度恰好等于、超过、远小于窗口高度时的行数与 `truncated` 标志 |
| `layout` 换行 | 仅 `\n`、仅自动换行、两者混合 |
| `content` 状态机 | `text` → `time` → `blank` → `time` 的完整迁移；空文本得到 `blank` |
| `http` 解析 | 四个端点、未知路径、未知方法、超大 Content-Length、非法 UTF-8、非法 JSON 各自的状态码与 `error.code` |
| `clock` tick 推导 | 含 `%S` 与不含 `%S` 的格式各自得到 1 s / 60 s；显式 `tick_ms` 覆盖 |

### 15.2 集成测试

- 用**临时端口**（`:0` 或从系统取空闲端口）启动 HTTP 服务与一个无窗口的渲染核心，`POST /text` 后断言：命令被派发、内容状态更新、表面字节中出现预期覆盖像素。
- 断言 `/status` 在三种模式下的字段完整性。
- 断言超限载荷返回 413 且内容状态未变。

### 15.3 窗口级验证（必须在真实窗口上执行）

这些不写成自动化测试，而是一个可重复的手工验证清单，在 M1 与发布前各执行一次：

1. 窗口矩形与配置的锚点解析结果一致。
2. 窗口矩形内的 `WindowFromPoint` 返回**其他**窗口（点击穿透）。
3. `GetWindowLongPtr(GWL_EXSTYLE)` 含第 6.1 节的五个位，且不含 `WS_EX_APPWINDOW`。
4. 截屏后逐像素校验：无"三通道同时比背景更暗"的像素（无黑边），且与 source-over 模型的偏差在整数舍入内。
5. 任务栏与 Alt-Tab 中不出现 glassine。
6. 后台运行 1 小时后 Working Set 增长 < 1 MB，且 GDI 对象数不增长。
7. 修改配置的锚点后重启，窗口出现在预期位置。

## 16. 参考资料：Catime 的借鉴与拒绝

Catime（v1.6.2，纯 C + Win32，995 KB）是与本项目最接近的公开实现。逐项处置：

**借鉴**

| 项 | 理由 |
|---|---|
| 窗口样式组合 | 与其 `window_core.c:71-88` 逐位一致 |
| `CreateDIBSection` + `UpdateLayeredWindow(ULW_ALPHA)` | 实测正确的逐像素 alpha 路径 |
| 字体 mmap | `CreateFileMappingW(PAGE_READONLY)`；对 10–20 MB 的 CJK 字体尤为重要 |
| 置顶重断言 | 其在生产中加入的 `window_topmost_retry.c` |
| 单实例用命名互斥体 | `CreateMutexW` |
| 输入侧去抖与载荷上限 | 其 `NOTIFY_MIN_INTERVAL_MS 1000` / `MAX_PLUGIN_OUTPUT_BYTES`，对应本规范的 64 KiB 上限与 2 s 超时 |
| `SetLayeredWindowAttributes` 与 `UpdateLayeredWindow` 不混用 | 其代码注释中的 Error 87，本次实测证实并加重（永久失败） |

**拒绝**

| 项 | 理由 |
|---|---|
| `stb_truetype` + 手写预乘混合 | 其 `drawing_text_stb_effect.c:92-96` 用"取最大 alpha"的启发式而非 source-over；我们用 cosmic-text 取覆盖率 + 一个被单元测试覆盖的合成函数 |
| `output.txt` 文件作为输入通道 | 无回执、无类型、无长度语义 |
| INI + `GetPrivateProfile*` | 换 TOML + serde |
| 纯 C 约束带来的手工 DIB 管理 | 换单一的 `Surface` 抽象 |

## 17. 实测依据

### 17.1 Spike 范围

`glassine-spike`（969 行，Windows 层 + 渲染层 + 测量脚手架）实现了与第 6、7 节相同的窗口配方与渲染管线，用于在写本规范前验证它们。

### 17.2 已验证事实

| 结论 | 验证方法 | 结果 |
|---|---|---|
| 预乘合成正确 | 6 个单元测试 | 全通过 |
| 表面字节序与预乘 | 导出交给 `UpdateLayeredWindow` 的原始 DIB 缓冲 | 全覆盖像素 R=230=A、G=B=⌊32A/255⌋=29；0 处"alpha=0 却有颜色"；0 处"通道 > alpha" |
| 抗锯齿质量 | 同上，统计覆盖率分布 | 230 种不同覆盖率；56% 像素为部分覆盖 |
| 无黑边/灰边 | 截屏 + 逐像素解算 source-over | 0 个暗边像素；4244 个文本像素中 0 个与模型偏差超过 3（最大偏差 0.76） |
| 点击穿透 | 进程内 `WindowFromPoint`，窗口内两点 | 两点均命中下层窗口 |
| 窗口配方 | 进程内解码 `GWL_EXSTYLE` | `0x080800A8`，与 6.1 一致 |
| `SetLayeredWindowAttributes` 冲突 | 调用后再呈现 | `0x80070057`，此后 43 次连续失败 |
| `WM_PAINT` 风暴 | 省略 `BeginPaint`/`EndPaint` | 80.9% 单核持续占用 |

### 17.3 由实测导致的规范修正

| 原设计 | 修正 | 证据 |
|---|---|---|
| 用 `tiny-skia` 做合成 | **移除该依赖** | cosmic-text 交给我们的是覆盖率而非像素（`swash.rs:216`：alpha=覆盖率、RGB 直通，且基色 alpha 被丢弃），预乘无论如何要自己做；而窗口无背景/圆角/矢量，`tiny-skia` 无事可做 |
| 内存预期 25–35 MB | 下调为 9.2 MB | 见 17.2 |
| 整体不透明度用 `LWA_ALPHA` | 改为像素缓冲内的 alpha 缩放 | 见 6.3 |
| 未考虑 `WM_PAINT` | 强制验证更新区域 | 见 6.4 |

### 17.4 尚未验证（本规范中属设计而非实测的部分）

| 项 | 状态 |
|---|---|
| HTTP → `PostMessage` → 重绘链路 | **未实现、未验证**。这是 M2 的核心风险，其失败症状是"接口返回 200 但窗口不刷新" |
| `WM_DPICHANGED` 与多显示器 | 未触发（全程 96 DPI 单显示器） |
| 长时间稳定性、GDI 对象泄漏 | 仅连续运行约 70 秒 |
| 文本溢出截断 | 当前字号下两行放得下，未触发 |
| macOS 全部 | 超出本规范范围（M5）。窗口层、呈现层、activation policy、内存口径均未在本机取得任何数据 |
| 目标机器上的 WebView2/防火墙等外部因素 | 不适用（glassine 不使用 WebView2，且只绑回环） |

## 18. 里程碑

| 里程碑 | 交付 | 出口条件 |
|---|---|---|
| M1 骨架 | `config` + `content` + `layout` + `render` + `platform::win`：显示配置的时钟 | 第 14 节全部指标达标；15.3 清单通过；spike 目录删除 |
| M2 接口 | `http` + 命令投递 + `PostMessage` 唤醒 + `/text`、`/time` | `curl` 写入文本后窗口在 50 ms 内更新；15.2 集成测试通过 |
| M3 完整 | `/quit`、`/status`、单实例、日志、自启、错误码全集 | 12 项错误路径各有测试 |
| M4 发布 | 打包为单 exe（静态 CRT）、使用文档 | 干净机器上免安装运行 |
| M5 macOS | `platform::mac` | 单独规范 |

实现计划覆盖 **M1–M4**，单一计划可完成。M5（macOS）需要自己的规范：第 17.4 节列出的未验证项中，macOS 相关的部分必须先在真机上取得实测数据，才能写出同等质量的规范——这也正是本规范在写平台行为时坚持附上实测数据的原因。

## 19. 已知设计取舍（供评审推翻）

| 取舍 | 选择 | 代价 |
|---|---|---|
| 传输层用 localhost HTTP | 换取"任何语言 + `curl` 即客户端 + 可调试" | 引入端口占用与防火墙语义；命名管道在这些方面更优 |
| 不做鉴权 | 简化调用方 | 同机任意进程可改写窗口内容 |
| 不做配置热重载 | 免除文件监听线程与部分应用状态 | 改配置需重启进程 |
| 单 accept 线程、串行处理 | 消除并发状态机 | 一个卡住的客户端最多阻塞其他调用方 2 秒 |
| 文本溢出截断而非缩放 | 排版结果可预测 | 调用方需自行控制长度 |
| 不做托盘 | 少一个常驻 UI 组件 | 退出只能靠 HTTP 或终止进程 |

---

## 评审要点

请重点确认：

1. 第 4 节的非目标边界是否与你的意图一致（尤其：确认不做托盘、不做任何自动回退）。
2. 第 9 节的接口形状（四个端点、错误码、串行语义）是否够用。
3. 第 10 节的配置字段是否覆盖你调窗口与文字所需的一切。
4. 第 14 节的验收上限是否要收紧（实测 9.2 MB，我写的是 ≤ 15 MB，留了余量）。
5. 第 18 节的里程碑顺序是否认可（M2 是唯一含未知风险的里程碑）。
