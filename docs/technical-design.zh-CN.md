# 技术设计

**[English](technical-design.md) | 简体中文**

本文介绍数据流和文件安全边界。验证证据与待完成事项见[实施状态](implementation-status.zh-CN.md)、[发布说明](release.zh-CN.md)和[验收清单](acceptance-checklist.zh-CN.md)。

## 工作流程与数据关系

底部导航允许用户直接进入任意阶段。虚线表示可选地传递审核清单；阶段不必按顺序执行。

```mermaid
flowchart LR
    Source[选择来源目录] --> Scan[1 扫描文件]
    Scan --> Route[2 设置整理规则]
    Route --> Copy[3 分类复制]
    Copy --> Verify[4 校验复制文件]
    Source -. 选择目录 .-> Metadata[5 检查元数据]
    Source -. 选择目录 .-> Duplicate[6 审核重复候选]
    Duplicate -. 可选审核清单 .-> Backup[7 多目标备份]
    Duplicate -. 可选审核清单 .-> Archive[8 创建加密归档]
    Source -. 独立选择目录 .-> Backup
    Source -. 独立选择目录 .-> Archive
```

扫描结果和任务状态通常只保留在当前会话。本机私有检查点可恢复未完成的分类复制、备份映射、归档输入或重复审核清单。检查点不会保存归档密码或旧的成功结果。恢复后会重新读取文件并检查当前内容。

## 分类复制与覆盖保护

```mermaid
sequenceDiagram
    actor User as 用户
    participant App as 应用
    participant Source as 来源文件
    participant Temp as 临时文件
    participant Dest as 目标目录

    User->>App: 确认复制计划
    App->>Source: 检查扫描时记录的文件状态
    App->>Temp: 有界流式复制并计算 SHA-256
    App->>Source: 复读来源并计算 SHA-256
    App->>Temp: 复读临时文件并计算 SHA-256
    App->>Dest: 哈希一致且目标路径未占用时提交
    Dest-->>App: 目标已存在或内容不同时报告
```

程序不会移动或删除原件。复制先写入目标目录中的临时文件；只有来源与临时文件内容一致且目标可以安全创建时，才提交最终文件。失败后不会通过覆盖已有文件重试。

## 加密归档校验

```mermaid
flowchart TD
    Input[扫描来源并校验输入] --> Build[写入加密 7z 临时归档]
    Build --> Split[按设定大小切分归档卷]
    Split --> Reassemble[读取拼接卷并生成归档清单]
    Reassemble --> Compare{路径与 SHA-256 匹配？}
    Compare -->|否| Reject[不提交]
    Compare -->|是| Optional{是否启用还原校验？}
    Optional -->|是| Extract[还原至隔离目录并复读]
    Optional -->|否| Commit[防覆盖提交归档卷]
    Extract -->|通过| Commit
    Extract -->|失败| Reject
```

恢复中断的归档任务时，需要重新输入密码；程序会从头扫描、重建并校验归档。分卷和隔离还原校验需要额外临时空间。限制和平台验收状态见[发布说明](release.zh-CN.md)。

## 相关文档

- [实施状态与验证记录](implementation-status.zh-CN.md)
- [构建、发布状态和已知限制](release.zh-CN.md)
- [干净设备验收清单](acceptance-checklist.zh-CN.md)
