# Oxide Stable Changelog

Stable releases are listed newest first. The release workflow uses each versioned
section as the detailed changelog attached to the corresponding GitHub Release.

## 0.1.0

### 中文

- 支持SSH、Mosh、Telnet、RDP、VNC、WSL图形转发、本地终端、SFTP、串口

### English

- Support SSH、Mosh、Telnet、RDP、VNC、WSL Graphic Forwarding、Local shell、SFTP、Serial port

## 0.1.1

### 中文

Oxide 0.1.1 新增动态背景、可叠加的视觉效果和按需安装的背景素材包，允许为系统浅色与深色模式分别配置完整外观，并重新整理首次使用引导。同时明确云同步的下载、合并与上传方向，修复 SSH 会话重连、Linux 粘贴编辑、macOS 远程桌面文件剪贴板，以及更新与便携版数据保留问题。

#### 🎨 完整亮暗外观与背景编辑

- “跟随系统外观”支持分别配置浅色和深色两套外观，每套包含应用主题、终端主题与背景。切换系统亮暗模式时，界面、现有终端和背景一起更新，无须重新连接会话。
- 背景设置将图片或视频素材、叠加特效和背景交互分区展示；可以单独使用程序生成的效果，也可以把效果叠加在图片、动图或视频上。
- 新增九点对齐，结合现有适配方式决定裁切后保留的位置；素材不透明度、模糊、阅读遮罩、作用范围和启用页面可分别调整。
- 主题预览与编辑预览同时呈现背景、透明度和裁切效果。主题编辑使用独立草稿，取消后恢复原来的外观。
- 素材卡片左上角显示文件格式，右上角保留删除按钮，并标明当前使用的素材。修复部分视频和导入图片无法显示缩略图的问题。
- 按素材和效果的实际能力显示参数，静态内容不再出现无效的播放速度等控制。修复新增滑杆点击、拖动时偏离鼠标的位置，以及切换选项后设置页滚动位置跳动的问题。

#### ✨ 程序生成效果与背景交互

- 新增轻纹理、雾光、光潮、光潮与粒子组合、流星、景深粒子和水面折光；各效果提供适用的强度、颜色、大小、速度、方向、粗糙度、光泽或密度控制，默认颜色可取自当前主题。
- 特效强度与素材不透明度分别控制，统一有无素材时的强度表现，改善特效叠加在图片、视频和动图上过淡的问题。
- 新增“输入时暂停动效”，连续输入时暂停动态素材和效果，停止输入约一秒后恢复。此选项由用户开启。
- 新增背景视差，静态图片和效果层可随鼠标轻微移动；景深粒子的不同层使用不同位移幅度，终端文字保持固定。
- 新增静态图片的缓慢运镜，支持平移、推近和拉远，可调整幅度与速度，并在往返运动时保留画面边缘覆盖。
- 新增全天候光照，按本地时间渐变背景光线与色彩，并可预览清晨、午后、傍晚和夜间。应用与终端主题仍按各自的亮暗配置切换。
- 背景切换增加平滑过渡；窗口不活跃、不可见或最小化时暂停播放，并遵循现有减少动态效果设置。

#### 🎬 原生视频播放与按需素材包

- 视频背景接入 macOS 系统媒体解码、Windows Media Foundation 和 Linux GStreamer；支持原生视频缓冲或纹理进入对应图形后端，在能力允许时减少 CPU 拷贝与重复上传。
- 保留软件解码和像素上传路径；自动模式按持续处理负载调整播放尺寸与帧率，用户手动设置的上限保持不变。硬件加速是否可用取决于设备、驱动和视频格式。
- 背景与预览复用同一媒体处理结果，按视频时间调度显示，限制提前解码和缓冲占用；将支持路径上的缩放、裁切与模糊交给图形后端处理。
- 修复 Windows 视频缓冲未提供行跨度时导致缩略图和背景不显示的问题；修复 4K 软件缩放后不透明视频的颜色与透明度处理。
- Linux 发行包补齐媒体解码组件及插件扫描器定位，使打包后的程序能够发现自带的解码支持。
- 新增插件市场“背景素材”分类与素材库“获取更多背景”入口。视频素材独立于主程序分发，可按需安装，主程序保留现有静态背景。
- 背景包安装后将声明的素材导入当前配置的素材库；便携版使用自己的数据目录。安装不改变当前主题和背景，卸载包后已导入素材继续保留，可在素材库单独删除。
- 已发布的 Oxide Backgrounds 1.0.0 提供“晨雾”和“夜流”两段原创 3840 × 2160、24 帧/秒、16 秒循环视频，适用于浅色和深色外观；需使用本次或更高的主程序版本安装。

#### 🧭 首次使用引导与界面体验

- 首次使用引导整理为五页：欢迎与须知、外观、连接与操作、AI 与命令行、开始使用。宽窗口采用分栏，窄窗口自动调整布局，内容区随窗口尺寸变化。
- 首页使用独立应用标记与标题，保留语言选择和必要说明；主题预览集中在外观页，最后一页聚焦打开终端、新建连接和导入连接。
- 步骤导航和页数居中，复用现有切换与入场动画；按钮颜色跟随当前主题，提示与说明重新整理为更直接的引导文案。
- 新用户只显示欢迎引导，不再同时显示旧版迁移介绍；完成引导前先保存状态，避免重新启动后重复出现。保存失败时保留引导并给出提示。
- 半透明面板与侧边区域新增随主题和透明度调整的柔和边缘层次，改善背景下分割线突兀的问题；背景和媒体绘制保留圆角裁切。
- 新建连接表单改用已安装插件的状态判断，避免绘制时反复探测外部程序；SSH 代理发现也减少阻塞探测，改善 Windows 打开表单和切换远程桌面页面的卡顿。
- 调整 Windows 背景与界面绘制路径，减少重复图形工作；修复依赖真实时间的弹层关闭状态，保持界面动画时钟一致。

#### ☁️ 云同步方向与首次下载

- 新增只读“查看云端”入口，允许先检查云端配置，再决定是否应用。
- 同步预览明确提供“使用云端配置”“合并本机与云端”“用本机更新云端”三种方向，并使用对应的说明和确认按钮。
- 新设备首次发现已有云端配置时，默认预览“使用云端配置”，确认后下载应用并建立同步基线，不上传本机默认设置；首次自动同步同样需要明确确认。
- 预览中的本机更改保留在计划内，取消不会提前推进已保存的同步基线。合并时重新读取本机当前设置，纳入预览期间发生的本机更改；各方向均保留未选同步范围，云端自身存在冲突时，下载前需完成选择。
- 下载完成后记录正确的云端状态与下载历史，保留现有恢复机制和同步密码修改流程。

#### 🔌 终端、SSH、SFTP 与远程桌面修复

- SSH 远端 Shell 退出后，可通过原窗格的重连按钮或 Enter 恢复会话，保留窗格身份和滚动历史；仍在使用的 SFTP、端口转发和其他消费者继续持有共享连接。
- 远程 Shell 集成失败时显示具体原因，包括 SFTP 不可用、不支持的 Shell、读写权限和安装未完成，并明确终端本身是否仍可使用，避免把可选集成问题误认为 SSH 连接失败。
- 修复 Linux 多行粘贴编辑窗口只能退格、删除却无法输入字符的问题；文字输入进入现有编辑器，编辑期间不会被转发给后台终端，确认粘贴后才发送。
- 修复 SFTP 文件拖放释放鼠标时先清理拖动状态的问题，使跨窗格拖放正确进入上传、下载或冲突处理流程。
- 修复 macOS RDP 文件剪贴板同步，连接建立或切回窗口时同步本地复制的文件，使远端菜单能直接识别可粘贴文件，并避免把远端文件重复回传。
- 修复 Linux 客户端标题栏关闭窗口时的工作区重入问题。

#### 🔎 文件管理器

- 支持浏览Wsl内的文件
- 支持读取Wsl本地终端的当前目录
- 支持从当前目录打开Wsl本地终端

#### 📦 更新与便携版

- 更新完成后先释放当前工作区更新，再执行保存历史和退出流程，修复 macOS 旧进程在更新退出时出现崩溃提示的问题。
- Windows 应用内更新统一由主程序启动更新辅助程序，等待旧进程结束后替换并启动新版，避免安装器重复启动辅助程序影响自动重启。
- 启用便携存储时，更新包仍匹配当前运行形式：macOS 应用包、Linux AppImage 和解压式便携版分别使用对应安装包。
- macOS 替换应用包时保留包内的便携标记、配置和数据目录；替换或数据恢复失败时回滚，避免更新后丢失便携数据。

#### 📌 升级说明

- 背景动效、输入时暂停、视差、运镜和全天候光照均可自行选择；需要保持静态外观时可关闭这些选项。播放性能和可用加速路径随硬件、驱动及媒体格式变化，可在自动模式之外手动设置分辨率和帧率。
- 视频素材包从插件市场按需安装，不随主程序携带。背景包导入的视频会占用当前配置的数据目录空间；卸载插件不会自动删除这些已导入的素材。
- 更新退出、安装和重启修复由新版客户端执行，从旧版首次升级时，旧客户端仍使用原有更新逻辑。
- 本版更新日志范围：[v0.1.0…v0.1.1](https://github.com/Flinglin/oxide/compare/v0.1.0...v0.1.1)。

### English

Oxide 0.1.1 adds animated backgrounds, composable visual effects, and on-demand background packs, with complete appearance profiles for the system's light and dark modes and a redesigned first-run guide. It also makes cloud download, merge, and upload directions explicit and fixes SSH session reconnection, Linux paste editing, macOS remote desktop file clipboard synchronization, and update handling for installed and portable builds.

#### 🎨 Complete light and dark appearances and background editing

- Follow System Appearance now supports separate light and dark profiles, each containing an application theme, terminal theme, and background. System appearance changes update the interface, existing terminals, and background together without reconnecting sessions.
- Background settings separate image or video media, overlay effects, and interactions. Generated effects can stand alone or be layered over images, animations, and videos.
- Added nine-point alignment to control which part of a background remains visible after cropping. Media opacity, blur, a readability overlay, background scope, and enabled pages can be configured separately.
- Theme and editor previews show the background, transparency, and crop together. Theme editing uses a separate draft; cancellation restores the previous appearance.
- Media cards show their file format at the top left, retain deletion at the top right, and identify the active item. Fixed missing thumbnails for some videos and imported images.
- Controls now reflect the selected media and effect, hiding irrelevant playback-speed controls for static content. Fixed pointer offsets in new sliders and settings scroll jumps after changing options.

#### ✨ Generated effects and background interactions

- Added subtle texture, fog glow, light tides, combined tides and particles, meteors, depth particles, and water caustics. Effects expose applicable strength, color, size, speed, direction, roughness, sheen, or density controls, with default colors derived from the current theme.
- Effect strength and media opacity are independent. Strength behaves consistently with or without media, improving overlays that were too faint on images, videos, and animations.
- Added an optional Pause Effects While Typing setting. Animated media and effects pause during continuous input and resume approximately one second after typing stops.
- Added background parallax for static images and effect layers. Depth-particle layers move by different amounts while terminal text stays fixed.
- Added slow camera motion for static images, with pan, zoom-in, and zoom-out options, adjustable amount and speed, and edge coverage throughout the repeating motion.
- Added all-day lighting that gradually changes background light and color with local time, with morning, afternoon, evening, and night previews. Application and terminal themes continue to follow their configured light and dark profiles.
- Background changes now transition smoothly. Playback pauses when the window is inactive, hidden, or minimized and follows the existing reduced-motion settings.

#### 🎬 Native video playback and on-demand background packs

- Video backgrounds use macOS system media decoding, Windows Media Foundation, and Linux GStreamer. Native video buffers or textures can enter the corresponding graphics backend to reduce CPU copies and repeated uploads where supported.
- Software decoding and pixel-upload paths remain available. Automatic mode adjusts playback dimensions and frame rate under sustained processing load while preserving manually configured limits. Hardware acceleration depends on the device, driver, and video format.
- Backgrounds and previews share media processing results, schedule presentation by video time, and bound decode-ahead work and buffering. Supported paths perform scaling, cropping, and blur in the graphics backend.
- Fixed missing Windows video thumbnails and backgrounds when decoded buffers omit their row stride. Fixed color and opacity handling for opaque videos resized at 4K through the software path.
- Linux packages now include media decoding components and locate the bundled plugin scanner so packaged applications can discover their decoder support.
- Added a Background Media marketplace category and a Get More Backgrounds entry in the media library. Videos are distributed independently as optional packs; the application retains its existing static backgrounds.
- Installing a pack imports its declared media into the current configuration's library, including the portable data directory. Installation leaves the current theme and background unchanged. Imported media remains after uninstalling the pack and can be deleted separately from the library.
- The published Oxide Backgrounds 1.0.0 pack provides two original 3840 × 2160, 24 fps, 16-second loops, Dawn Mist and Night Flow, for light and dark appearances. Installation requires this host release or a later version.

#### 🧭 First-run guide and interface experience

- Reorganized onboarding into five pages: Welcome and Notices, Appearance, Connections and Operations, AI and Command Line, and Getting Started. Wide windows use columns; narrow windows adapt the layout, and the content area follows the window size.
- The first page uses a separate application mark and title, retaining language selection and essential information. Theme preview is concentrated on Appearance, and the final page focuses on opening a terminal, creating a connection, and importing connections.
- Centered step navigation and page numbering, reused existing entry and switching animations, made button colors follow the theme, and rewrote guidance more directly.
- New users see onboarding without the old-version migration introduction. Completion is saved before closing to avoid showing the guide again after restart; a failed save keeps the guide open with a message.
- Translucent panels and sidebar regions now have softer edge definition that follows the theme and opacity, improving abrupt separators over backgrounds. Background and media rendering retain rounded clipping.
- Connection forms use installed-plugin state instead of repeatedly probing external programs during rendering. SSH agent discovery also avoids blocking probes, improving responsiveness when opening forms and switching remote desktop pages on Windows.
- Updated Windows background and interface rendering to reduce repeated graphics work. Fixed overlay dismissal states that depended on wall-clock time so they use the interface animation clock consistently.

#### ☁️ Cloud sync direction and first-device downloads

- Added a read-only View Cloud Configuration entry to inspect remote configuration before applying it.
- Sync previews explicitly offer Use Cloud Configuration, Merge Local and Cloud, and Update Cloud from This Device, with matching explanations and confirmation buttons.
- A new device discovering existing cloud configuration defaults to previewing Use Cloud Configuration. Confirmation downloads and applies it, establishes a sync baseline, and does not upload local defaults. Initial automatic sync also requires explicit confirmation.
- Local preview edits remain in the plan; cancellation does not advance the saved sync baseline. Merging re-reads current local configuration to include edits made during the preview. All directions preserve excluded scope, and downloads require choices for conflicts already present in the cloud.
- Download completion records the correct cloud state and download history, retaining existing recovery and sync-password-change flows.

#### 🔌 Terminal, SSH, SFTP, and remote desktop fixes

- After a remote SSH shell exits, its reconnect button or Enter restores the session in the original pane, preserving pane identity and scrollback. SFTP, forwarding, and other consumers retain their ownership of the shared connection.
- Remote shell integration failures now explain the cause, including unavailable SFTP, unsupported shells, read/write permissions, and incomplete installation, and clarify whether the SSH terminal remains usable.
- Fixed the Linux multiline paste editor accepting Backspace and Delete but not typed characters. Text reaches the existing editor, editing keys are not forwarded to the background terminal, and content is sent only after confirming the paste.
- Fixed SFTP mouse release clearing drag state before completing a drop, restoring upload, download, and conflict handling across panes.
- Fixed macOS RDP file clipboard synchronization. Connecting or returning to the window advertises locally copied files so the remote menu can recognize available files without a preceding paste shortcut, while avoiding echoing remote-origin files.
- Fixed workspace reentry when closing a Linux window through its client-side titlebar.

#### 🔎 File Manager

- Supports browsing files within WSL
- Supports retrieving the current directory from a local WSL terminal
- Supports opening a local WSL terminal from the current directory

#### 📦 Updates and portable builds

- Update completion releases the current workspace update before saving history and quitting, fixing the macOS old process reporting a crash during update shutdown.
- Windows in-app updates now launch the helper from the application, wait for the old process to exit, replace files, and start the new version, avoiding duplicate helper launches from the installer that interfered with automatic restart.
- Portable storage preserves the running package's update format: macOS application bundles, Linux AppImages, and extracted portable builds each select their corresponding package.
- macOS application replacement preserves bundle-local portable markers, configuration, and data directories, with rollback if replacement or data restoration fails.

#### 📌 Upgrade notes

- Background effects, typing pauses, parallax, camera motion, and all-day lighting are optional. Disable them for a static appearance. Playback performance and acceleration depend on hardware, drivers, and media formats; resolution and frame rate can be set manually instead of using automatic mode.
- Video packs are installed on demand from the marketplace and are not bundled with the application. Imported videos occupy space in the current configuration's data directory; uninstalling a plugin does not automatically delete imported media.
- Update shutdown, installation, and restart fixes are performed by the new client. The first upgrade from an older version still uses that older client's update logic.
- Full release range: [v0.1.0…v0.1.1](https://github.com/Flinglin/oxide/compare/v0.1.0...v0.1.1).

## 0.1.2

### 中文

Oxide 0.1.2 完善 SSH 提权凭据、命令历史、文件导航和离线端口转发管理，整理设置与首次使用引导，并修复云同步初始化、主机监控和 SSH 重连问题。

#### 🌐 独立联网工具与全局子 Agent 设置

- 新增 DeepSeek 搜索和网页读取工具，搜索账号独立于聊天模型配置。可在全局 AI 设置中选择已保存的官方 DeepSeek 配置，让其他聊天模型通过工具使用搜索服务。
- 搜索与网页读取结果显示来源链接，并区分认证、余额不足、限流、页面不可用及请求失败等情况。网页读取限于受支持的公开 HTTP/HTTPS 地址。
- 子 Agent 的启用状态、默认模型和并发限制统一为全局设置；所有对话共享并发上限，等待中的任务释放运行名额。关闭后不再启动新子任务，已运行任务可继续完成。
- 工具设置明确区分“允许使用工具”和“自动批准操作”。自动批准列表中未勾选的操作仍可在确认后运行；停用工具则不能运行。子 Agent 沿用父任务允许的工具范围与审批规则。

#### 🔑 SSH 提权凭据与动态验证码

- 新增 SSH 通用提权凭据范围，可复用 sudo、su 和自定义密码提示凭据；连接专属匹配优先，本地终端保持独立范围。
- 密码提示只有一个匹配凭据时，可按 Enter 填入；存在多个匹配时，先显示名称、类型和来源供选择，支持方向键、Enter、鼠标和 Esc。开始手动输入或切换终端会关闭选择器。
- 启用敏感凭据同步且同步全部连接时，SSH 通用凭据随连接同步；只同步部分连接时不改动通用范围。包含密码的加密连接导出包会保存按连接限定的副本，导入后不会自动变成全局默认凭据。
- 共享 TOTP 凭据支持在设置中编辑连接绑定，保存时应用替换关系；命令行创建和编辑连接支持按名称或 ID 选择已有 TOTP 凭据，也可清除绑定。

#### ⌨️ 命令历史与快捷命令

- 已保存的 SSH、Telnet、Mosh 和串口连接可选择在本机保存命令历史，重启后继续使用；补齐 Telnet、Mosh 及串口文本控制台的历史建议。
- 历史列表支持当前连接与全部连接范围，可暂停当前连接记录、删除单条记录或清空历史；保留天数可配置，所有已保存连接合计最多保留 10,000 条。
- 快捷命令匹配规则支持选择主机地址、连接名称或全部字段，结合通配符与逗号分隔规则定位目标。
- 简化命令历史范围切换控件，协调终端输入栏、快捷命令面板与命令发送器在背景下的表面样式。

#### 📁 文件导航与端口转发

- SFTP 新增“前往终端目录”，按同一服务器的当前终端或最近使用终端定位目录，也适用于独立 SFTP 窗口。默认快捷键为 macOS `⌘⇧G`、Windows/Linux `Ctrl+Shift+G`，支持自定义；终端目录变化不会自动移动文件浏览器。
- 本地文件管理器支持拖动收藏项排序、调整收藏侧栏宽度及双击恢复默认宽度，顺序和宽度会保存；系统目录与驱动器保持原有位置。
- 已保存的端口转发规则在连接断开时仍可查看、编辑、保存和删除。“保存并启动”可通过现有连接流程建立 SSH 连接，无须额外打开终端。
- 停止转发后保留规则并关闭自动启动，手动停止的规则不会因重连再次启动；断线恢复保留规则身份，避免旧连接清理影响替换后的运行实例。

#### 🎨 设置、技能管理与首次使用引导

- 统一设置行的名称、说明、控件对齐和窄窗口换行；路径输入、代理表单、带单位数字框和背景预览继续使用适合各自内容的布局。
- 技能管理新增名称与说明搜索、范围筛选、启用数量、可展开详情和格式、读取、同名覆盖诊断，并提供打开所在目录的入口；停用技能保留原文件。
- 重新组织 AI 提供商与上下文页面，明确当前聊天配置、搜索服务配置和高级上下文容量设置；整理工具审批与子 Agent 的作用范围说明。
- 外部 MCP 控制页分别展示服务端点、监听端口、待审批操作和客户端权限；细粒度权限按功能分组，允许使用的工具与审批模式分别设置，保留原有权限含义。
- 协调壁纸下的标签、卡片、输入控件和终端辅助面板底色，限制嵌套表面的不透明度累加，并减弱侧栏分隔边缘；文字、强调色和媒体保持各自的绘制规则。
- 继续整理五页首次使用引导：统一内容宽度与间距，把连接流程与功能介绍分栏，更多功能按需展开；AI 开关前置，背景素材区限制高度，开始使用页采用更紧凑的操作入口并移除重复快捷键。

#### 🛠️ 连接、监控与同步修复

- SSH 连接失败时只重置仍在等待连接的窗格，保留已经进入 Shell 启动阶段的重连，防止重复发起。
- 修复同一节点同时存在重连和新终端打开请求时，关闭重连窗格会丢失临时节点清理标记的问题。
- X11 转发按终端报告协商成功、缺少本地显示或授权、服务端拒绝和超时等状态，并补充 Windows 本地 X Server 的要求；远端 `DISPLAY` 仍由 SSH 服务端分配。
- 远端平台检测显式使用 POSIX Shell，并支持仅接受交互式 Shell、不接受 exec 请求的 SSH 服务端；不支持的 Shell 不再反复触发自动集成检查提示。
- 主机监控打开后立即采样，刷新时保留现有数据直到新数据到达；区分环境检测中、无法识别系统和采样失败，并提供重试入口。修复列表行提示状态互相影响的问题。
- VNC 查看器断开或失败后保持停止，等待用户明确重连，不因页面重新绘制而重复启动连接。
- 修复部分 WebDAV 服务在首次同步目录尚不存在时返回 409 导致初始化失败的问题；首次同步失败或取消后，密码仍可重新设置，已有加密数据则继续走密码更换流程。
- 修正工作区启动和生命周期定时任务的调度，改善通知、后台状态和界面更新的时序一致性。

#### 📦 构建环境

- Nix 开发环境补齐图形运行库搜索路径，以及 `libdrm` 和 Mesa，使通过 `nix develop` 后执行 `cargo run` 的程序能够找到相关运行库；此项针对开发环境，发行包无法启动的反馈仍需单独定位。
- macOS 构建通过 Xcode 查找已安装的 Metal 工具链，避免仅依赖默认工具路径；本机仍需安装 Metal Toolchain。

#### 👥 贡献者

- @m00nLi：修复 SSH 重连状态与临时节点清理问题。
- @sincerely-hello-world：修复 Nix 开发环境的图形运行库配置。

#### 📌 使用与升级说明

- 本机命令历史保存默认关闭；开启后完整命令会写入未加密的本地文件，参数中的密码或令牌也可能被记录。仅记录开启后的命令，默认保留 90 天，设为 0 可永久保留。
- 联网工具需启用工具调用并配置官方 DeepSeek 账号；搜索词和网页地址会发送给 DeepSeek，搜索及网页读取按请求单独计费。
- 图片输入能力取决于实际模型和接入端点，图片 token 估算不代替服务商账单。
- 本版更新日志范围：[v0.1.1…v0.1.2](https://github.com/Flinglin/oxide/compare/v0.1.1...v0.1.2)。

### English

Oxide 0.1.2 adds image input, tool-result images, Xiaomi MiMo, and independently configured DeepSeek web search and page reading to OxideSens. It also improves SSH privilege credentials, command history, file navigation, and offline forwarding management, reorganizes settings and onboarding, and fixes cloud-sync initialization, host monitoring, and SSH reconnection.

#### 🌐 Independent web tools and global subagent settings

- Added DeepSeek web search and page-reading tools with a search account configured separately from the chat model. Global AI settings can use a saved official DeepSeek configuration so other chat models can access the search service through tools.
- Search and page-reading results show source links and distinguish authentication errors, insufficient balance, rate limits, unavailable pages, and request failures. Page reading accepts supported public HTTP/HTTPS URLs.
- Subagent enablement, default model, and concurrency limits are now global. Conversations share the concurrency limit, and waiting tasks release their running slots. Disabling subagents prevents new subtasks while allowing running tasks to finish.
- Tool settings distinguish tool availability from automatic approval. Unchecked entries in the automatic-approval list can still run after confirmation; disabled tools cannot run. Subagents retain their parent task's allowed tools and approval rules.

#### 🔑 SSH privilege credentials and one-time passwords

- Added a common SSH privilege-credential scope for sudo, su, and custom password prompts. Connection-specific matches take priority, and local terminals retain a separate scope.
- At a password prompt, Enter fills a single matching credential. Multiple matches open a chooser showing name, type, and source, with arrow-key, Enter, mouse, and Esc support. Manual input or switching terminals closes the chooser.
- Full connection sync includes common SSH credentials when sensitive-credential sync is enabled; syncing selected connections leaves the common scope unchanged. Encrypted connection exports that include passwords carry connection-scoped copies, which do not become global defaults on import.
- Shared TOTP credentials now support editing connection bindings in settings, with replacements applied on save. CLI connection creation and editing can select an existing TOTP credential by name or ID, or remove its binding.

#### ⌨️ Command history and quick commands

- Saved SSH, Telnet, Mosh, and serial connections can optionally retain command history on this device across restarts. Restored history suggestions for Telnet, Mosh, and serial text consoles.
- History supports current-connection and all-connection scopes, pausing recording for a connection, deleting individual entries, and clearing history. Retention is configurable, with up to 10,000 entries across saved connections.
- Quick-command rules can match host addresses, connection names, or all fields, using wildcards and comma-separated patterns.
- Simplified the history scope selector and coordinated wallpaper-backed surfaces in the terminal input bar, quick-command panel, and command sender.

#### 📁 File navigation and port forwarding

- Added Go to Terminal Directory in SFTP, using the current or most recently used terminal on the same server, including detached SFTP windows. The customizable default shortcut is `⌘⇧G` on macOS and `Ctrl+Shift+G` on Windows/Linux. Terminal navigation does not automatically move the file browser.
- Local File Manager favorites can be reordered by dragging, and the favorites sidebar can be resized or reset by double-clicking its edge. Order and width are saved; system folders and drives keep their positions.
- Saved forwarding rules remain visible and editable while disconnected and can be saved or deleted offline. Save and Start uses the existing SSH connection flow without opening an extra terminal.
- Stopping a forward retains its rule and disables automatic startup. Manually stopped rules remain stopped after reconnection; interrupted forwards retain their identity, and cleanup from an old connection does not affect its replacement.

#### 🎨 Settings, skills, and onboarding

- Unified setting labels, descriptions, control alignment, and narrow-window wrapping, while retaining suitable layouts for paths, proxy forms, numeric fields with units, and background previews.
- Skill management now offers name and description search, scope filtering, enabled counts, expandable details, and diagnostics for invalid, unreadable, or overridden skills, plus an Open Containing Folder action. Disabling a skill preserves its files.
- Reorganized AI provider and context pages to distinguish the current chat configuration, search-service configuration, and advanced context-capacity settings, with clearer tool-approval and subagent scope descriptions.
- External MCP controls separate the endpoint, listening port, pending approvals, and client permissions. Fine-grained permissions are grouped by function, with tool access and approval mode configured separately and existing permission semantics preserved.
- Coordinated wallpaper-backed tabs, cards, input controls, and terminal panels, limited accumulated opacity in nested surfaces, and softened sidebar separators. Text, accents, and media retain their own rendering rules.
- Refined the five-page onboarding flow with consistent widths and spacing, side-by-side connection steps and feature introductions, and expandable additional features. AI switches appear earlier, the background gallery has a bounded height, and the final page uses compact actions without duplicate shortcuts.

#### 🛠️ Connections, monitoring, and sync fixes

- SSH connection failures now reset only panes still waiting for a connection, preserving reconnects already starting a shell and preventing duplicate attempts.
- Fixed loss of temporary-node cleanup state when a reconnect and a new terminal open share a node and the reconnect pane is closed.
- X11 forwarding reports per-terminal negotiation success, missing local display or authorization, server rejection, and timeouts. Windows guidance now explains the local X server requirement; the SSH server assigns the remote `DISPLAY`.
- Remote platform detection explicitly uses a POSIX shell and supports SSH servers that accept interactive shell requests but reject exec requests. Unsupported shells no longer trigger repeated automatic integration-check notices.
- Host monitoring samples immediately on opening and retains existing data during refresh until replacements arrive. It distinguishes environment detection, unknown systems, and sampling failures, with a retry action. Fixed tooltip state leaking between list rows.
- Disconnected or failed VNC viewers remain stopped until an explicit reconnect instead of restarting during a redraw.
- Fixed first-sync initialization against WebDAV servers that return 409 when a sync directory's ancestor is missing. Failed or canceled initial sync attempts allow password setup to be retried, while existing encrypted data continues to use the password-rotation flow.
- Corrected scheduling of workspace startup and lifecycle timers for more consistent notification, background-state, and UI updates.

#### 📦 Build environments

- The Nix development shell now exposes graphics runtime-library paths and includes `libdrm` and Mesa, allowing `cargo run` inside `nix develop` to locate the required libraries. This fixes the development environment; reported packaged-app startup failures still require separate investigation.
- macOS builds locate the installed Metal toolchain through Xcode instead of relying solely on the default tool path. Metal Toolchain must still be installed locally.

#### 👥 Contributors

- @m00nLi: SSH reconnect-state and temporary-node cleanup fixes.
- @sincerely-hello-world: Nix development-shell graphics runtime configuration.

#### 📌 Usage and upgrade notes

- Saving command history locally is off by default. When enabled, complete commands are written to an unencrypted local file, including any passwords or tokens in arguments. Recording begins when enabled; the default retention is 90 days, and 0 retains history indefinitely.
- Web tools require tool calling and an official DeepSeek account. Search queries and page URLs are sent to DeepSeek, and search and page-reading requests are billed separately.
- Image support depends on the actual model and endpoint. Estimated image tokens do not replace the provider's billing data.
- Full changelog: [v0.1.1…v0.1.2](https://github.com/Flinglin/oxide/compare/v0.1.1...v0.1.2).