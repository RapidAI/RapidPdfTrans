# RapidPdfTrans

尽量保住版式的 PDF 翻译库，用来替换 [BabelDOC](https://github.com/funstory-ai/BabelDOC)。硬性要求是：文字不能悄悄消失，版式尽量与原文一致。

它打开 PDF，把内容流解释到单个字形，并检查每个字形都还在。翻译器给这些字形标上译文。不加 `--output` 的 `rpt translate` 只报告译文，不写文件。加上 `--output` 会把字形重写进 PDF，并删掉已经改写的那些字形原来的文本绘制指令。

英文说明在 [README.md](README.md)。

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
| `--glossary source=target` | 可重复。调用前在本地替换。 |
| `--translate-references` | 翻译参考文献。默认保留原文。 |
| `--translate-figures` | 翻译图内文字。默认只翻译图注。 |
| `--translate-tables` | 翻译单元格和表头。默认只翻译表注。 |
| `--cjk-style` | `serif`（默认，宋体）或 `sans`（黑体）。也可写成 `RPT_CJK_STYLE` 和 JSON `cjk_style`。 |
| `--cjk-font`、`--cjk-bold-font`、`--cjk-sans` | TTF、OTF 或 TTC。这些路径优先于 `--cjk-style`。`#N` 选择字体集里的面。 |
| `--cjk-size-scale`、`--cjk-leading` | 默认 `0.90` 和 `1.60`。 |
| `--batch-size` | 每次请求的段落数。默认 `8`。 |
| `--jobs` | 并行请求数。默认 `4`，或 `RPT_TRANSLATE_JOBS`，上限 `8`。排版仍是单线程。 |
| `--replay file.json` | 复用以前的翻译 JSON，不再请求模型。 |
| `--compact` | 标准输出上一行 JSON。 |
| `--json` | 为兼容而保留。JSON 总会写到标准输出。 |

`--json` 不指定文件。`--output` 成功之后，进程还会打印抽取 JSON，其中有 `translations` 数组和 `translator` 对象（`model`、`backend`、`base_url`、`calls`、`cache_hits`）。

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

**没有翻译器。** 什么都没配置时会探测 Google Translate。要改用 LLM 网关，在 `rpt translate`、Python 的 `save` / `translate`、C++ 的 `save` 或 `rpt_save` 之前导出 `RPT_LLM_API_KEY`。写在选项 JSON 里的密钥会被忽略。

**用户字体缺字。** 缺的字改用当前风格绘制（默认思源宋体，`sans` 时是思源黑体），并打印 `CJK font … has no glyph for U+…`。路径不是文件时，在发出请求之前就失败：`rpt: CJK font not found: ...`。无法识别的 `cjk_style` 同样会失败：`unknown cjk_style …; use sans or serif`。

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

`.github/workflows/sdk.yml` 为每个平台打包头文件、共享库和静态库，并带上 CMake 配置和 Unix pkg-config。`.github/workflows/wheels.yml` 用 cibuildwheel 构建 CPython 3.10+ 的 abi3 wheel（每个平台一个 wheel，可在 3.10 及更新的 CPython 上加载）。两个工作流在 pull request、`workflow_dispatch` 和 `v*` tag 上运行。pull request 只上传构建产物。tag 名必须与 `Cargo.toml` 里的 `workspace.package.version` 一致（版本 `0.1.3` 对应 tag `v0.1.3`），才会把压缩包、wheel 和 sdist 挂到 GitHub Release，并把 wheel 和 sdist 发布到 PyPI。

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
