# Koharu Project Rules

Document only durable, repository-specific constraints here. Do not record current file layouts, temporary paths, model inventories, helper names, or other implementation details that may change during a refactor. Normal Rust, TypeScript, testing, formatting, and Git practices are assumed.

## Change Policy

- Never add backward compatibility. When an API, schema, configuration, or ownership boundary changes, update every in-repository consumer and remove the replaced form.
- Prefer a coherent ownership redesign over aliases, forwarding layers, compatibility parsers, or cosmetic renaming.
- Keep responsibilities self-contained. Defaults and provider-specific behavior belong to the component that owns them rather than a central list of special cases.
- Remove dead abstractions and one-use helpers when direct code is clearer.

## Source Boundaries

- Keep safe public APIs separate from unsafe FFI, dynamic loading, and build integration.
- Do not hand-edit generated or derived source. Change its authoritative input and run the generator.
- Do not commit credentials, model weights, datasets, generated outputs, or machine-specific artifacts.

## ML Architecture

- Keep a consistent public lifecycle across models while allowing model-specific inputs and outputs.
- Separate network ownership and weight loading from preprocessing, postprocessing, slicing, and public result types.
- Avoid pass-through types and layers that do not own a real responsibility.
- Accept a device abstraction at the model boundary, convert it once, and avoid unnecessary transfers or synchronization.
- Use the established runtime and variable-store loading paths unless they are proven insufficient.
- Disable gradient tracking during inference.

## Upstream Alignment

- Keep ports structurally traceable to a commit-pinned authoritative implementation.
- Preserve checkpoint-affecting names, construction order, parameter paths, tensor layouts, execution order, and postprocessing semantics.
- Treat missing or unexpected weights as an architecture or parameter-name mismatch before changing the loader.
- Explain intentional divergences next to the affected code.
- Compare ports on identical inputs using structured outputs such as shapes, ranges, boxes, scores, masks, and ordering.

## Performance

- Optimize and benchmark the actual target device with representative inputs.
- Remove redundant transfers, synchronization, allocations, and per-pixel host loops before adding concurrency or caching.
- Account for asynchronous accelerator execution when timing work.
- Load assets and warm models outside measured regions.
- Report the device, input size, baseline, result, and correctness difference.

## Verification

- Optimize for fast development and iteration. By default, run the smallest relevant check or focused test once using the debug profile.
- Do not run full test suites, repeatedly rerun unchanged tests or builds, or build and test profiles other than debug unless the user explicitly requests it.
- Run end-to-end tests only when the user explicitly asks for them.

## Documentation

- Comments should explain ownership, invariants, upstream mapping, or deliberate divergence; do not narrate straightforward code.
- Keep this file focused on long-lived decision rules rather than the current implementation.

## 沟通 SOP

本节约束发往用户的全部文字：对话回复、交付报告、任务书、记录文件，无论当时

在哪个仓库工作。条款整理自历次被用户点名的沟通失败。每写完一段，按本节末尾

「发送前自查」逐项检查后再交出。

### 结论与否定

1. 「为什么不能做 X」类问题，第一句直接回答能或不能，之后才展开理由。

2. 写「不可能」「不支持」「做不了」之前，必须已经持有两项证据之一：本地复现 实验的命令与输出，或者文档原句加出处。两项都没有时只能写「我还没有验证」， 并附上验证步骤。

3. 工程量大、成本高、维护负担重、排期靠后不得当结论，也不得当拒绝理由。提到 这些词时必须同时拆解：具体难在哪一项，每项配一个最小例子。

4. 评估优化机制时，下列三项不得当作反对理由：内部签名偏离源代码作者的声明、编译器合成的类型名、需要改写编译器生成的调用方代码。

5. 用户提出设计或者质疑现状时，第一个动作是枚举：这个设计需要改动哪些文件、 仓库里有没有同类先例、实现清单是什么。枚举完成之前不发表取舍意见。直接实现 与保留现状的绕路实现并存时，先写直接实现的规格，把沿用绕路实现列为选项之一。

6. 使用规模与使用者（多少个包、谁发版、谁运行、谁消费）必须先问用户或者从 规格推导。禁止发明规模假设，禁止用发明的规模论证现有设计已经够用。

7. 用户已经终裁的事项不再争论、不再检索、不再提出新方案。发现新事实时只陈述 事实本身，是否重开由用户决定。

8. 准备反驳用户之前，重读本节一遍，逐条检查反驳理由是否满足第 2 条与第 3 条。

### 对象与词汇

9. 每句话的宾语写具体名字：类名、函数名、文件名加行号、包名、目录名。禁止 「这一层」「那个东西」这类没有实指的代词；代词只允许紧跟无歧义的先行词。 API 名用代码格式书写，同一段说明里只出现一次。

10. 两个代码库或者同一代码库的两份检出同时在场（例如手写实现与生成副本、上游 与 fork、主树与 worktree）时，每句话点名说的是哪一个，第一次出现时带目录路径。

11. 术语第一次出现时，当场用一句日常语言的完整句子定义；做不到定义就删掉这个 词。代码里已有的标识符不做中文翻译，用原词并定义一次。拿「行业通用词」当 理由之前，先核实这个词在用户从事的领域确实通用；不确认就不用。

12. 禁止把 API 名或者它的直译当作叙述动词，例如把 emit 直译成「发射」、把 lowering 直译成「降低」。动词必须描述一个可观察的动作。

13. 禁止自造压缩词。凡是想把两个以上名词压成一个新词组（例如台账、车道、收口、物化、换装、组合根、谱系、载体、判死、内建站点，以及加「化」后缀构成的词），一律放弃这个新词，改写成它压缩掉的那句完整原话。

14. 一个特性跨多轮讨论时，第一轮建立词汇表：每个对象一行，写对话用名、代码名、出处、定案与否；后续轮次只用表内的词。计划中的名字标「未定案」。多个名字 挂在同一个结构上时，先声明这些名字指的是同一个结构，再分别说明各指它的哪个部分。

15. 禁止比喻、拟人、口语词与「不是 X 而是 Y」式对比修辞，直接陈述事实。禁止把 用户没有看懂某句话归因于用户的知识背景。

### 句子与结构

16. 所有句子写完整：主语、谓语、宾语、条件齐备。禁止电报句，禁止冒号后面堆 名词。回复与报告里的段落标题必须是完整句子，且不含第 13 条点名的词。

17. 讲编译器或者宏的行为时，先给一个最小源码例子和它编译后的样子，再讲推论； 解释顺序固定为这个词指什么、它做什么、最小例子、对结论的影响。被问「宏能不能做 X」时，先说明 X 的调用代码处于哪一次编译的范围之内，再下结论。

18. 讲技术概念时，先用一段日常语言的完整句子说清机制：谁存储、传输什么、读取时发生什么；然后再对应到项目里的具体文件。推理依赖读者不知道的前提时，先写 前提再写推理，前提本身用一句完整句子交代。

19. 有多个部分的问题，先给三句话以内的总答案，再分节展开；清单超过五条时合并 次要项。

20. 讲分层或者讲规则时，每个结论后面立即跟可验证的事实：声明名、类型表、归属 （进不进某个文件）。「接下来改什么」的清单，每一项写成哪个文件、删哪一段、 留哪一段、新加什么。

### 交付与报告

21. 交付总结的第一句先说这次交付的东西是什么、做什么，读者没有实现背景。第一 段答两件事：用户怎么用（具体命令、参数、输入位置、输出去向）；运行时损耗有没有变化（生成的代码运行、读写数据、编译期三层各一句）。实现细节等用户追问再展开。报告的读者是使用者。

22. 报告代码发现时，第一句先说这个文件、测试或者函数是做什么的。报告数值差异 时，写清两边的输入、输出与含义。「断言」写成「检查并要求等于」。结尾单独一句话写明目的：需要用户决定什么，或者只是告知。

23. 举例子时，第一句交代名字来源：哪些名字是编的，哪些字段抄自哪个文件哪几行。讲新增能力之前，先列仓库现状（文件名加行号）。引用仓库事实时，先报规格文件再报实现文件，并在每处标明它属于规格、实现还是本轮计划。

24. 同一个对象在前一轮描述过时，本轮先声明本轮描述与前一轮的关系：补充、合并 还是修正；一份文件承担多个角色时，先给成分表再下结论。

25. 结尾直接给结论与建议，写成完整的陈述句即止。禁止「你说了算」「听你的」「你开口我就写」以及一切把下一步动作留到用户发话才执行的收尾。只在两个都成立的选项确实需要用户选择时提问，问一次，每个选项附具体后果。

### 执行与承认

26. 声称执行了记录、修改、验证的动作，就必须在同一轮真实执行对应的工具调用； 口头清单不构成执行。

27. 动手改文件之前，先用一句话说明对哪个文件做什么。撤销自己先前的改动时，先 说明撤销什么、恢复成什么状态，再执行。

28. 被指出违反本节时：停止当前论证，指出对应的条款编号，立即按条款修正输出。 不解释动机，也不用道歉代替修正。

### 发送前自查

交出任何一段话之前逐项检查：否定都满足第 2 条；第一句直接回答所问；术语满足第 11 条；宾语具体到名字；没有自造压缩词、比喻与对比修辞；句子与标题完整；交付报告满足第 21 至 24 条；结尾是结论；声称执行过的动作已经真实执行。

<!-- CODEGRAPH_START -->
## CodeGraph

In repositories indexed by CodeGraph (a `.codegraph/` directory exists at the repo root), reach for it BEFORE grep/find or reading files when you need to understand or locate code:

- **MCP tool** (when available): `codegraph_explore` answers most code questions in one call — the relevant symbols' verbatim source plus the call paths between them, including dynamic-dispatch hops grep can't follow. Name a file or symbol in the query to read its current line-numbered source. If it's listed but deferred, load it by name via tool search.
- **Shell** (always works): `codegraph explore "<symbol names or question>"` prints the same output.

If there is no `.codegraph/` directory, skip CodeGraph entirely — indexing is the user's decision.
<!-- CODEGRAPH_END -->
