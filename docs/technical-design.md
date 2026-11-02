# 技术设计

本文概述应用的数据流和文件安全边界。各阶段的详细实现、验证证据与未完成事项见[实施状态](implementation-status.md)和[发布说明](release.md)。

## 阶段与数据关系

底部导航允许用户进入各阶段；下图中的虚线表示可选地携带审核清单，不代表阶段必须按顺序执行。

```mermaid
flowchart LR
    Source[选择来源目录] --> Scan[1 扫描文件]
    Scan --> Route[2 设置分类规则]
    Route --> Copy[3 分类复制]
    Copy --> Verify[4 完整性核对]
    Source -. 选择目录 .-> Metadata[5 元数据检查]
    Source -. 选择目录 .-> Duplicate[6 重复候选审核]
    Duplicate -. 可选审核清单 .-> Backup[7 多目标备份]
    Duplicate -. 可选审核清单 .-> Archive[8 加密归档]
    Source -. 独立选择目录 .-> Backup
    Source -. 独立选择目录 .-> Archive
```

扫描结果和任务状态通常只保留在当前会话。分类复制、备份映射、归档输入以及重复审核清单可通过本机检查点恢复；密码和旧的成功校验状态不会随检查点保存。恢复任务会重新读取文件并检查内容。

## 分类复制与覆盖保护

```mermaid
sequenceDiagram
    actor User as 用户
    participant App as 应用
    participant Source as 来源文件
    participant Temp as 临时文件
    participant Dest as 目标目录

    User->>App: 确认分类计划
    App->>Source: 检查扫描时记录的文件状态
    App->>Temp: 有界流式复制并计算 SHA-256
    App->>Source: 复读来源并计算 SHA-256
    App->>Temp: 复读临时文件并计算 SHA-256
    App->>Dest: 哈希一致且目标不存在时提交
    Dest-->>App: 已存在或不一致时报告错误
```

原件不会被移动或删除。复制先写入目标目录中的临时文件；只有来源、临时文件内容一致且目标路径可安全提交时，才提交最终文件。失败时不以覆盖已有文件的方式重试。

## 加密归档校验

```mermaid
flowchart TD
    Input[扫描来源并核对输入] --> Build[生成加密 7z 临时归档]
    Build --> Split[按设定大小切分归档卷]
    Split --> Reassemble[重组卷流并读取归档清单]
    Reassemble --> Compare{路径与 SHA-256 匹配?}
    Compare -->|否| Reject[拒绝提交]
    Compare -->|是| Optional{是否启用还原校验?}
    Optional -->|是| Extract[隔离目录还原并复读校验]
    Optional -->|否| Commit[防覆盖提交归档卷]
    Extract -->|通过| Commit
    Extract -->|失败| Reject
```

归档恢复时需要重新输入密码，并从头扫描、生成和验证归档。分卷写入和隔离目录还原会使用额外临时空间；具体限制及平台验收情况见[发布说明](release.md)。

## 相关文档

- [实施状态与测试证据](implementation-status.md)
- [构建、发布状态和已知限制](release.md)
- [目标设备验收清单](acceptance-checklist.md)
