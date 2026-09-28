# glassine

一个 Windows 桌面透明便签窗口：无边框、背景透明、始终置顶、鼠标点击穿透。内容由本机的 HTTP
接口控制——可以是当前时间，也可以是任何你发过去的文本（滚动歌词、便签、提示语）。

- **不是**常驻服务，也**不是**通用 Widget 平台。它只做一件事：在一块固定大小的透明窗口上渲染
  时间或文本。
- 单一可执行文件，无运行时依赖，不需要安装。
- 只监听 `127.0.0.1`，**从不**监听 `0.0.0.0`。

## 安装

把 `glassine.exe` 复制到任意目录，运行一次即可。首次启动会使用内置默认值（配置文件不存在也
能跑），窗口出现在主显示器顶部居中。

开机自启（可选，默认关闭）：

```powershell
glassine.exe --install-autostart      # 在启动文件夹创建快捷方式
glassine.exe --uninstall-autostart    # 删除该快捷方式
```

两者都可以重复执行：已存在时覆盖，已删除时不报错。

## 配置

路径：`%APPDATA%\glassine\config.toml`（默认程序打开它的方式：托盘菜单 → 打开配置文件）。

优先级从低到高：**内置默认值 < 配置文件 < 环境变量 < 命令行参数**。
环境变量为 `GLASSINE_CONFIG`、`GLASSINE_PORT`、`GLASSINE_LOG`；命令行参数见下文。

完整键与默认值：

```toml
[window]
anchor = "top-center"        # top/middle/bottom 与 left/center/right 组合，共 9 种
offset = [0, 64]             # 相对锚点的偏移，物理像素，屏幕坐标系（+x 向右、+y 向下）
size = [640, 220]            # 窗口大小，物理像素，两维都须 ≥ 1
monitor = "primary"          # "primary"，或索引数字（"1" = 第二块显示器），或设备名
opacity = 100                # 0-100，整幅表面的透明度
reassert_topmost_ms = 5000   # 重新置顶的间隔；0 关闭

[text]
family = "Microsoft YaHei"   # 字体族；找不到时回退到系统 sans-serif 并告警
size = 34.0                  # 物理像素
weight = 400                 # 100-900（CSS 语义）
line_height = 44.0           # 物理像素
color = "#FFFFFF"            # #RRGGBB；透明度只由 alpha 表达
alpha = 230                  # 0-255
align = "center"             # left | center | right

[clock]
format = "%H:%M:%S"          # strftime 形式
utc_offset = "local"         # "local" | "+08:00" | "-05:00"
# tick_ms = 1000             # 不设时由 format 推导（含 %S 为 1s，含 %M 为 1s，以此类推）

[server]
port = 17321                 # 0 与 >65535 非法

[log]
level = "info"               # error | warn | info | debug | trace
```

**非法值一律拒绝启动**（退出码 2，打印字段名与原因），没有静默回落默认值的路径——点穿窗口没有
交互入口，配置错误若被吸收，表现就是"窗口不见了"。

想先看结果再启动：

```powershell
glassine.exe --check-config
```

它打印将要创建的窗口矩形、目标显示器与其工作区、端口、字体解析结果，然后以退出码 0 退出，不
创建窗口、不绑定端口。

配置**只在启动时读取**。改完要重启才生效（见下文"让配置生效"）。

## 接口

四个端点，地址 `127.0.0.1:<server.port>`，无鉴权（能连上回环的进程本就已能读配置）。

```bash
# 显示文本（纯文本或 JSON 都行）
curl -X POST --data-binary "今天要交周报" http://127.0.0.1:17321/text
curl -X POST -H "Content-Type: application/json" -d '{"text":"Hello"}' http://127.0.0.1:17321/text

# 切回时钟
curl -X POST http://127.0.0.1:17321/time

# 查询当前状态
curl http://127.0.0.1:17321/status

# 退出（先写响应，再退出进程，退出码 0）
curl -X POST http://127.0.0.1:17321/quit
```

`POST /text` 的正文上限 64 KiB；超限返回 413 `payload_too_large`，窗口内容保持不变。空串正文
让窗口变成空白（`mode` 为 `blank`）。

`GET /status` 返回：

```json
{
  "ok": true,
  "mode": "text",
  "window": {"x": 120, "y": 120, "width": 640, "height": 220},
  "monitor": {"name": "\\\\.\\DISPLAY1", "dpi": 96},
  "truncated": false,
  "text_bytes": 42,
  "position_source": "config"
}
```

- `mode`：`time` | `text` | `blank`，只描述内容，不描述交互模式。
- `window`：窗口当前的物理像素矩形（可能因 DPI 或显示器变化与配置不同）。
- `truncated`：当前内容排版时是否截断过行；`time`/`blank` 模式恒为 `false`。
- `text_bytes`：当前文本的 UTF-8 字节数；`time`/`blank` 模式为 `0`。
- `position_source`：`config`（由配置锚点定位）或 `override`（拖动过）。用于回答"为什么窗口不在
  我以为的位置"。

错误码：413 `payload_too_large`、400 `invalid_json`、400 `invalid_utf8`、404 `unknown_endpoint`、
405 `method_not_allowed`、408 `timeout`。所有响应都是 JSON，错误响应形如
`{"ok": false, "error": {"code": "...", "message": "..."}}`。

调用方断开连接不影响窗口：窗口保持当前内容，进程继续运行。

## 移动窗口

托盘菜单 → **移动窗口**，然后拖动窗口；松开后位置被记住，写入
`%LOCALAPPDATA%\glassine\window_state.json`，此后启动都优先使用它（`/status` 的
`position_source` 会变成 `override`）。

要让配置里的 `anchor`/`offset` 重新接管：

```powershell
glassine.exe --reset-window-position
```

这个文件是缓存，不是配置：损坏或字段缺失时会被忽略并按配置锚点定位，只记一条告警，不会导致启
动失败。glassine 从不改写 `config.toml`。

## 让配置生效

配置只在启动时读取，生效流程是：**托盘 → 退出，然后重新启动**。

启动日志会记录配置文件的路径与其 mtime，因此"我的修改到底加载了没有"可以直接从日志确认：

```
glassine 0.1.0 start config=C:\Users\you\AppData\Roaming\glassine\config.toml mtime=2026-09-28T10:00:00Z port=17321
```

## 退出码

| 码 | 含义 |
|---|---|
| 0 | 正常退出（托盘"退出"，或 `POST /quit`） |
| 1 | 已有实例在运行 |
| 2 | 配置非法（stderr 指出字段与原因） |
| 3 | 端口被占用（stderr 给出端口号；不会静默换端口） |

三条退出路径都会先移除托盘图标，不会残留幽灵图标。

## 日志

`%LOCALAPPDATA%\glassine\logs\glassine.log`，按大小轮转（单文件 1 MiB，保留 3 个）。
记录启动配置摘要、配置路径与 mtime、位置覆盖文件是否生效、绑定端口、托盘图标注册结果、每次接口
调用的端点与结果码、窗口矩形、置顶重断言、移动模式的进入与退出、错误。

**不记录完整文本内容**（只有字节数），避免把调用方的数据写进磁盘。

## 故障排查

| 症状 | 处理 |
|---|---|
| 窗口不见了 | `glassine.exe --check-config` 看它会被放在哪；若之前拖动过，`--reset-window-position` 让配置重新接管 |
| 启动立刻退出，码 3 | 端口被别的进程占着。日志与 stderr 里有端口号，`--port` 可临时换一个 |
| 启动立刻退出，码 1 | 已经有一个实例在运行，只能有一个 |
| 启动失败，码 2 | 配置里有非法值，stderr 给出了字段名与原因 |
| 托盘图标不见了 | 日志里会有注册失败的原因。此时接口与窗口仍然工作，退出可用 `POST /quit` 或结束进程 |
| 托盘图标成了"幽灵"（鼠标划过后才消失） | 进程被强杀（没走正常退出路径）。重新启动一次即可清掉 |
