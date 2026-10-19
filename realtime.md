# Loupe OCR 改造 · 实时进度（realtime）

> 最后更新：2026-10-19 16:00（Asia/Shanghai）
> 工作目录：`/home/bing/workspace/loupe`
> 分支：`ocr-playground`（基于官方 tag `48.1`）
> 验证工程：`/home/bing/workspace/ocr-verify`（独立 cargo 项目，不污染 loupe）
> 模型位置：`~/.local/share/loupe/ocr-models/{tiny_det,tiny_rec}/`（运行时按 `LOUPE_OCR_MODELS`→XDG→/usr/local→/usr 顺序查找）

## 1. 最终目标

把本机系统自带的 Loupe（`org.gnome.Loupe`，`/usr/bin/loupe`，Debian 包 `loupe 48.1-3`）**直接替换掉**，
换成我们自己加了 OCR 能力的版本：

- 在系统文件管理器（Nautilus）里双击 png/jpg 等图片，**打开即自动 OCR**；
- **可以直接在图上拖选、复制图片上的文字**（类似 QQ 截图识字 / PowerToys Text Extractor 的体验）；
- 中英文可用，离线优先（**已定：PP-OCRv6 tiny ONNX + pure-onnx-ocr 纯 Rust，不用 Tesseract/Python**）；
- 替换后：`xdg-mime query default image/png` 仍是 `org.gnome.Loupe.desktop`，
  但它指向的已经是我们的新版，系统原版不再被使用。

## 2. 为什么分两阶段

| 阶段 | 做法 | 目的 |
|------|------|------|
| **A. 完成前验证（现在）** | 用官方机制 `-Dapp_id_suffix=.Devel` 编出共存版：`org.gnome.Loupe.Devel` + `~/.local/bin/loupe`，`--prefix=$HOME/.local` | 随便改、随便崩，不影响系统默认看图，崩了还能用原版 |
| **B. 完成时替换（最终）** | 同一份代码，去掉 `.Devel` 后缀，以 `release` profile 重新编译，打包/安装到系统路径（`/usr/local` 或替换 `/usr`），把 `image/png` 等默认关联切到新版 | 正式接管本机看图入口 |

当前处于 **阶段 A**。阶段 B 在 OCR 功能稳定、验证通过后才做。

## 3. 做到哪了（进度）

- [x] 确认本机现状：GNOME + Nautilus，双击 png 默认 `org.gnome.Loupe.desktop`（Loupe/图像查看器），`~/.config/mimeapps.list` 无覆盖
- [x] 确认 Loupe 开源：GPL-3.0-or-later，源码 `https://gitlab.gnome.org/GNOME/loupe`
- [x] 搜过 OCR 插件：**官方 Loupe 无插件体系、无 OCR**，搜到的重名 `loupe` OCR 项目与本项目无关
- [x] 代码拉到 `/home/bing/workspace/loupe`（`git clone` 官方仓库）
- [x] 踩坑并解决：`main(51.0)` 要求 `libadwaita>=1.8.0`，本机 trixie 只有 `1.7.6` → 切到与系统同代的 tag `48.1`
- [x] 装好编译依赖（经 pkexec）：`meson / libgtk-4-dev / libadwaita-1-dev / libgweather-4-dev / libseccomp-dev / liblcms2-dev / itstool / yelp-tools` 等
- [x] 配置通过：`meson setup builddir --prefix=$HOME/.local -Dapp_id_suffix=.Devel -Dprofile=dev`
- [x] 编译通过（`ninja -C builddir`，约 1m16s）并安装到 `~/.local`
- [x] 共存验证通过：
  - 系统版 `/usr/bin/loupe`（6.9M）完好，默认 mime 未动
  - 共存版 `~/.local/bin/loupe`（183M dev 版）`--help` 正常
  - `org.gnome.Loupe.Devel.desktop` / `.Devel.gschema.xml` / `dbus service(org.gnome.Loupe.Devel)` 独立存在
  - 已把 Devel 桌面 `Exec` 改为绝对路径 `/home/bing/.local/bin/loupe %U`
- [x] 建工作分支 `ocr-playground`，后续改造都在此分支
- [x] **OCR 路线验证通过（2026-10-19）**：`pure-onnx-ocr 0.3.1`（纯 Rust tract 推理，零 C/C++）+ `PP-OCRv6 tiny`（det 1.7M + rec 4.3M，HuggingFace `PaddlePaddle/PP-OCRv6_tiny_{det,rec}_onnx`），Apache-2.0 与 GPL-3.0 兼容
  - 测试图：`/home/bing/下载/粘贴的图像.png`（1026x448 黑底白字小说文本， hardest case）
  - 结果：13 行全中，置信度 0.97~0.999，每行带多边形坐标（可做图上叠加选中）；首跑 0.42s（含编译），稳定态 0.21s，微信/QQ 级体验
  - 微瑕：“流淌”误为“流消”、末行缺右引号，可接受
  - 结论：pure-onnx-ocr 可用，**下一步把它嵌进 Loupe**（`src/ocr.rs` 后台线程 + `GtkOverlay` 文字层）
- [x] **Loupe 集成第一步完成（2026-10-19）**：OCR 后台骨架 + 右上角红黄绿状态灯
  - 新增 `src/ocr.rs`：进程级共享 `OcrEngine`（Mutex，只在阻塞推理 ~0.2s 内持有），`OcrStatus{Idle/Running/Done/Error}`，`OcrLine{文本/置信度/四边形坐标}`（为图上选词 overlay 预留），`OcrEvent{generation, outcome}`
  - `LpImageWindow` 每窗口独立：`ocr_generation` 计数器 + `async_channel` 主线程回传；切图即 `trigger_ocr`，关窗口 `connect_close_request→cancel_ocr` 作废在途任务（过期结果直接丢弃；tract 推理本身不可抢占，属协作式取消）；OCR 异常只亮红灯，绝不碰看图链路
  - 顶栏新增 `● OCR` 按钮：灰=无本地图，黄=识别中，绿=完成（tooltip 带行数，点击复制全文+toast），红=出错（tooltip 带完整错误信息，点击重试）
  - 样式进 `data/resources/style.css`（`.ocr-dot-{idle,running,done,error}`）
  - 改动文件：`Cargo.toml`(pure-onnx-ocr 0.3.1) / `src/lib.rs` / `src/ocr.rs`(新) / `src/widgets/image_window.{rs,ui}` / `data/resources/style.css`
  - 真机验证（共存版 `org.gnome.Loupe.Devel`）：png 图亮绿灯、txt 文件亮红灯且 Loupe 原生报错页不受影响、双窗口红绿互不干扰、点绿灯剪贴板拿到 13 行全文；另发现 Loupe 系单实例（二次启动只开新窗口），窗口级隔离依然成立
  - 待补：tooltip 肉眼确认（XWayland 下截不到悬浮窗）；黄灯一闪而过属正常（0.2s 级）
- [x] **图上拖选 overlay 第一版（2026-10-19）**：`LpImage.img_to_widget_coord`（zoom/居中/滚动精确逆变换）+ `LpImagePage` 内 `GtkOverlay/GtkFixed` 透明文字层，每行一个可选中 Label（透明字+hover 描边+选中高亮），跟随缩放/滚动/窗口变化，旋转时自动隐藏；切图即清旧框，同文件切回命中缓存免重识
  - 修过一个坑：overlay 浮层缺 `hexpand/vexpand` 会被分配 0 尺寸导致 label 收不到事件（表现为拖选变平移）
  - 注意：共存版是 dev profile（未优化），tract 首跑约 50s 黄灯，release 后 0.2s；属验证期现象
- [x] **根因定位+修复（2026-10-19）**：诊断日志证实 `layer_alloc=0x0`（GtkOverlay 只按最小尺寸分配浮层），label 无事件=拖选变平移；已加 `set_size_request(图片宽高)` + idle/500ms 延迟重排兜底；新日志 `layer_alloc=1390x459 label_alloc=107x39`，映射 1:1 正确
- [x] **删除拖出复制文件（2026-10-19）**：`LpImageView::drag_source::prepare` 改为永远 Denied；平移、滚轮缩放、触屏手势不动。注意外部拖入仍可接受（`different_source` 逻辑不受影响）
- [x] **命中问题根因+重构（2026-10-19）**：独立实验证实父容器 `can-target=false` 会让 GTK picking 跳过整棵子树——这就是 label 隐身的原因；已删掉中间 `GtkFixed` 层，13 个 label 改为 overlay 直接子件（margin 定位），空白处天然漏事件给图片（平移保留）。`pick` 诊断现显示 `is_label=true has_ocr_class=true`，坐标 1:1 命中
- [x] **用户实测通过（2026-10-19）**：行上可拖选出蓝色高亮（截图确认）→ 链路全通
- [x] **字框收缩（2026-10-19）**：按用户方案——先按行高设字，`measure` 自然宽度，超框就按比例缩小（只小不大，兜底 4px）；已重装重启
- [x] **表格竖排图分析（2026-10-19）**：`ocr-verify --release` 原生跑社保备案截图（1857x850）：149 框 / 0.54s；横排正文几乎全对（0.95~1.0）；竖排表头被拆成单字框（字多半认对但顺序散）；少量噪框（V/Z/▲/□/空串）+ 个别错字（合同类别令/业下载模板）。结论：**OCR 管线+模型侧问题，不是 Loupe 集成 bug**——缺方向分类+旋转裁剪（pure-onnx-ocr 支持，需另下方向模型）、tiny 容量上限；表格结构还原超出 OCR 范畴。按用户意见暂不优化
- [x] **交互重做（2026-10-19）**：去掉松手自动复制（太意外）。改为标准逻辑：框内小拖=原生单框选择；拖出起始框=手势 claim（顺带取消平移）+多框联动高亮；**选中只是选中，Ctrl+C 才复制**（capture 期按键拦截，有跨框才吃掉，否则走原生/窗口行为）；灯点击=拷全文不变。坐标改用手势自带 overlay 坐标，不再碰原始事件
- [x] **抢占重做（2026-10-19）**：日志证实旧手势连 begin 都收不到（label 原生先 claim）。独立实验证实不可选 label 上 select_region 无效。改为 capture 期手势、命中即 claim，全量自绘（单框亦然）；Ctrl+C 逻辑不变（span 现包含单框）。代价：文本点击无原生 caret（鼠标工具可接受）
- [x] **多框高亮改自绘（2026-10-19）**：独立实验证实 X11 PRIMARY 单主人——多 label `select_region` 只有最后一个生效。遂全部自绘：每行一段 `.ocr-hl` 半透明圆角矩形（Pango `index_to_pos` 量字符像素宽，整行直接用框），label 只当透明命中层；复制组装逻辑不动。已重装重启
- [x] **统一浅蓝（2026-10-19）**：label 设 `selectable(false)`，原生深蓝选择彻底消失，只剩自绘 `.ocr-hl` 浅蓝；残留 `selection` CSS 规则已删。已重装重启
- [x] **微信式干净（2026-10-19）**：悬停描边已删（hover 只剩文字 tooltip）；1.5s 提示闪也按要求拿掉——平时画面零框，只有拖选中才出浅蓝。已重装重启（二进制确认无残留）
- [x] **Ctrl+C 改窗口级（2026-10-19）**：原 page 级按键监听因拖后焦点不在子树里而收不到，Ctrl+C 掉到“复制图像”。改为窗口 capture 拦截：有 span 就拷文字（输入框焦点时除外），否则走原生。已重装重启
- [x] **中文翻译（2026-10-19）**：8 条 OCR 新文案全部补进 `po/zh_CN.po`（风格对齐“图像已复制到剪贴板上”），`msgfmt` 校验通过，已重装（.mo 确认含新串）重启
- [ ] **待用户亲测（等绿灯后）**：跨行拖→Ctrl+C，toast 是中文了吗？灯的悬停提示也是中文了吗？
- [ ] **待定**：是否删除图片拖动平移、只保留 OCR 拖选（用户提议；建议先验证共存效果再定，见下方）
- [ ] 原型验证通过后：完善为“打开自动 OCR + 图上拖选”，处理缩放/裁剪坐标映射、中文字库、异步不卡 UI
- [ ] 最终替换：release 编译 → 系统安装 → 默认关联切换 → 回归验证 → 备份/回滚说明

## 4. 关键信息速查

- 上游：`origin https://gitlab.gnome.org/GNOME/loupe.git`
- 基线：`48.1`（`49f3808 NEWS: Update for 48.1`），分支 `ocr-playground`
- 技术栈：Rust 97% + GTK4/libadwaita + glycin，无插件接口，只能 fork 硬改
- 官方文档注意：`CONTRIBUTING.md` 禁止向上游提交 LLM 生成的代码（本地自用不受影响）
- 常用命令：
  ```bash
  cd /home/bing/workspace/loupe
  git log --oneline -5
  meson setup builddir --prefix=$HOME/.local -Dapp_id_suffix=.Devel -Dprofile=dev  # 首次
  ninja -C builddir            # 编译
  meson install -C builddir    # 装到 ~/.local
  sed -i 's|^Exec=loupe %U|Exec=/home/bing/.local/bin/loupe %U|' ~/.local/share/applications/org.gnome.Loupe.Devel.desktop
  ~/.local/bin/loupe --help   # 冒烟测试（共存版）
  /usr/bin/loupe --help       # 系统原版对照
  xdg-mime query default image/png   # 应仍为 org.gnome.Loupe.desktop（阶段A不改默认）
  ```
- PATH 提醒：`~/.local/bin` 在 PATH 中排在 `/usr/bin` 前面，终端敲 `loupe` 现在进的是共存版；要明确用原版请写全路径 `/usr/bin/loupe`。

## 5. 风险与回滚

- 共存阶段风险极低：系统文件均未动，删共存版只需删 `~/.local/bin/loupe` + `~/.local/share/applications/org.gnome.Loupe.Devel.desktop` 等并 `update-desktop-database`。
- 最终替换阶段再做：系统包备份、`.deb` 或 `checkinstall` 留档、保留原版二进制，另写替换步骤。
