# RapidPdfTrans

尽量保住版式的 PDF 翻译库，用来替换 [BabelDOC](https://github.com/funstory-ai/BabelDOC)。硬性要求是：文字不能悄悄消失，版式尽量与原文一致。

它打开 PDF，把内容流解释到单个字形，并检查每个字形都还在。翻译器给这些字形标上译文。不加 `--output` 的 `rpt translate` 只报告译文，不写文件。加上 `--output` 会把字形重写进 PDF，并删掉已经改写的那些字形原来的文本绘制指令。

英文说明在 [README.md](README.md)。与 BabelDOC 的墙钟时间、峰值 RSS 和输出大小见 [与 BabelDOC 0.6.4 的实测](#与-babeldoc-064-的实测)。对照页见 [对照页](#对照页)。

## 用法

### 安装

需要 Rust stable（1.88 及以上，工作区锁定 `stable`）：

```bash
cargo build -p rpt-cli --release
./target/release/rpt extract testdata/hello.pdf --compact
```

`rpt extract` 把 JSON 打到标准输出，不写 PDF。`testdata/hello.pdf` 的纯文本是 `Hello`，覆盖率里 `"pending": 5`。

Python（需要 `python3-dev`）。这个模块直接调 Rust 核心，不经过 C ABI：

```bash
cargo build -p rpt-python --release
cp target/release/librapidpdftrans.so rapidpdftrans.so
python3 -c 'import rapidpdftrans; print(rapidpdftrans.version()); print("Hello" in rapidpdftrans.extract("testdata/hello.pdf"))'
```

`rapidpdftrans.translate(path)` 返回 JSON，不写文件。`rapidpdftrans.save(path, output)` 写出 PDF。

C++ 是 C ABI 上只有头文件的包装：

```bash
cargo build -p rpt-ffi --release
g++ -std=c++17 -I include -I bindings/cpp bindings/cpp/smoke.cpp \
  target/release/librapidpdftrans.a -ldl -lm -lpthread -lgcc_s -o smoke
./smoke testdata/hello.pdf
```

`rpt::Document::extract` 返回 JSON。`save` 写出 PDF；没有配置翻译器且 Google Translate 不可达时抛异常。

Go 在 `bindings/go` 里，先有上面的静态库：

```bash
go test ./...
```

```go
text, err := rapidpdftrans.Extract("testdata/hello.pdf")
```

`Translate` 返回 JSON，不写 PDF。写文件的 C 函数是 `rpt_save`。

### 环境变量

密钥只放在环境变量里。选项 JSON 中的 `api_key` 会被忽略。

| 变量 | 何时需要 | 默认 |
| --- | --- | --- |
| `RPT_LLM_API_KEY` | 使用 LLM 时必须有 | 未设置 |
| `RPT_LLM_BASE_URL` | 可选，LLM 网关 | `https://hub.mypapers.top/api/llm/v1` |
| `RPT_LLM_MODEL` | 可选，LLM 模型 | `auto` |
| `RPT_TRANSLATOR` | 可选，后端名称 | 未设置（见下文） |
| `RPT_GOOGLE_API_KEY` | Cloud Translation v2 | 未设置 |
| `RPT_GOOGLE_CREDENTIALS` | `google-v3` 服务账号 | 未设置 |
| `RPT_GOOGLE_ACCESS_TOKEN` | `google-v3` 持有者令牌 | 未设置 |
| `RPT_CJK_STYLE` | `serif`（宋体）或 `sans`（黑体） | `serif` |
| `RPT_CJK_FONT` | 可选，常规字体；优先于风格 | 所选风格的 Regular |
| `RPT_CJK_BOLD_FONT` | 可选，粗体；优先于风格 | 所选风格的 Bold |
| `RPT_CJK_SANS` | 可选，只替换常规无衬线源字体 | 常规字体 |
| `RPT_TRANSLATE_JOBS` | 可选，并行请求数 | `4`（上限 `8`） |
| `RPT_MAX_TRANSLATE_SECS` | 可选，单个文档所有模型调用的总时长预算（秒） | 不限 |
| `RPT_GATEWAY_DOWN_AFTER` | 连续被拒（401/403/410/400 Access denied）多少次后不再发请求 | `6` |
| `RPT_CJK_SIZE_SCALE` | 可选 | `0.90` |
| `RPT_CJK_LEADING` | 可选 | `1.60` |

`RPT_TRANSLATOR` 可以是 `maclaw`、`openai`、`google`、`google-v2`、`google-v3` 或 `google-unofficial`。它未设置，且没有 `RPT_LLM_API_KEY`、`RPT_LLM_BASE_URL`、`RPT_LLM_MODEL`、`model` 或 `base_url` 时，顺序是：`RPT_GOOGLE_API_KEY`（Cloud Translation v2），然后 v3 凭据（`RPT_GOOGLE_CREDENTIALS` 或 `RPT_GOOGLE_ACCESS_TOKEN`），然后用 3 秒探测 `https://translate.googleapis.com/translate_a/single?client=gtx`。探测成功就用这个端点。探测失败则停止，并提示设置 `RPT_LLM_API_KEY` 或 `RPT_TRANSLATOR`。显式的翻译器名称或 LLM 设置会跳过探测。

### 字体

默认中文是宋体：思源宋体 / Noto Serif CJK SC。正文用 Regular，标题、章节名、「摘要」和其它粗体源字体用 Bold。黑体（思源黑体 / Noto Sans CJK SC）是可选项。

`--cjk-style serif` 是默认。`--cjk-style sans` 选择黑体。同一个值也可以写成环境变量 `RPT_CJK_STYLE`，以及 Python 和 C 选项 JSON 里的 `cjk_style`。只接受 `sans` 和 `serif`。

`--cjk-font` 替换常规字体，`--cjk-bold-font` 替换粗体。这两个路径都优先于 `--cjk-style`。设置了 `--cjk-sans` 时，它仍只替换常规无衬线源字体，并且对这种源字体优先于 `--cjk-font`。三个路径都接受 `.ttf`、`.otf` 或 `.ttc`。路径末尾加 `#N` 选择字体集的第 N 个面（常见的 `msyh.ttc#0` 是微软雅黑）。同样的字符串也可以写成环境变量 `RPT_CJK_FONT`、`RPT_CJK_BOLD_FONT` 和 `RPT_CJK_SANS`。Python 的 `save` / `translate` 和 C 的 `rpt_save` / `rpt_translate` 在选项 JSON 里用 `cjk_font`、`cjk_bold_font`、`cjk_sans` 和 `cjk_style`（`cjk_bold` 与 `cjk_bold_font` 相同）。

只嵌入这一页用到的字形。用户字体没有的字改用当前风格的内置字体（同一字重），并给出警告：

```text
rpt: warning: CJK font /path/to/face.ttf has no glyph for U+4E2D; those characters use Noto Serif CJK
```

思源宋体和思源黑体是 SIL Open Font License 1.1。字体字节不放进 wheel，也不放进 SDK。本机已经安装对应家族（`fonts-noto-cjk`，或下面列出的路径）时用那份文件。没有安装时，第一次使用会下载所请求家族的简体 Regular 和 Bold。默认先下载思源宋体（Serif 2.003，大约 24 MB 和 25 MB）。`--cjk-style sans` 再下载思源黑体（Sans 2.004，大约 16 MB 和 17 MB）。缓存目录依次是 `RPT_FONT_CACHE`、Windows 的 `%LOCALAPPDATA%\rapidpdftrans`、macOS 的 `~/Library/Caches/rapidpdftrans`，以及 `$XDG_CACHE_HOME/rapidpdftrans` 或 `~/.cache/rapidpdftrans`。旁边会写上 `OFL.txt`。子集在 Rust 里完成，不需要 Python。正文用 Regular，粗体用 Bold。Debian 字体集里的简体中文是第 2 个面。

`--cjk-font` 未设置时，宋体正文用 `/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc`（第 2 个面），或 `/usr/share/fonts/truetype/noto` 下的同名文件，再否则用缓存里的思源宋体 Regular。粗体用 `NotoSerifCJK-Bold.ttc`（第 2 个面），再否则用缓存里的 Bold。`--cjk-style sans` 时，正文用 `NotoSansCJK-Regular.ttc`（第 2 个面），粗体用 `NotoSansCJK-Bold.ttc`（第 2 个面），再否则用缓存里的黑体文件。这些字体无法子集时，回退顺序是已设置的 `RPT_CJK_FONT`，然后 `/usr/share/fonts/opentype/noto` 或 `/usr/share/fonts/truetype/noto` 下的 `NotoSerifCJK-Regular.ttc`，再然后同目录的 `NotoSansCJK-Regular.ttc`，以及 `/usr/share/fonts/truetype/wqy/wqy-microhei.ttc`、`/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf`。

### 翻译

```bash
export RPT_LLM_API_KEY=...
./target/release/rpt translate paper.pdf --from en --to zh --output paper.zh.pdf
```

`rpt translate` 上实际存在的参数：

| 参数 | 含义 |
| --- | --- |
| `--output` | 写出译文 PDF。不加就不写 PDF。 |
| `--from`、`--to` | 源语言和目标语言。默认 `en` 和 `zh`。 |
| `--mode` | `replace`（默认）、`bilingual`、`side-by-side`、`alternating` 或 `overlay`。 |
| `--bilingual` | 双语输出。版式由 `--layout` 决定。 |
| `--layout` | `side-by-side`（默认）、`alternating` 或 `overlay`。配合 `--bilingual` 或 `--mode bilingual`。 |
| `--bilingual-output` | 用同一次翻译再写一份双语 PDF。 |
| `--max-pages N` | 只处理前 N 页。 |
| `--model`、`--base-url` | 覆盖这一次的 `RPT_LLM_MODEL` 和 `RPT_LLM_BASE_URL`。 |
| `--glossary source=target` | 可重复。也可以是 CSV 文件（`src,tgt` 或 `source,target`，表头可省略）。调用前在本地替换。 |
| `--system-prompt TEXT` | 追加到系统提示词。已存在的文件路径会读取文件；`@路径` 一定按文件读。 |
| `--timeout-secs N` | 单次请求的硬截止时间，覆盖 DNS、连接、TLS 和响应体。默认 `60` 秒。卡住的调用会被放弃并按退避重试，最多四次。 |
| `--max-translate-secs N` | 整个文档模型调用的总时长预算。用完后其余段落保留原文，退出码 2。默认读 `RPT_MAX_TRANSLATE_SECS`，否则不限。 |
| `--gateway-down-after N` | 连续 N 次被拒（401、403、410、400 access denied）后停止发送。默认 6。 |
| `--progress text` | 进度写到标准错误：`text`（默认）、`json`（每行一个对象）或 `off`。每行带阶段（`connect`、`recv-body`、`retry`）和段落。`waiting Ns` 是停住。`watchdog: aborting segment …` 点名已经超过截止时间的那一批。重写阶段每五秒打一行 `rewriting`，写完打 `wrote`。 |
| `--translate-references` | 翻译参考文献。默认保留原文。 |
| `--translate-figures` | 翻译图内文字。默认只翻译图注。 |
| `--translate-tables` | 连单元格一起翻译。默认翻译表注和列表头，单元格保留原文。 |
| `--cjk-style` | `serif`（默认，宋体）或 `sans`（黑体）。也可写成 `RPT_CJK_STYLE` 和 JSON `cjk_style`。 |
| `--cjk-font`、`--cjk-bold-font`、`--cjk-sans` | TTF、OTF 或 TTC。这些路径优先于 `--cjk-style`。`#N` 选择字体集里的面。 |
| `--cjk-size-scale`、`--cjk-leading` | 默认 `0.90` 和 `1.60`。 |
| `--batch-size` | 每次请求的段落数。默认 `8`。 |
| `--jobs` | 并行请求数。默认 `4`，或 `RPT_TRANSLATE_JOBS`，上限 `8`。排版仍是单线程。 |
| `--replay file.json` | 复用以前的翻译 JSON，不再请求模型。 |
| `--compact` | 标准输出上一行 JSON。 |
| `--json` | 为兼容而保留。标准输出始终是摘要。 |
| `--glyphs` | 标准输出改为完整的逐字形 JSON，而不是摘要。 |
| `--detail FILE` | 把完整的逐字形 JSON 写到文件。除非同时加 `--glyphs`，标准输出仍是摘要。 |

`--json` 不指定文件。标准输出是摘要：页面信息、`glyph_count`、覆盖率计数、`translations` 数组，以及 `translator` 对象（`model`、`backend`、`base_url`、`calls`、`cache_hits`）。逐字形文档用 `--glyphs` 或 `--detail`。

进度在标准错误，不进这份 JSON。只有真正漏译（正文仍是英文、溢出、缺字形、缺字体、列表不完整）才算覆盖率未完成。参考文献、图、表、代码、公式和人名单独计数，不会让本次失败。作者姓名保持拉丁字母。模型原样返回的标题会再请求一次，多出来的副标题不会画上去。

### 示例输出

`testdata/hello.pdf` 抽出的纯文本是 `Hello`。这时还没有翻译，覆盖率不算完成：

```json
{"complete": false, "pending": 5, "total": 5}
```

没有 LLM 设置且 Google Translate 不可达时，翻译以状态 1 退出，并且不写文件：

```text
rpt: translation failed: no translator is configured and Google Translate is unreachable; set RPT_LLM_API_KEY or RPT_TRANSLATOR
```

显式设置 `RPT_TRANSLATOR=maclaw` 但没有密钥时，仍会报告 `RPT_LLM_API_KEY` 未设置。

### 排错

**小内存主机与不稳定网关（≤ 8 GiB）。** 同时只跑一到两个文档，用 `--jobs 2`，并把 `--max-translate-secs` 设得明显低于外部看门狗（3 小时强杀时用 `--max-translate-secs 7200`）：一篇 97 页、21 MB 的论文在翻译之后排版与重写约需 15 分钟，RSS 峰值约 1.1 GiB（翻译阶段约 100 MiB）。预算用完后剩下的段落保留原文，PDF 照常写出，退出码 2。网关以 401/403/410 或 `400 Access denied` 拒绝时，连续 `--gateway-down-after` 次后停止发送。最后一行 stderr（`rpt: summary …`）在 `--progress off` 时也会输出，供服务记录日志。

```bash
rpt translate paper.pdf --output paper.zh.pdf --progress off --translate-tables \
  --timeout-secs 105 --jobs 2 --max-translate-secs 7200
```

**没有翻译器。** 什么都没配置时会探测 Google Translate。要改用 LLM 网关，在 `rpt translate`、Python 的 `save` / `translate`、C++ 的 `save` 或 `rpt_save` 之前导出 `RPT_LLM_API_KEY`。写在选项 JSON 里的密钥会被忽略。

**豆腐块（空白方框）。** 自定义字体没有某个字的轮廓时，如果当前字体族有这个字，不会画成空白方框。程序打印 `rpt: warning: CJK font … has no glyph for U+…`，并从思源宋体取这个字；`--cjk-style sans` 时从思源黑体取。空白方框表示回退字体族也没有这个字。把包含该字的 `.ttf`、`.otf` 或 `.ttc` 传给 `--cjk-font`。`#N` 用来选择字体集里的一个面。路径不是文件时，在发出请求之前就失败：`rpt: CJK font not found: ...`。无法识别的 `cjk_style` 同样会失败：`unknown cjk_style …; use sans or serif`。

```bash
./target/release/rpt translate paper.pdf --from en --to zh \
  --cjk-style sans \
  --output paper.zh.pdf
```

```bash
./target/release/rpt translate paper.pdf --from en --to zh \
  --cjk-font /path/to/msyh.ttc#0 \
  --cjk-bold-font /path/to/msyhbd.ttc#0 \
  --output paper.zh.pdf
```

## 目录

| 路径 | 作用 |
| --- | --- |
| `crates/rpt-core` | 内容流解释器、字体解码、字形守恒、翻译器 |
| `crates/rpt-ffi` | C ABI（`rpt_open`、`rpt_extract`、`rpt_translate`、`rpt_save`、`rpt_free`） |
| `crates/rpt-cli` | `rpt extract` 与 `rpt translate` |
| `crates/rpt-qa` | 语料保真报告（`rpt-qa`） |
| `crates/rpt-python` | PyO3 原生模块，不经过 C ABI |
| `include/rapidpdftrans.h` | cbindgen 生成的 C 头文件 |
| `bindings/cpp` | 只有头文件的 RAII 包装 |
| `bindings/go` | 通过 cgo 链接静态库 |
| `testdata/hello.pdf` | 各语言绑定的冒烟样例 |

坐标是 PDF 用户空间：原点在页面左下角，y 向上（`pdf-user-space-origin-bottom-left-y-up`）。页面 `/Rotate` 记在页信息里，不写进字形矩阵。`q` / `Q` 保存图形状态和文本状态（`Tc` `Tw` `Tz` `TL` `Ts` `Tf` `Tr`），按规范不保存文本矩阵。

## 构建

需要 Rust stable（1.88 及以上，工作区锁定 `stable`）：

```bash
cargo test --workspace
cargo build -p rpt-cli --release
./target/release/rpt extract testdata/hello.pdf --json
```

`rpt extract` 把逐字形 JSON、纯文本和覆盖率报告打到标准输出。不加 `--output` 的 `rpt translate` 翻译这些文字，不写 PDF。加上 `--output` 才会写出译文 PDF。

Python（需要 `python3-dev`）：

```bash
cargo build -p rpt-python --release
cp target/release/librapidpdftrans.so rapidpdftrans.so
python3 -c 'import rapidpdftrans; print(rapidpdftrans.extract("testdata/hello.pdf")[:200])'
```

C ABI（Go 和 C++ 使用）：

```bash
cargo build -p rpt-ffi --release
g++ -std=c++17 -I include -I bindings/cpp bindings/cpp/smoke.cpp \
  target/release/librapidpdftrans.a -ldl -lm -lpthread -lgcc_s -o smoke
```

同一次 `rpt-ffi` 构建还会生成 C 共享库：`target/release/librapidpdftrans.so`（macOS 为 `.dylib`，Windows 为 `rapidpdftrans.dll`）。需要在 rustc 参数里加上 `--crate-type cdylib`。`rpt-python` 会写出同名文件，所以先构建并复制其中一个，再构建另一个。

Go（先打好 release 静态库，再在 `bindings/go` 里）：

```bash
go test ./...
```

`rpt_save` 会翻译已打开的文档并写出 PDF。没有配置翻译器且 Google Translate 不可达时返回 -1，并且不会创建文件。

## 预编译包

`.github/workflows/sdk.yml` 为每个平台打包头文件、共享库和静态库，并带上 CMake 配置和 Unix pkg-config。`.github/workflows/wheels.yml` 用 cibuildwheel 构建 CPython 3.10+ 的 abi3 wheel（每个平台一个 wheel，可在 3.10 及更新的 CPython 上加载）。两个工作流在 pull request、`workflow_dispatch` 和 `v*` tag 上运行。pull request 只上传构建产物。tag 名必须与 `Cargo.toml` 里的 `workspace.package.version` 一致（版本 `0.1.7` 对应 tag `v0.1.7`），才会把压缩包、wheel 和 sdist 挂到 GitHub Release，并把 wheel 和 sdist 发布到 PyPI。

`.github/workflows/release-cli.yml` 在 `main`、`v*` tag 和 `workflow_dispatch` 上构建 Linux x86_64 与 aarch64 的 `rpt`（glibc 和 musl）。与 `workspace.package.version` 同名的 tag 会把这些压缩包挂到 GitHub Release。推送到 `main` 只上传构建产物。

不构建 32 位 macOS：没有对应的托管 runner，当前 CPython 也不提供 32 位 macOS 安装包。musllinux wheel 只覆盖 x86_64 和 aarch64，因为 Rust 没有 `i686-unknown-linux-musl` 或 `armv7-unknown-linux-musleabihf` 的宿主编译器，而 cibuildwheel 在目标容器里编译。Intel macOS 使用 `macos-15-intel`；GitHub 已不再提供 `macos-13`。PyPI 上传使用仓库 secret `PYPI_APIKEY`（PyPI API token），并设置 `skip-existing`。不使用 trusted publishing。

## 翻译器

默认后端是 OpenAI 兼容的对话接口：

- 基址 `https://hub.mypapers.top/api/llm/v1`（可用 `RPT_LLM_BASE_URL` 或 `base_url` 覆盖）
- 模型 `auto`（可用 `RPT_LLM_MODEL` 或 `model` 覆盖）
- 密钥只从环境变量 `RPT_LLM_API_KEY` 读取

选项 JSON 里的 `api_key` 会被忽略。优先级是：显式选项、环境变量、内置默认值。普通测试走进程内的假 HTTP 服务。只有设置了 `RPT_LLM_API_KEY` 时才会打真实接口。

`RPT_TRANSLATOR` 选择后端：`maclaw`、`openai`、`google` / `google-v2`（`RPT_GOOGLE_API_KEY`）、`google-v3`（`RPT_GOOGLE_CREDENTIALS` 服务账号或 `RPT_GOOGLE_ACCESS_TOKEN`），以及 `google-unofficial`（`translate.googleapis.com/translate_a/single?client=gtx`）。显式配置优先于已配置的 LLM，LLM 优先于 Google。什么都没配置时，3 秒探测成功就会选用非官方端点。占位符 `⟦N⟧` 在这个端点上以 `XPHNX` 发送，返回后再还原。请求会分批，同时最多 4 个，HTTP 429/5xx 会退避重试。

```bash
export RPT_LLM_API_KEY=...
./target/release/rpt translate paper.pdf --from en --to zh --output paper.zh.pdf
```

设置了 `RPT_LLM_API_KEY` 时，这条命令请求 `RPT_LLM_BASE_URL`（未设置时是 `https://hub.mypapers.top/api/llm/v1`），模型是 `RPT_LLM_MODEL`（未设置时是 `auto`）。密钥只有 `RPT_LLM_API_KEY`。没有 LLM 设置、也没有 `RPT_TRANSLATOR` 时，探测成功就改用 Google Translate。`--glossary transformer=Transformer` 是可选项，在调用前本地替换。并行请求默认是 4。

中文字形从本机字体嵌入。默认是宋体，思源宋体 / Noto Serif CJK SC：正文 Regular，标题 Bold（`RPT_CJK_STYLE=serif`，或 `--cjk-style serif`）。`--cjk-style sans` 选择黑体，思源黑体 / Noto Sans CJK SC，同样是正文 Regular、标题 Bold。`RPT_CJK_FONT` 和 `RPT_CJK_BOLD_FONT` 分别替换常规和粗体，并且优先于风格。只有设置了 `RPT_CJK_SANS` 时，它才替换常规无衬线源字体。路径可以以 `#N` 结尾来选择字体集里的面，包括 `msyh.ttc#0` 这种 TrueType 字体集。嵌入的是这一页用到的字形子集（Identity-H 和 ToUnicode）。TrueType 子集是一份独立的 TTF，`maxp` 覆盖每一个轮廓。用户字体缺的字改用当前风格，并在警告里列出。思源字体是 SIL OFL 1.1，不放进 wheel 或 SDK。没有系统字体的机器会在第一次使用时把所请求家族的 SC Regular 和 Bold 下载到 `RPT_FONT_CACHE`（或平台缓存目录），默认先下载宋体，并在旁边保存 `OFL.txt`。这些字体无法子集时，回退顺序是已设置的 `RPT_CJK_FONT`，然后 `/usr/share/fonts/opentype/noto` 或 `/usr/share/fonts/truetype/noto` 下的 `NotoSerifCJK-Regular.ttc` 和 `NotoSansCJK-Regular.ttc`，再然后 `/usr/share/fonts/truetype/wqy/wqy-microhei.ttc`、`/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf`。水平前进量来自 `hmtx`，没有做 OpenType GSUB。

默认输出是纯译文：译文在原位替换原文。双语是可选项。`--mode side-by-side` 左页原文、右页译文。`--mode alternating` 先放原文页，再放译文页。`--mode overlay` 保留原文并在同一页画出译文。`--bilingual` 选择左右并排；用 `--layout alternating` 或 `--layout overlay` 改版式。只写 `--layout` 不会改变默认。C ABI 的 `rpt_save`、C++ `save` 和 Python `save` 同样默认 `output_mode` 为 `replace`；只有明确要求双语时，`bilingual_layout` 才生效。单独的 layout 字段仍是纯译文。`--max-pages N` 只处理前 N 页。放不下、或与公式等保留字形共用操作符的段落，仍留原文。

调用前会把 URL、邮箱、`{花括号}`、数字、文内引用（`[12]`、`[1-3]`、`(Smith et al., 2020)`、`Smith (2019)`）和术语表替换成 `⟦N⟧` 占位符，译完再还原。术语在本地替换（更长的优先，ASCII 按词边界），不指望模型遵守一张术语表。占位符丢失会重试一次，仍然丢失就报错。

参考文献整节不翻译。`References`、`Bibliography`、`Works Cited`、`参考文献`、`Literatur`、`Références` 这类标题，以及编号或作者-年份条目，都会进入这一节。它跨栏、跨页延续，直到下一节标题（`Appendix`、`附录`、`A. Proofs` 等）为止。这些字形记为 `kept_original`，原因是 `references`。默认 `skip_references` 为 true；在选项 JSON 里设为 false，或给 CLI 加上 `--translate-references`，才会翻译这一节。

`rpt translate --output` 会把这些字形写回。不加 `--output` 时，它们仍是 `translated_pending_rewrite`。

## 字形守恒

抽出的每个字形一开始都是 `pending`。只有每个字形都恰好有一个最终状态时，文档才算完成：

- `rewritten`：译文已经写回 PDF
- `kept_original`：保留原绘制，并给出原因
- `non_text`：判定为非文本，并给出原因

`translated_pending_rewrite` 只记下译文，在重写完成前仍算未解决。因此只做抽取时，报告里的未解决数量等于字形数量。哪条抽取路径悄悄丢掉字形，对应测试就会失败。

每条字形记录包含：页码、Unicode（可以是多个字符；映射失败时为空）、字符码字节、已知时的 GID、字体名和字号、文本渲染矩阵、近似包围盒、前进量、颜色、渲染模式、不可见和裁剪标记，以及来源位置（页面内容流、Form XObject、注释外观或 Type3 字形过程，含操作符序号和字节范围）。`rpt translate --output` 用这个位置删掉每个已改写字形原来的文本绘制操作符。

## 解释器目前覆盖的内容

- 图形状态：`q` / `Q`、`cm`、文本状态、`Tm` / `Td` / `TD` / `T*` / `'` / `"`，以及 `TJ` 字距
- 简单字体 `/Widths`，CID 的 `/W` / `/DW`；竖排用 `/W2` / `/DW2`
- Form XObject（矩阵、资源、继承、循环）和注释 `/AP` 的正常外观
- Type3 字形过程，包括里面再画出来的文字
- 内联图像；单个坏操作符不会让整页中断
- 不可见文字（`Tr` 3 或 7）会记录并打标，不会丢掉
- 矩形裁剪会标出落在外面的字形；其他路径只标 `clip_uncertain`
- Unicode 回退顺序：ToUnicode CMap，预定义 CJK CMap（用 `encoding_rs` 解码，不是 Adobe 的 CID 表），简单编码和 `/Differences`（Adobe Glyph List），最后才是内嵌字体的 cmap。映射失败的字形会保留并打标。

## 保真语料

`rpt-bench` 用掉字、占位符与公式保留、溢出、样式、非文字区域 SSIM，以及参考文献操作符是否逐字节相同来给译文 PDF 打分。`python3 corpus/bench/run.py` 把 CI 子集的恒等对照写到 `corpus/benchmarks/`。指标说明见 `corpus/benchmarks/README.md`。

### 与 BabelDOC 0.6.4 的实测

记录时间 2026-10-23T01:01:19Z。rpt 0.1.4 是提交 `e7df215`，rpt 0.1.5 是提交 `59ff1ce`。BabelDOC 是 PyPI 上的 0.6.4（`bitstring` 4.4.0；5.0.0 无法导入 `BitStream`）。网关 `https://hub.mypapers.top/api/llm/v1`，模型 `auto`，并发 4。峰值 RSS 来自 `/usr/bin/time -v` 的 maximum resident set size。覆盖率是 `rpt-bench pair --render-pages 0` 的行覆盖：源文字行的框仍然碰到输出字形。最后一列是 rpt 自己的覆盖报告。

| PDF | 引擎 | 耗时 | 峰值 RSS | 输出 | 覆盖率 | rpt 覆盖 |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| arXiv 2610.02163，11 页，285,600 字节 | rpt 0.1.4 | 627.7s | 431.7 MB | 1,120,581 | 0.8672 | 不完整，102 未解决 |
| arXiv 2610.02163，11 页，285,600 字节 | rpt 0.1.5 | 328.6s | 189.5 MB | 1,084,324 | 0.8772 | 不完整，102 处真实漏译，4,364 处有意保留 |
| arXiv 2610.02163，11 页，285,600 字节 | BabelDOC 0.6.4 | 431.2s | 2,126.2 MB | 829,495 | 0.9311 | — |
| arXiv 2610.03665，16 页，607,541 字节 | rpt 0.1.4 | 798.1s | 579.4 MB | 1,729,640 | 0.8999 | 不完整，786 未解决 |
| arXiv 2610.03665，16 页，607,541 字节 | rpt 0.1.5 | 676.2s | 203.0 MB | 1,707,061 | 0.9011 | 不完整，786 处真实漏译，17,683 处有意保留 |
| arXiv 2610.03665，16 页，607,541 字节 | BabelDOC 0.6.4 | 620.9s | 1,984.7 MB | 1,308,117 | 0.9523 | — |
| large-30p，30 页，43,418,334 字节 | rpt 0.1.4 | 96.7s | 1,656.4 MB | 44,165,179 | 1.0000 | 完整，0 未解决 |
| large-30p，30 页，43,418,334 字节 | rpt 0.1.5 | 403.6s | 267.1 MB | 44,181,722 | 1.0000 | 完整，0 处真实漏译，69,750 处有意保留 |
| large-30p，30 页，43,418,334 字节 | BabelDOC 0.6.4 | 503.2s | 2,442.4 MB | 753,874 | 1.0000 | — |

`large-30p` 由 `corpus/bench/prod_bench.py` 生成（sha256 `3ee761fab420a44b1f83c0ed603eaec4378b4c41fba38444386b6f2bc4d7dd28`）。每页是 letter，一张未压缩的 800×600 RGB 图，外加同样的 40 行 Helvetica。原来那份约 45 MB 的生产 PDF 不在这份检出里；这个文件是同一形状（先前那份是 43,414,698 字节）。两个 rpt 版本对它都是 4 次模型调用、725 次进程内缓存命中。用户态 CPU 时间分别是 86.0s（0.1.4）和 87.5s（0.1.5）。0.1.5 在 403.6s 里 CPU 占用 21%，多出来的墙钟时间是在等待。BabelDOC 写出的单语 PDF 是 753,874 字节，仍有 30 页和 30 张图；它工作目录里的中间单语文件是 44,076,339 字节。计时前已经跑过 `babeldoc --warmup`，字体和版面模型的下载不在计时里。

```bash
sudo apt-get install -y time   # 提供 /usr/bin/time
python3 -m venv .venv
.venv/bin/pip install 'BabelDOC==0.6.4' 'bitstring>=4.3,<5'
.venv/bin/babeldoc --warmup

git worktree add /tmp/rpt-014 e7df215
cargo build -p rpt-cli --release --manifest-path /tmp/rpt-014/Cargo.toml
cargo build -p rpt-cli -p rpt-qa --release --bin rpt --bin rpt-bench

export RPT_LLM_API_KEY=...
export RPT_LLM_BASE_URL=https://hub.mypapers.top/api/llm/v1
export RPT_LLM_MODEL=auto
unset RPT_TRANSLATE_CHECKPOINT RPT_CORPUS_DIR RPT_FONT_DIR RPT_CJK_FONT RPT_TRANSLATOR RPT_GOOGLE_API_KEY

python3 corpus/bench/prod_bench.py \
  --rpt-014 /tmp/rpt-014/target/release/rpt \
  --rpt-015 target/release/rpt \
  --rpt-bench target/release/rpt-bench \
  --babeldoc .venv/bin/babeldoc \
  --jobs 4 \
  --work /tmp/rpt-prod-bench
```

rpt 使用 `--jobs 4`、默认批大小 8、替换模式。BabelDOC 使用 `--qps 4 --pool-max-workers 4 --ignore-cache --no-dual --no-auto-extract-glossary --watermark-output-mode no_watermark --primary-font-family serif`。脚本把 `table.md` 和 `results.jsonl` 写到 `--work`。密钥只留在环境变量里。

### 同一份 30 页文件：中文与左右对照

把同一份已保存的译文分别送进两个二进制（`--replay`），所以计时只含重写。源文件 `large-30p` 是 43,418,334 字节。峰值 RSS 来自 `/usr/bin/time -v`。

| 模式 | 引擎 | 耗时 | 峰值 RSS | PDF | 标准输出 |
| --- | --- | ---: | ---: | ---: | ---: |
| 替换（中文） | rpt 0.1.4 | 88.5s | 1,659.4 MB | 44,181,722 | 239,123,335 |
| 替换（中文） | rpt 0.1.5 | 83.4s | 262.2 MB | 44,181,722 | 2,105,440 |
| 左右对照 | rpt 0.1.4 | 91.4s | 1,703.3 MB | 88,550,395 | 239,123,335 |
| 左右对照 | rpt 0.1.5 | 86.9s | 263.4 MB | 44,392,754 | 2,105,440 |

左右对照从 88.6 MB、峰值 1,703 MB 降到 44.4 MB、峰值 263 MB。中文 PDF 两个版本都是 44,181,722 字节。标准输出从 239 MB 的逐字形 JSON 变成 2.1 MB 的摘要。

```bash
# 0.1.5。0.1.4 去掉 --progress 即可。
/usr/bin/time -v -- target/release/rpt translate large-30p.pdf \
  --replay translations.json --from en --to zh --jobs 4 --progress off \
  --mode side-by-side --output large-bi.pdf
```

### 对照页

网关和模型与上面的实测相同（`https://hub.mypapers.top/api/llm/v1`，模型 `auto`，并发 4）。每一张图从左到右是原文、rpt 0.1.5、BabelDOC 0.6.4，大约 110 dpi，由 `corpus/bench/compare_pages.py` 生成。先跑 pngquant；仍然超过 300 KB 的改成 WebP。下面每个文件都小于 300 KB。

两篇论文的图就是上面那次实测写出的 PDF。书是同两个引擎对第 1–35 页另计的一次。峰值 RSS 来自 `/usr/bin/time -v`。书的两份输出仍是完整的 1016 页：只翻译了第 1–35 页，第 36–1016 页还是原来的英文。rpt 的进度行一直停在 2/35，但后面的页已经译了；写出的 PDF 里中文一直到第 35 页。这次 rpt 报告覆盖不完整：漏了 225 个字形，19,379 个字形是有意保留。

| 文档 | 引擎 | 耗时 | 峰值 RSS | 输出 |
| --- | --- | ---: | ---: | ---: |
| Open Logic Text，1016 页中的第 1–35 页，源文件 4,063,129 字节 | rpt 0.1.5 | 793.1s | 230.9 MB | 6,397,502 |
| Open Logic Text，1016 页中的第 1–35 页，源文件 4,063,129 字节 | BabelDOC 0.6.4 | 902.5s | 2,241.0 MB | 6,762,851 |
| arXiv 2610.02163，11 页，285,600 字节 | rpt 0.1.5 | 328.6s | 189.5 MB | 1,084,324 |
| arXiv 2610.02163，11 页，285,600 字节 | BabelDOC 0.6.4 | 431.2s | 2,126.2 MB | 829,495 |
| arXiv 2610.03665，16 页，607,541 字节 | rpt 0.1.5 | 676.2s | 203.0 MB | 1,707,061 |
| arXiv 2610.03665，16 页，607,541 字节 | BabelDOC 0.6.4 | 620.9s | 1,984.7 MB | 1,308,117 |

这两篇论文的 0.1.4 一行在上面的实测表里，不是这组图用的 PDF。

**图中各页的许可证。** The Open Logic Text 完整构建，修订 9620cc7（2026-07-12），CC BY 4.0。PDF 第 1 页印着 “Creative Commons Attribution 4.0 International.” 来源 `https://builds.openlogicproject.org/open-logic-complete.pdf`，sha256 `e17ac6f54723999b727b56b60f864a969f2b7c883be5ce7149f13472a0ee14ae`。arXiv:2610.02163（*AutoCompact*）和 arXiv:2610.03665（*Pivot-SD*）都是 CC BY 4.0。摘要页（`https://arxiv.org/abs/2610.02163`、`https://arxiv.org/abs/2610.03665`）上的 CC BY 4.0 徽章指向 `http://creativecommons.org/licenses/by/4.0/`。两份 PDF 正文都没有印这条许可。2610.03665 第 16 页上的 MIT 和 Apache-2.0 是所引模型的许可，不是这篇论文的。论文文件是 `corpus/ci/arxiv-2610.02163.pdf` 和 `corpus/ci/arxiv-2610.03665.pdf`。

**Open Logic 第 2 页（关于本书）。** 正文。两边都译了段落，并保住了换行。

![Open Logic 第 2 页：原文、rpt 0.1.5、BabelDOC 0.6.4](docs/compare/open-logic-p02.png)

**Open Logic 第 3 页（目录）。** rpt 在大多数引导线旁放上了中文标题。第 1.5 节仍是英文（`Pairs, Tuples, Cartesian Products`）。BabelDOC 在这一页更差：好几个标题叠在同一行，页码粘在标题上（`集合25`），许多行只剩下点线。

![Open Logic 第 3 页：原文、rpt 0.1.5、BabelDOC 0.6.4](docs/compare/open-logic-p03.png)

**Open Logic 第 33 页（有序对和一则证明）。** 两边都把 Definition 1.23 留成英文，旁边是 `⟨a, b⟩ = {{a}, {a, b}}`，也都将递归式拆成 `A1` / `= A` 和 `Ak+1 Ak` / `= × A`。rpt 译了后面的段落和证明的开头；嵌套元组的括号丢了（`⟨x, y, z⟩` 被拆开），例 1.25 里原来放集合括号的地方是空白。BabelDOC 把 Definition 1.23 后面那段和证明留成英文，并把 `A`、`A × B` 和花括号从中文句子里拽出来，元组那一行同样是断的。BabelDOC 保住了更多原来的公式墨迹。rpt 译了更多散文，也把它们弄坏了。

![Open Logic 第 33 页：原文、rpt 0.1.5、BabelDOC 0.6.4](docs/compare/open-logic-p33.png)

**arXiv 2610.02163 第 1 页（标题和摘要）。** rpt 标题仍是英文，作者姓名和大学名称保持拉丁字母，摘要译成了中文，包括 9.2% / SWE-PolyBench 那句。BabelDOC 把标题译了，这是 rpt 没做的；但摘要正文没了：`摘要` 后面就是 arXiv 邮戳，这一页找不到 `9.2`。单位被挤进作者行（`Dong31新加坡管理大学`）。

![arXiv 2610.02163 第 1 页：原文、rpt 0.1.5、BabelDOC 0.6.4](docs/compare/arxiv-2610.02163-p01.webp)

**arXiv 2610.02163 第 6 页（表和图）。** rpt 把表 1 和图上的标签留成英文（默认不译表），标题和下面的段落译了，方法名还在。BabelDOC 译了单元格，表更好读；同时留下英文表题，把金额标签拆成 `4.00美` / `元`，并把下面段落里的名字弄丢了（`SWE-Compressor` 和 `AutoCompact` 变成空档）。

![arXiv 2610.02163 第 6 页：原文、rpt 0.1.5、BabelDOC 0.6.4](docs/compare/arxiv-2610.02163-p06.webp)

**arXiv 2610.02163 第 8 页（摘要卡片）。** rpt 两张卡片仍是英文，图题和正文译了。BabelDOC 译了卡片。下面那张是坏的：`Recorded state` 和 `Proposed next action` 仍是英文，一致性那一行写成 `C一致`，结尾是 `ion.`。

![arXiv 2610.02163 第 8 页：原文、rpt 0.1.5、BabelDOC 0.6.4](docs/compare/arxiv-2610.02163-p08.webp)

**arXiv 2610.03665 第 1 页（双栏）。** 两边都把两栏混在一起。rpt 译了标题和摘要，但引言穿进了摘要栏，作者上标缠在一起（`Chen-Hao 1,2,*‡ Chao`）。BabelDOC 译了标题，作者块更整齐，摘要仍是英文，并把中文引言画在这段英文摘要上。作者块 BabelDOC 更好。摘要真正译出来的只有 rpt。

![arXiv 2610.03665 第 1 页：原文、rpt 0.1.5、BabelDOC 0.6.4](docs/compare/arxiv-2610.03665-p01.webp)

**arXiv 2610.03665 第 5 页（公式和算法 1）。** 展示公式 (9)–(12) 两边都还在。BabelDOC 的 `g(t)` 出现四次，和原文一致；rpt 的文本里只有一次。BabelDOC 留下一整段英文和几行算法（`Select the top-K`、`foreach`）。rpt 译了更多散文和算法，算法步骤插在段落中间。公式块 BabelDOC 更好。rpt 覆盖的词更多，顺序乱了。

![arXiv 2610.03665 第 5 页：原文、rpt 0.1.5、BabelDOC 0.6.4](docs/compare/arxiv-2610.03665-p05.webp)

**arXiv 2610.03665 第 6 页（两张表）。** BabelDOC 译了表 1 的标题和行标签，也译了表 2 的各行，但表 2 的英文标题还在。rpt 两张表都留成英文，后面一节也有一块英文（从 `learn from failed trajectories` 到训练数据那段）。这一页 BabelDOC 更好。

![arXiv 2610.03665 第 6 页：原文、rpt 0.1.5、BabelDOC 0.6.4](docs/compare/arxiv-2610.03665-p06.webp)

```bash
# 书。论文的数字和 PDF 来自上面的 corpus/bench/prod_bench.py。
# 需要 /usr/bin/time、pymupdf、pngquant 和 cwebp。密钥只留在环境变量里。
/usr/bin/time -v -- target/release/rpt translate open-logic-complete.pdf \
  --from en --to zh --output open-logic-rpt.pdf \
  --jobs 4 --max-pages 35 --progress off \
  --base-url "$RPT_LLM_BASE_URL" --model "$RPT_LLM_MODEL"

/usr/bin/time -v -- .venv/bin/babeldoc --openai \
  --openai-model "$RPT_LLM_MODEL" \
  --openai-base-url "$RPT_LLM_BASE_URL" \
  --openai-api-key "$RPT_LLM_API_KEY" \
  --lang-in en --lang-out zh \
  --no-dual --no-auto-extract-glossary --ignore-cache \
  --watermark-output-mode no_watermark \
  --primary-font-family serif \
  --qps 4 --pool-max-workers 4 \
  --pages 1-35 \
  --output open-logic-babel --working-dir open-logic-babel/work \
  --files open-logic-complete.pdf

python3 corpus/bench/compare_pages.py \
  --source open-logic-complete.pdf \
  --rpt open-logic-rpt.pdf \
  --babel open-logic-babel/open-logic-complete.no_watermark.zh.mono.pdf \
  --pages 2,3,33 --out-dir docs/compare --prefix open-logic

python3 corpus/bench/compare_pages.py \
  --source corpus/ci/arxiv-2610.02163.pdf \
  --rpt arxiv-2610.02163-rpt.pdf \
  --babel arxiv-2610.02163.no_watermark.zh.mono.pdf \
  --pages 1,6,8 --out-dir docs/compare --prefix arxiv-2610.02163

python3 corpus/bench/compare_pages.py \
  --source corpus/ci/arxiv-2610.03665.pdf \
  --rpt arxiv-2610.03665-rpt.pdf \
  --babel arxiv-2610.03665.no_watermark.zh.mono.pdf \
  --pages 1,5,6 --out-dir docs/compare --prefix arxiv-2610.03665
```

`corpus/manifest.json` 记录真实论文和可再分发图书的直接下载 URL、来源、许可证、sha256、大小和特征标签。`python3 corpus/fetch.py` 把它们下载到被 git 忽略的缓存，这些文件不进仓库。例外是 `corpus/ci/`：九篇 CC BY 4.0 论文，每篇不到 700 KB，URL 和许可证写在 `corpus/ci/manifest.json`，CI 直接用它们。`testdata/hello.pdf` 由 `cargo run -p rpt-core --example hello_pdf` 生成，只用于单元测试，不是语料。`rpt-qa` 对每份文件做抽取和字形覆盖率检查，用 Poppler 对文字，并对一份逐字节相同的副本做渲染对比（整页 SSIM，以及去掉字形框之后的非文字区域 SSIM）。lopdf 另存是另一次结构往返，会改写文件，报告里单独列出。CI 只跑标记了 `ci: true` 的小子集。全量语料在夜间的 `Corpus fidelity` 工作流。说明见 `corpus/README.md`。

## 里程碑

1. **M1.** 内容流解释器、字形守恒、可替换翻译器、C / Python / Go / C++ 骨架、命令行。
2. **M2.** 版面分析、在现有占位符之外保护更多结构、把缓存和术语表接进版面模型。
3. **M3（已开始）。** 用 `hmtx` 前进量、中文断行和字体子集重排，精确删除原文本操作符，写出 PDF，并支持双语。`rpt_save` 会写文件。GSUB 整形和完整段落版面还没做。
4. **M4.** 更完整的版面模型、对不是真文字的页面做 OCR、用 PDFium 做渲染对比。

## 需要取舍的决定

- 非 Identity 的 CJK CMap 用字符码本身去查 `/W`，而不是 Adobe-GB1 / Japan1 的 CID。码不在表里时用 `/DW`。Identity-H/V 把数值码当作 CID。
- 没有 `/Widths` 的标准 14 字体使用 `MissingWidth`（默认 0）。没有内置 AFM，所以这类前进量在 PDF 没写宽度时是错的。
- 矩形裁剪可以标出字形。任意路径只标 `clip_uncertain`。
- Type3 会同时记下 Type3 字符本身和 char proc 里面的文字。
- 包围盒是 em 近似（下降 0.2，上升 0.8），不是油墨边界。
- 没有 ZapfDingbats 编码表。
- PDFium 不是运行时依赖。环境里没有 libpdfium，所以没有交叉校验测试。
- 翻译缓存只在一次调用内有效，键是屏蔽后的原文。
