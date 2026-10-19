# Loupe OCR（loupe_ocr）

GNOME 默认看图应用 [Loupe](https://gitlab.gnome.org/GNOME/loupe)（本仓库基于 tag `48.1`）的一个 OCR 功能分支：
打开图片后自动做一次整图 OCR，然后可以像微信 / QQ 截图那样，**直接在图上拖框选中文字、按 `Ctrl+C` 复制**。

上游 Loupe 本身没有插件机制，所以这个功能是以 fork + 直接改源码的方式实现的。

## 跟上游比，多了什么

- **打开即 OCR**：每个窗口打开本地图片后，后台线程自动跑一次整图识别，标题栏右侧有一盏状态灯：`● OCR`
  - 灰 = 空闲、**黄 = 识别中**、绿 = 完成（tooltip 显示行数）、红 = 失败（tooltip 显示错误，纯提示，看图不受影响）。
- **图上拖选复制（微信/QQ 手感）**：
  - 在图片上按住左键拖框，框住哪些字、就高亮哪些字（淡蓝色自绘高亮，不是系统选区）；
  - 支持**跨行、跨多个识别框**连选，松开不自动复制；
  - 按 **`Ctrl+C` 显式复制**：同行文字直接拼接、跨行用换行拼接，弹中文 toast（“已从图像复制 N 个字符”）；
  - 点一下标题栏的 `● OCR` 灯 = 复制本图全部识别文字；
  - 空闲时图上干净、无悬停框、无闪烁。
- **多窗口独立**：Loupe 是单进程多窗口，本分支每个窗口有自己的 generation 计数器 + 主线程 channel，并行开图互不干扰；关窗口或切图会丢弃过期结果（tract 中途不可抢占，是合作式取消）。
- **中文**：8 条 OCR 文案已加入 `po/zh_CN.po`（状态灯、完成/失败、复制 toast）。
- **手势取舍**：为了让拖框手势稳定，**删掉了“从窗口往文件管理器拖出图片”的拖拽**；平移、缩放、滚动、触摸、外部分享进来的拖放都保留。

## 技术栈

- **语言/框架**：Rust + GTK4 + libadwaita，构建 `meson + ninja + cargo`（跟上游一致）。
- **看图管线（上游）**：`glycin` 沙盒化解码 + GPU 加速渲染。
- **OCR 管线（本分支新增）**：[`pure-onnx-ocr 0.3.1`](https://crates.io/crates/pure-onnx-ocr)（PP-OCRv6 tiny，纯 Rust `tract` 推理，无 C/C++ 依赖）
  - `ocr-models/tiny_det/` 检测模型 ~1.7M 参数，`ocr-models/tiny_rec/` 识别模型 ~4.3M 参数；
  - 进程级共享 `OcrEngine`（`Mutex`，只在阻塞推理的 ~0.2–0.5s 内持有，release 实测，dev 模式 tract 很慢属正常）；
  - 每窗口 `generation` + `async_channel` 回主线程，过期结果丢弃。
- **图上文字层**：识别框按图片坐标→窗口坐标映射（`img_to_widget_coord`，`widget = img*zoom + rendering - adj`）生成透明 `GtkLabel` 覆盖层做字符定位（Pango `xy_to_index`），再用 `.ocr-hl` 自绘矩形高亮（X11 PRIMARY 单 owner，多 `GtkLabel` 选区只能保留最后一个，所以不能用原生选区）。
- **手势仲裁**：capture 阶段 `GtkGestureDrag` + `claim` 抢过 label 原生手势；`Ctrl+C` 是窗口级 capture 控制器（与焦点无关，有输入框聚焦时让路）。

## 改了哪些文件（相对上游 48.1）

| 文件 | 说明 |
|---|---|
| `src/ocr.rs`（新增） | OCR 后端：模型查找、全局 engine、generation 事件、`run_ocr()` |
| `src/widgets/image_window.rs` | 每窗口 OCR 状态机、状态灯、后台线程、窗口级 `Ctrl+C` |
| `src/widgets/image_window.ui` | 标题栏 `● OCR` 按钮 |
| `src/widgets/image_page.rs` / `.ui` | 图上透明文字层、拖框跨选、高亮、复制 |
| `src/widgets/image/rendering.rs` | 新增 `img_to_widget_coord()`（`widget_to_img_coord` 的精确逆变换） |
| `src/widgets/image_view.rs` | 禁掉窗口内拖出手势（保拖框），保留平移/滚动/触摸 |
| `data/resources/style.css` | 状态灯 + `.ocr-hl` 高亮样式 |
| `po/zh_CN.po` | 8 条 OCR 中文文案 |
| `ocr-models/`（新增） | PP-OCRv6 tiny 检测+识别 ONNX 模型及 yml（共 ~6MB，随仓库一起发布） |
| `realtime.md` | 开发过程纪要 |

## 模型

模型就在本仓库 `ocr-models/` 里（总量约 6MB，不用额外下载）：

```
ocr-models/
  tiny_det/inference.{onnx,yml}   # 检测 ~1.7M
  tiny_rec/inference.{onnx,yml}   # 识别 ~4.3M
```

运行时查找顺序（`src/ocr.rs::model_base_dir`）：`$LOUPE_OCR_MODELS` → `$XDG_DATA_HOME/loupe/ocr-models`（即 `~/.local/share/loupe/ocr-models`）→ `/usr/local/share/loupe/ocr-models` → `/usr/share/loupe/ocr-models`。

## 构建与安装（Debian 13 trixie / GNOME，系统里是 Loupe 48.1-3）

```bash
# 依赖（跟上游一致，另需 cargo/rustc）
sudo apt install meson ninja-build cargo rustc \
  libgtk-4-dev libadwaita-1-dev libgweather-4-dev \
  libseccomp-dev liblcms2-dev itstool gettext

# 本分支基于 48.1（main 需要 libadwaita>=1.8，系统是 1.7.6）
git checkout ocr-playground   # 或你 fork 后的对应分支

# 开发跑（dev 下 tract 很慢，首次 OCR 黄灯 ~50s 属正常，release 约 0.2–0.5s）
LOUPE_OCR_MODELS=$PWD/ocr-models meson setup builddir
ninja -C builddir
./builddir/src/loupe

# release 安装到 /usr（替换系统 loupe 的位置，模型一起装）
meson setup builddir-release --buildtype=release --prefix=/usr
ninja -C builddir-release
sudo ninja -C builddir-release install
sudo mkdir -p /usr/share/loupe/ocr-models
sudo cp -r ocr-models/tiny_det ocr-models/tiny_rec /usr/share/loupe/ocr-models/
```

卸载官方版、换成我们这个并设为默认看图：

```bash
sudo apt purge -y loupe          # 卸载官方 48.1-3
sudo ninja -C builddir-release install   # 我们的装到 /usr/bin/loupe
sudo cp -r ocr-models/tiny_det ocr-models/tiny_rec /usr/share/loupe/ocr-models/
gio mime image/png org.gnome.Loupe.desktop
gio mime image/jpeg org.gnome.Loupe.desktop
```

## 许可证

跟上游一致：`GPL-3.0-or-later`（见 `COPYING.md`）。OCR 依赖 `pure-onnx-ocr` 为 Apache-2.0，与 GPL-3.0-or-later 兼容。
