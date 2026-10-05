# RapidPdfTrans

尽量保住版式的 PDF 翻译库，用来替换 [BabelDOC](https://github.com/funstory-ai/BabelDOC)。硬性要求是：文字不能悄悄消失，版式尽量与原文一致。

当前是里程碑 1：打开 PDF，把内容流解释到单个字形，并检查每个字形都还在。翻译器可以给字形标上译文。重写 PDF 是后面的里程碑，这里不会删掉原来的文本绘制指令。

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
./target/release/rpt extract file.pdf --json
```

`rpt extract` 把逐字形 JSON、纯文本和覆盖率报告打到标准输出。`rpt translate` 翻译这些文字，仍然不写回 PDF。

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

Go（先打好 release 静态库，再在 `bindings/go` 里）：

```bash
go test ./...
```

`rpt_save` 已经留在 ABI 里，目前返回 -1，错误信息是 `PDF rewriting is not implemented (milestone M3)`。

## 翻译器

默认后端是 OpenAI 兼容的对话接口：

- 基址 `https://hub.mypapers.top/api/llm/v1`（可用 `RPT_LLM_BASE_URL` 或 `base_url` 覆盖）
- 模型 `auto`（可用 `RPT_LLM_MODEL` 或 `model` 覆盖）
- 密钥只从环境变量 `RPT_LLM_API_KEY` 读取

选项 JSON 里的 `api_key` 会被忽略。优先级是：显式选项、环境变量、内置默认值。普通测试走进程内的假 HTTP 服务。只有设置了 `RPT_LLM_API_KEY` 时才会打真实接口。

```bash
export RPT_LLM_API_KEY=...
./target/release/rpt translate paper.pdf --from en --to zh \
  --glossary transformer=Transformer
```

调用前会把 URL、邮箱、`{花括号}`、数字和术语表替换成 `⟦N⟧` 占位符，译完再还原。术语在本地替换（更长的优先，ASCII 按词边界），不指望模型遵守一张术语表。占位符丢失会重试一次，仍然丢失就报错。译过的字形标记为 `translated_pending_rewrite`。这不是最终状态，因为 PDF 还没重写。

## 字形守恒

抽出的每个字形一开始都是 `pending`。只有每个字形都恰好有一个最终状态时，文档才算完成：

- `rewritten`：译文已经写回（尚未实现）
- `kept_original`：保留原绘制，并给出原因
- `non_text`：判定为非文本，并给出原因

`translated_pending_rewrite` 只记下译文，在重写完成前仍算未解决。因此只做抽取时，报告里的未解决数量等于字形数量。哪条抽取路径悄悄丢掉字形，对应测试就会失败。

每条字形记录包含：页码、Unicode（可以是多个字符；映射失败时为空）、字符码字节、已知时的 GID、字体名和字号、文本渲染矩阵、近似包围盒、前进量、颜色、渲染模式、不可见和裁剪标记，以及来源位置（页面内容流、Form XObject、注释外观或 Type3 字形过程，含操作符序号和字节范围）。后面的里程碑用这个位置精确删掉原来的文本绘制操作符。

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

`rpt-bench` 用掉字、占位符与公式保留、溢出、样式、非文字区域 SSIM 给译文 PDF 打分。`python3 corpus/bench/run.py` 把 CI 子集的恒等对照（每个 PDF 与自身比较）写到 `corpus/benchmarks/`。指标说明见 `corpus/benchmarks/README.md`。RapidPdfTrans 还不能写出译文 PDF（`rpt_save` 仍是 M3 占位）。

`corpus/manifest.json` 记录真实论文和可再分发图书的直接下载 URL、来源、许可证、sha256、大小和特征标签。`python3 corpus/fetch.py` 把它们下载到被 git 忽略的缓存，这些文件不进仓库。例外是 `corpus/ci/`：九篇 CC BY 4.0 论文，每篇不到 700 KB，URL 和许可证写在 `corpus/ci/manifest.json`，CI 直接用它们。`testdata/hello.pdf` 由 `cargo run -p rpt-core --example hello_pdf` 生成，只用于单元测试，不是语料。`rpt-qa` 对每份文件做抽取和字形覆盖率检查，用 Poppler 对文字，并对一份逐字节相同的副本做渲染对比（整页 SSIM，以及去掉字形框之后的非文字区域 SSIM）。lopdf 另存是另一次结构往返，会改写文件，报告里单独列出。CI 只跑标记了 `ci: true` 的小子集。全量语料在夜间的 `Corpus fidelity` 工作流。说明见 `corpus/README.md`。

## 里程碑

1. **M1（当前）。** 内容流解释器、字形守恒、可替换翻译器、C / Python / Go / C++ 骨架、命令行。
2. **M2.** 版面分析、在现有占位符之外保护更多结构、把缓存和术语表接进版面模型。
3. **M3.** 用整形和字体子集做重排，精确删除原文本操作符，写回 PDF，以及双语模式。`rpt_save` 从这里开始真正写文件。
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
